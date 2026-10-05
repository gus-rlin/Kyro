use super::{IdentityService, IssuedSession};
use crate::{AppError, AppResult};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashSet;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}
#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Key>,
}
#[derive(Deserialize)]
struct Key {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    alg: Option<String>,
    n: String,
    e: String,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    iat: i64,
    exp: i64,
    nonce: String,
    nbf: Option<i64>,
    azp: Option<String>,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    auth_time: Option<i64>,
    acr: Option<String>,
    #[serde(default)]
    amr: Vec<String>,
}

impl IdentityService {
    pub(crate) async fn oidc_begin(&self) -> AppResult<(String, Zeroizing<String>)> {
        let provider = self.config.oidc.as_ref().ok_or(AppError::NotFound)?;
        let binding = Zeroizing::new(crate::governance::token()?);
        self.rate("oidc", &binding).await?;
        let state = Zeroizing::new(crate::governance::token()?);
        let nonce = Zeroizing::new(crate::governance::token()?);
        let verifier = Zeroizing::new(crate::governance::token()?);
        let state_hash = Sha256::digest(state.as_bytes());
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(nonce.as_bytes())
            .map_err(|_| AppError::Internal)?;
        let context = format!(
            "{}:{}:oidc:{}",
            self.config.tenant_id,
            self.config.application_id,
            crate::governance::hex(&state_hash)
        );
        let encrypted = URL_SAFE_NO_PAD.encode(self.cipher.seal(&context, verifier.as_bytes())?);
        let mut tx = self.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('oidc-flows:'||$1::text||':'||$2::text,0))").bind(self.config.tenant_id).bind(self.config.application_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM app_oidc_flows WHERE expires_at<=clock_timestamp()")
            .execute(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM app_oidc_flows")
            .fetch_one(&mut *tx)
            .await?;
        if count >= 1000 {
            return Err(AppError::Quota);
        }
        sqlx::query("INSERT INTO app_oidc_flows(tenant_id,application_id,issuer,state_hash,nonce_hash,browser_binding_hash,pkce_verifier,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(self.config.tenant_id).bind(self.config.application_id).bind(&provider.issuer).bind(&state_hash[..]).bind(Sha256::digest(&nonce_bytes).to_vec()).bind(Sha256::digest(binding.as_bytes()).to_vec()).bind(encrypted).bind(Utc::now()+Duration::minutes(10)).execute(&mut *tx).await?;
        tx.commit().await?;
        let mut url =
            Url::parse(&provider.authorization_endpoint).map_err(|_| AppError::Internal)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email")
            .append_pair("client_id", &provider.client_id)
            .append_pair("redirect_uri", &provider.redirect_uri)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair(
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            )
            .append_pair("code_challenge_method", "S256");
        Ok((url.into(), binding))
    }
    pub(crate) async fn oidc_callback(
        &self,
        state: &str,
        code: Zeroizing<String>,
        binding: &str,
    ) -> AppResult<IssuedSession> {
        let provider = self.config.oidc.as_ref().ok_or(AppError::NotFound)?;
        if state.len() != 43 || binding.len() != 43 || code.is_empty() || code.len() > 4096 {
            return Err(AppError::Unauthorized);
        }
        self.rate("callback", binding).await?;
        let state_hash = Sha256::digest(state.as_bytes());
        let mut tx = self.begin().await?;
        let flow=sqlx::query("DELETE FROM app_oidc_flows WHERE issuer=$1 AND state_hash=$2 AND browser_binding_hash=$3 AND expires_at>clock_timestamp() RETURNING nonce_hash,pkce_verifier").bind(&provider.issuer).bind(&state_hash[..]).bind(Sha256::digest(binding.as_bytes()).to_vec()).fetch_optional(&mut *tx).await?.ok_or(AppError::Unauthorized)?;
        tx.commit().await?;
        // Consumption commits before contacting the provider: an ambiguous exchange
        // requires a new flow and can never replay the authorization code.
        let encrypted = URL_SAFE_NO_PAD
            .decode(flow.try_get::<String, _>("pkce_verifier")?)
            .map_err(|_| AppError::Internal)?;
        let context = format!(
            "{}:{}:oidc:{}",
            self.config.tenant_id,
            self.config.application_id,
            crate::governance::hex(&state_hash)
        );
        let verifier = self.cipher.open(&context, &encrypted)?;
        let verifier = std::str::from_utf8(&verifier).map_err(|_| AppError::Internal)?;
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("client_id", provider.client_id.as_str()),
            ("redirect_uri", provider.redirect_uri.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier),
        ];
        if let Some(secret) = &self.client_secret {
            fields.push(("client_secret", secret.as_str()));
        }
        let response = self
            .client
            .post(&provider.token_endpoint)
            .form(&fields)
            .send()
            .await
            .map_err(|_| AppError::Unavailable)?;
        if !response.status().is_success() {
            return Err(AppError::Unauthorized);
        }
        let body = Zeroizing::new(read_bounded(response, 65536).await?);
        let token: TokenResponse =
            serde_json::from_slice(&body).map_err(|_| AppError::Unauthorized)?;
        let token = Zeroizing::new(token.id_token);
        if token.is_empty() || token.len() > 65536 {
            return Err(AppError::Unauthorized);
        }
        let claims = self
            .verify_oidc(&token, &flow.try_get::<Vec<u8>, _>("nonce_hash")?)
            .await?;
        let t = self.config.tenant_id;
        let a = self.config.application_id;
        let mut tx = self.begin().await?;
        self.lock_authority(&mut tx).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "oidc-identity:{t}:{a}:{}:{}",
                provider.issuer, claims.sub
            ))
            .execute(&mut *tx)
            .await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT principal_id FROM app_external_identities WHERE issuer=$1 AND subject=$2",
        )
        .bind(&provider.issuer)
        .bind(&claims.sub)
        .fetch_optional(&mut *tx)
        .await?;
        let principal = match existing {
            Some(p) => p,
            None => {
                if !self.config.oidc_signup {
                    return Err(AppError::Unauthorized);
                }
                let role = self.config.signup_role.as_ref().ok_or(AppError::Internal)?;
                let safe:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_role_permissions WHERE role=$1) AND NOT EXISTS(SELECT 1 FROM app_role_permissions WHERE role=$1 AND permission='*')").bind(role).fetch_one(&mut *tx).await?;
                if !safe {
                    return Err(AppError::Forbidden);
                }
                let p = Uuid::new_v4();
                sqlx::query("INSERT INTO app_principals(tenant_id,id) VALUES($1,$2)")
                    .bind(t)
                    .bind(p)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO app_memberships(tenant_id,application_id,principal_id,role) VALUES($1,$2,$3,$4)").bind(t).bind(a).bind(p).bind(role).execute(&mut *tx).await?;
                p
            }
        };
        let auth_time = claims
            .auth_time
            .and_then(|n| DateTime::from_timestamp(n, 0));
        let mfa = auth_time.filter(|time| {
            *time > Utc::now() - Duration::minutes(5)
                && *time <= Utc::now() + Duration::seconds(30)
                && claims
                    .acr
                    .as_ref()
                    .is_some_and(|acr| provider.mfa_acr.contains(acr))
                && claims
                    .amr
                    .iter()
                    .any(|amr| matches!(amr.as_str(), "mfa" | "otp" | "hwk"))
        });
        sqlx::query("INSERT INTO app_external_identities(tenant_id,application_id,issuer,subject,principal_id,email,email_verified,auth_time,acr,amr) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(tenant_id,application_id,issuer,subject) DO UPDATE SET email=EXCLUDED.email,email_verified=EXCLUDED.email_verified,auth_time=EXCLUDED.auth_time,acr=EXCLUDED.acr,amr=EXCLUDED.amr,updated_at=clock_timestamp()").bind(t).bind(a).bind(&provider.issuer).bind(&claims.sub).bind(principal).bind(&claims.email).bind(claims.email_verified).bind(auth_time).bind(&claims.acr).bind(&claims.amr).execute(&mut *tx).await?;
        let session = self
            .session(
                &mut tx,
                principal,
                mfa,
                claims.acr.as_deref(),
                &claims.amr,
                "oidc",
            )
            .await?;
        // Retain the provider's authentication time; a token issued using a provider
        // SSO cookie is not proof that credentials were checked again just now.
        sqlx::query("UPDATE app_sessions SET auth_time=$1 WHERE token_hash=$2")
            .bind(auth_time)
            .bind(Sha256::digest(session.token.as_bytes()).to_vec())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(session)
    }
    async fn verify_oidc(&self, token: &str, nonce_hash: &[u8]) -> AppResult<Claims> {
        let p = self.config.oidc.as_ref().ok_or(AppError::NotFound)?;
        let header = decode_header(token).map_err(|_| AppError::Unauthorized)?;
        if header.alg != Algorithm::RS256 {
            return Err(AppError::Unauthorized);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or(AppError::Unauthorized)?;
        let response = self
            .client
            .get(&p.jwks_uri)
            .send()
            .await
            .map_err(|_| AppError::Unavailable)?;
        if !response.status().is_success() {
            return Err(AppError::Unavailable);
        }
        let jwks: Jwks = serde_json::from_slice(&read_bounded(response, 256 * 1024).await?)
            .map_err(|_| AppError::Unavailable)?;
        if jwks.keys.is_empty() || jwks.keys.len() > 64 {
            return Err(AppError::Unavailable);
        }
        let mut matching = jwks
            .keys
            .iter()
            .filter(|key| key.kid.as_deref() == Some(kid));
        let key = matching.next().ok_or(AppError::Unauthorized)?;
        if matching.next().is_some()
            || key.kty != "RSA"
            || key.key_use.as_deref() != Some("sig")
            || key.alg.as_deref() != Some("RS256")
            || key.n.len() > 8192
            || key.e.len() > 16
        {
            return Err(AppError::Unauthorized);
        }
        let modulus = URL_SAFE_NO_PAD
            .decode(&key.n)
            .map_err(|_| AppError::Unauthorized)?;
        let exponent = URL_SAFE_NO_PAD
            .decode(&key.e)
            .map_err(|_| AppError::Unauthorized)?;
        if !(256..=1024).contains(&modulus.len())
            || modulus[0] & 0x80 == 0
            || exponent.is_empty()
            || exponent.len() > 8
            || exponent.last().is_none_or(|v| v & 1 == 0)
        {
            return Err(AppError::Unauthorized);
        }
        let key =
            DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| AppError::Unauthorized)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[p.issuer.as_str()]);
        validation.set_audience(&[p.client_id.as_str()]);
        validation.leeway = 0;
        validation.required_spec_claims = ["iss", "aud", "sub", "exp", "iat", "nonce"]
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AppError::Unauthorized)?
            .claims;
        let now = Utc::now().timestamp();
        let audience = match &claims.aud {
            Audience::One(a) => a == &p.client_id,
            Audience::Many(a) => a.len() == 1 && a[0] == p.client_id,
        };
        let nonce = URL_SAFE_NO_PAD
            .decode(&claims.nonce)
            .map_err(|_| AppError::Unauthorized)?;
        if claims.iss != p.issuer
            || !audience
            || claims.sub.is_empty()
            || claims.sub.len() > 255
            || claims.sub.contains(char::is_control)
            || claims.iat > now + 60
            || claims.iat < now - 600
            || claims
                .exp
                .checked_sub(claims.iat)
                .is_none_or(|n| !(1..=3600).contains(&n))
            || claims.nbf.is_some_and(|n| n > now + 60)
            || claims.azp.as_ref().is_some_and(|a| a != &p.client_id)
            || nonce.len() != 32
            || Sha256::digest(&nonce)[..] != nonce_hash[..]
            || claims.amr.len() > 32
            || claims
                .amr
                .iter()
                .any(|s| s.len() > 128 || s.contains(char::is_control))
            || claims
                .acr
                .as_ref()
                .is_some_and(|s| s.len() > 128 || s.contains(char::is_control))
            || claims
                .auth_time
                .is_some_and(|n| n > claims.iat + 60 || n < 0)
            || claims
                .email
                .as_ref()
                .is_some_and(|s| super::normalize_email(s).is_err())
            || claims.email_verified && claims.email.is_none()
        {
            return Err(AppError::Unauthorized);
        }
        Ok(claims)
    }
}
async fn read_bounded(mut response: reqwest::Response, max: usize) -> AppResult<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(AppError::Unavailable);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AppError::Unavailable)? {
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(AppError::Unavailable);
        }
        bytes.extend(chunk);
    }
    Ok(bytes)
}
