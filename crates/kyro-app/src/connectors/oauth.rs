use super::*;
use crate::exchange::{RestrictedMethod, RestrictedRequest};
use base64::Engine;
use chrono::DateTime;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthProvider {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub revocation_endpoint: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: BTreeSet<String>,
    #[serde(default)]
    pub client_secret_post: bool,
}
impl OAuthProvider {
    pub(super) fn validate(&self, p: &Profile, test: bool) -> AppResult<()> {
        if !bounded(&self.client_id, 256)
            || self.scopes.is_empty()
            || self.scopes.len() > 16
            || self
                .scopes
                .iter()
                .any(|s| !bounded(s, 256) || s.chars().any(char::is_whitespace))
        {
            return Err(AppError::invalid("oauth_configuration_invalid"));
        }
        for endpoint in [&self.authorization_endpoint, &self.revocation_endpoint] {
            let u = url::Url::parse(endpoint)
                .map_err(|_| AppError::invalid("oauth_endpoint_invalid"))?;
            if u.query().is_some()
                || u.fragment().is_some()
                || u.username() != ""
                || u.password().is_some()
            {
                return Err(AppError::invalid("oauth_endpoint_invalid"));
            }
            if !(test && u.scheme() == "http" && u.host_str().is_some_and(|h| h.ends_with(".test")))
            {
                crate::exchange::validate_http_destination(&u, &p.allowed_hosts)?;
            }
            if !u.host_str().is_some_and(|h| p.allowed_hosts.contains(h)) {
                return Err(AppError::invalid("oauth_endpoint_not_allowed"));
            }
        }
        for endpoint in [&self.issuer, &self.redirect_uri] {
            let u = url::Url::parse(endpoint)
                .map_err(|_| AppError::invalid("oauth_identity_uri_invalid"))?;
            if u.username() != ""
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
                || (u.scheme() != "https"
                    && !(test && u.scheme() == "http" && u.host_str() == Some("127.0.0.1")))
            {
                return Err(AppError::invalid("oauth_identity_uri_invalid"));
            }
        }
        Ok(())
    }
}
fn settings(p: &Profile) -> AppResult<&OAuthProvider> {
    match &p.configuration {
        Provider::OAuth { settings } => Ok(settings),
        _ => Err(AppError::Forbidden),
    }
}
fn connection_id(tx: &AppTx, p: &Profile) -> Uuid {
    crate::governance::stable_id(
        "oauth.connection",
        &format!("{}/{}", p.id, tx.actor().principal_id()),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Complete {
    adapter_id: Uuid,
    id: Uuid,
    state: String,
    code: String,
    issuer: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowContent {
    verifier: String,
    #[serde(default)]
    code: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
}
pub(super) async fn execute(
    service: &ConnectorService,
    tx: &mut AppTx,
    r: &OperationRequest,
) -> AppResult<Value> {
    match r.action.as_str() {
        "oauth.begin" => {
            tx.require_elevated()?;
            let i: AdapterId = decode(r)?;
            let p = service.admitted(tx, i.id).await?;
            let cfg = settings(p)?;
            let admission = connection_id(tx, p);
            tx.lock_record_key("oauth.admission", admission).await?;
            let n:i64=sqlx::query_scalar("SELECT count(*) FROM app_connector_oauth_flows WHERE adapter_id=$1 AND state IN ('pending','queued') AND expires_at>clock_timestamp()").bind(p.id).fetch_one(tx.conn()).await?;
            if n >= 3 {
                return Err(AppError::Quota);
            }
            let state = Zeroizing::new(crate::governance::token()?);
            let verifier = Zeroizing::new(crate::governance::token()?);
            let id = Uuid::new_v4();
            let content = Zeroizing::new(
                serde_json::to_vec(&FlowContent {
                    verifier: verifier.to_string(),
                    code: None,
                })
                .map_err(|_| AppError::Internal)?,
            );
            let cipher = service
                .cipher
                .seal(&service.context(tx, id, "oauth-flow"), &content)?;
            sqlx::query("INSERT INTO app_connector_oauth_flows(tenant_id,principal_id,id,adapter_id,profile_hash,state_hash,content_cipher) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(p.id).bind(p.hash()?).bind(Sha256::digest(state.as_bytes()).to_vec()).bind(cipher).execute(tx.conn()).await?;
            let mut u =
                url::Url::parse(&cfg.authorization_endpoint).map_err(|_| AppError::Internal)?;
            u.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", &cfg.client_id)
                .append_pair("redirect_uri", &cfg.redirect_uri)
                .append_pair(
                    "scope",
                    &cfg.scopes.iter().cloned().collect::<Vec<_>>().join(" "),
                )
                .append_pair("state", &state)
                .append_pair("code_challenge_method", "S256")
                .append_pair(
                    "code_challenge",
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(Sha256::digest(verifier.as_bytes())),
                );
            tx.audit(
                "B156",
                "oauth.begin",
                Some(id),
                json!({"adapter_id":p.id,"scopes":cfg.scopes}),
            )
            .await?;
            Ok(
                json!({"id":id,"secret_once":{"authorization_url":u.as_str()},"expires_in_seconds":600}),
            )
        }
        "oauth.complete" => {
            tx.require_elevated()?;
            let i: Complete = decode(r)?;
            let p = service.admitted(tx, i.adapter_id).await?;
            let cfg = settings(p)?;
            if !bounded(&i.code, 4096) || i.state.len() > 128 || i.issuer != cfg.issuer {
                return Err(AppError::invalid("oauth_callback_invalid"));
            }
            let row=sqlx::query("SELECT state_hash,attempts,content_cipher,profile_hash FROM app_connector_oauth_flows WHERE id=$1 AND adapter_id=$2 AND source_session_id=kyro_app_session_id() AND state='pending' AND expires_at>clock_timestamp() FOR UPDATE").bind(i.id).bind(p.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            let attempts: i32 = row.try_get("attempts")?;
            if attempts >= 5 {
                return Err(AppError::Forbidden);
            }
            if row.try_get::<Vec<u8>, _>("profile_hash")? != p.hash()? {
                return Err(AppError::conflict("oauth_profile_changed"));
            }
            if row.try_get::<Vec<u8>, _>("state_hash")?
                != Sha256::digest(i.state.as_bytes()).to_vec()
            {
                sqlx::query("UPDATE app_connector_oauth_flows SET attempts=attempts+1 WHERE id=$1")
                    .bind(i.id)
                    .execute(tx.conn())
                    .await?;
                return Ok(json!({"accepted":false,"attempts_remaining":4-attempts}));
            }
            let context = service.context(tx, i.id, "oauth-flow");
            let plain = service
                .cipher
                .open(&context, &row.try_get::<Vec<u8>, _>("content_cipher")?)?;
            let mut content: FlowContent =
                serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?;
            content.code = Some(i.code);
            let plain =
                Zeroizing::new(serde_json::to_vec(&content).map_err(|_| AppError::Internal)?);
            let cipher = service.cipher.seal(&context, &plain)?;
            sqlx::query(
                "UPDATE app_connector_oauth_flows SET state='queued',content_cipher=$2 WHERE id=$1",
            )
            .bind(i.id)
            .bind(cipher)
            .execute(tx.conn())
            .await?;
            // A new authorization may rotate the old refresh credential remotely.
            sqlx::query("UPDATE app_connector_oauth_connections SET state='unknown',version=version+1 WHERE adapter_id=$1 AND state<>'revoked'").bind(p.id).execute(tx.conn()).await?;
            let result = service
                .prepare(tx, r, p, Call::OAuthExchange { flow_id: i.id })
                .await?;
            sqlx::query("UPDATE app_connector_oauth_flows SET call_id=$2 WHERE id=$1")
                .bind(i.id)
                .bind(
                    Uuid::parse_str(result["id"].as_str().ok_or(AppError::Internal)?)
                        .map_err(|_| AppError::Internal)?,
                )
                .execute(tx.conn())
                .await?;
            Ok(result)
        }
        "oauth.refresh" | "oauth.revoke" => {
            tx.require_elevated()?;
            let i: AdapterId = decode(r)?;
            let p = service.admitted(tx, i.id).await?;
            settings(p)?;
            let id = connection_id(tx, p);
            let row=sqlx::query("SELECT version,state,profile_hash FROM app_connector_oauth_connections WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            let version: i64 = row.try_get("version")?;
            if r.expected_version != Some(version)
                || row.try_get::<String, _>("state")? != "active"
                || row.try_get::<Vec<u8>, _>("profile_hash")? != p.hash()?
            {
                return Err(AppError::conflict("oauth_connection_changed"));
            }
            let revoke = r.action == "oauth.revoke";
            sqlx::query("UPDATE app_connector_oauth_connections SET state=$2,version=version+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(if revoke{"revoking"}else{"refreshing"}).execute(tx.conn()).await?;
            let specification = if revoke {
                Call::OAuthRevoke {
                    connection_id: id,
                    version: version + 1,
                }
            } else {
                Call::OAuthRefresh {
                    connection_id: id,
                    version: version + 1,
                }
            };
            let result = service.prepare(tx, r, p, specification).await?;
            sqlx::query("UPDATE app_connector_oauth_connections SET call_id=$2 WHERE id=$1")
                .bind(id)
                .bind(
                    Uuid::parse_str(result["id"].as_str().ok_or(AppError::Internal)?)
                        .map_err(|_| AppError::Internal)?,
                )
                .execute(tx.conn())
                .await?;
            Ok(result)
        }
        "oauth.status" | "oauth.use" => {
            let i: AdapterId = decode(r)?;
            let p = service.admitted(tx, i.id).await?;
            settings(p)?;
            status(tx, connection_id(tx, p)).await
        }
        _ => Err(AppError::NotFound),
    }
}
async fn status(tx: &mut AppTx, id: Uuid) -> AppResult<Value> {
    let row = sqlx::query(
        "SELECT version,state,scopes,expires_at FROM app_connector_oauth_connections WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(
        json!({"id":id,"version":row.try_get::<i64,_>("version")?,"state":row.try_get::<String,_>("state")?,"scopes":row.try_get::<Vec<String>,_>("scopes")?,"expires_at":row.try_get::<DateTime<Utc>,_>("expires_at")?}),
    )
}
async fn flow(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    id: Uuid,
) -> AppResult<(FlowContent, Vec<u8>)> {
    let row=sqlx::query("SELECT profile_hash,content_cipher FROM app_connector_oauth_flows WHERE id=$1 AND adapter_id=$2 AND source_session_id=kyro_app_session_id() AND state='queued' AND expires_at>clock_timestamp()").bind(id).bind(p.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<Vec<u8>, _>("profile_hash")? != p.hash()? {
        return Err(AppError::conflict("oauth_profile_changed"));
    }
    let cipher: Vec<u8> = row.try_get("content_cipher")?;
    let plain = service
        .cipher
        .open(&service.context(tx, id, "oauth-flow"), &cipher)?;
    Ok((
        serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?,
        Sha256::digest(&cipher).to_vec(),
    ))
}
async fn tokens(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    id: Uuid,
    version: i64,
    required_state: &str,
) -> AppResult<(Tokens, Vec<u8>)> {
    let row=sqlx::query("SELECT version,state,profile_hash,credential_cipher,expires_at FROM app_connector_oauth_connections WHERE id=$1 AND adapter_id=$2").bind(id).bind(p.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<i64, _>("version")? != version
        || row.try_get::<String, _>("state")? != required_state
        || row.try_get::<Vec<u8>, _>("profile_hash")? != p.hash()?
    {
        return Err(AppError::conflict("oauth_credential_changed"));
    }
    if required_state == "active" && row.try_get::<DateTime<Utc>, _>("expires_at")? <= Utc::now() {
        return Err(AppError::conflict("oauth_access_expired"));
    }
    let cipher: Vec<u8> = row.try_get("credential_cipher")?;
    let plain = service
        .cipher
        .open(&service.context(tx, id, "oauth-credentials"), &cipher)?;
    let hash=Sha256::digest(serde_json::to_vec(&json!({"cipher":protocols::hex(&Sha256::digest(&cipher)),"version":version,"state":required_state})).map_err(|_|AppError::Internal)?).to_vec();
    Ok((
        serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?,
        hash,
    ))
}
pub(super) async fn validate(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
) -> AppResult<(Option<Vec<u8>>, Option<i64>)> {
    settings(p)?;
    let hash = match c {
        Call::OAuthExchange { flow_id } => flow(service, tx, p, *flow_id).await?.1,
        Call::OAuthRefresh {
            connection_id,
            version,
        } => {
            let (t, h) = tokens(service, tx, p, *connection_id, *version, "refreshing").await?;
            if t.refresh_token.is_none() {
                return Err(AppError::conflict("oauth_refresh_not_available"));
            }
            h
        }
        Call::OAuthRevoke {
            connection_id,
            version,
        } => {
            tokens(service, tx, p, *connection_id, *version, "revoking")
                .await?
                .1
        }
        _ => return Err(AppError::Forbidden),
    };
    Ok((Some(hash), None))
}
pub(super) async fn build(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
) -> AppResult<RestrictedRequest> {
    let cfg = settings(p)?;
    let mut fields = vec![];
    let endpoint = match c {
        Call::OAuthExchange { flow_id } => {
            let (f, _) = flow(service, tx, p, *flow_id).await?;
            fields.extend([
                ("grant_type", "authorization_code".into()),
                ("code", f.code.ok_or(AppError::Internal)?),
                ("code_verifier", f.verifier),
                ("redirect_uri", cfg.redirect_uri.clone()),
            ]);
            &p.endpoint
        }
        Call::OAuthRefresh {
            connection_id,
            version,
        } => {
            let (t, _) = tokens(service, tx, p, *connection_id, *version, "refreshing").await?;
            fields.extend([
                ("grant_type", "refresh_token".into()),
                ("refresh_token", t.refresh_token.ok_or(AppError::NotFound)?),
            ]);
            &p.endpoint
        }
        Call::OAuthRevoke {
            connection_id,
            version,
        } => {
            let (t, _) = tokens(service, tx, p, *connection_id, *version, "revoking").await?;
            let refresh = t.refresh_token.is_some();
            fields.extend([
                ("token", t.refresh_token.unwrap_or(t.access_token)),
                (
                    "token_type_hint",
                    if refresh {
                        "refresh_token"
                    } else {
                        "access_token"
                    }
                    .into(),
                ),
            ]);
            &cfg.revocation_endpoint
        }
        _ => return Err(AppError::Forbidden),
    };
    if p.secret_ref.is_none() || cfg.client_secret_post {
        fields.push(("client_id", cfg.client_id.clone()));
    }
    Ok(RestrictedRequest {
        method: RestrictedMethod::Post,
        url: url::Url::parse(endpoint).map_err(|_| AppError::Internal)?,
        headers: vec![(
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: protocols::form(&fields),
    })
}
pub(super) async fn authorize_client(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    r: &mut RestrictedRequest,
) -> AppResult<()> {
    let Some(reference) = p.secret_ref else {
        return Ok(());
    };
    let cfg = settings(p)?;
    let secret = service
        .vault
        .resolve(tx, p.id, reference, "connector.send")
        .await?;
    let secret = std::str::from_utf8(&secret).map_err(|_| AppError::Unavailable)?;
    if !bounded(secret, 4096) {
        return Err(AppError::Unavailable);
    }
    if cfg.client_secret_post {
        r.body.extend(b"&");
        r.body
            .extend(protocols::form(&[("client_secret", secret.into())]));
    } else {
        fn enc(s: &str) -> String {
            url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
        }
        r.headers.push((
            "authorization".into(),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    enc(&cfg.client_id),
                    enc(secret)
                ))
            ),
        ));
    }
    if r.body.len() > p.max_request_bytes {
        return Err(AppError::invalid("oauth_request_limit"));
    }
    Ok(())
}
pub(super) async fn access(
    service: &ConnectorService,
    tx: &mut AppTx,
    id: Uuid,
    required_scopes: &BTreeSet<String>,
) -> AppResult<(Zeroizing<String>, Vec<u8>)> {
    tx.require_operation("B156", "oauth.use")?;
    let p = service.admitted(tx, id).await?;
    settings(p)?;
    let cid = connection_id(tx, p);
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM app_connector_oauth_connections WHERE id=$1")
            .bind(cid)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
    let (credentials, hash) = tokens(service, tx, p, cid, version, "active").await?;
    let scopes: Vec<String> =
        sqlx::query_scalar("SELECT scopes FROM app_connector_oauth_connections WHERE id=$1")
            .bind(cid)
            .fetch_one(tx.conn())
            .await?;
    if !required_scopes.is_subset(&scopes.into_iter().collect()) {
        return Err(AppError::Forbidden);
    }
    Ok((Zeroizing::new(credentials.access_token), hash))
}
pub(super) async fn apply(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    reply: &Value,
) -> AppResult<Value> {
    let cfg = settings(p)?;
    let cid = connection_id(tx, p);
    if let Call::OAuthRevoke {
        connection_id,
        version,
    } = c
    {
        let n=sqlx::query("UPDATE app_connector_oauth_connections SET state='revoked',credential_cipher=NULL,version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND version=$2 AND state='revoking'").bind(connection_id).bind(version).execute(tx.conn()).await?.rows_affected();
        if n != 1 {
            return Err(AppError::conflict("oauth_revoke_generation_changed"));
        }
        return Ok(json!({"id":connection_id,"state":"revoked","version":version+1}));
    }
    let token = |key: &str| {
        reply[key]
            .as_str()
            .filter(|s| bounded(s, 4096) && !s.chars().any(char::is_whitespace))
            .map(str::to_owned)
    };
    let access = token("access_token").ok_or(AppError::invalid("oauth_access_token_invalid"))?;
    if !reply["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(AppError::invalid("oauth_token_type_invalid"));
    }
    let lifetime = reply["expires_in"]
        .as_i64()
        .filter(|s| *s >= 1 && *s <= 86400)
        .ok_or(AppError::invalid("oauth_expiry_required"))?;
    let conservative_lifetime = lifetime - p.timeout_ms.div_ceil(1000) as i64 - 5;
    if conservative_lifetime <= 0 {
        return Err(AppError::invalid("oauth_expiry_too_short"));
    }
    let maximum_scopes = if let Call::OAuthRefresh { connection_id, .. } = c {
        let scopes: Vec<String> = sqlx::query_scalar(
            "SELECT scopes FROM app_connector_oauth_connections WHERE id=$1 AND state='refreshing'",
        )
        .bind(connection_id)
        .fetch_one(tx.conn())
        .await?;
        scopes.into_iter().collect::<BTreeSet<_>>()
    } else {
        cfg.scopes.clone()
    };
    let scopes = if let Some(value) = reply.get("scope") {
        let s = value
            .as_str()
            .ok_or(AppError::invalid("oauth_scope_invalid"))?;
        if s.len() > 4096 {
            return Err(AppError::invalid("oauth_scope_invalid"));
        }
        s.split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    } else {
        maximum_scopes.clone()
    };
    if scopes.is_empty() || !scopes.is_subset(&maximum_scopes) {
        return Err(AppError::invalid("oauth_scope_expanded"));
    }
    let refresh = if reply.get("refresh_token").is_some() {
        Some(token("refresh_token").ok_or(AppError::invalid("oauth_refresh_token_invalid"))?)
    } else if let Call::OAuthRefresh {
        connection_id,
        version,
    } = c
    {
        tokens(service, tx, p, *connection_id, *version, "refreshing")
            .await?
            .0
            .refresh_token
    } else {
        None
    };
    let credentials = Zeroizing::new(
        serde_json::to_vec(&Tokens {
            access_token: access,
            refresh_token: refresh,
        })
        .map_err(|_| AppError::Internal)?,
    );
    let cipher = service
        .cipher
        .seal(&service.context(tx, cid, "oauth-credentials"), &credentials)?;
    let expires = Utc::now() + chrono::Duration::seconds(conservative_lifetime);
    let version:i64=sqlx::query_scalar("INSERT INTO app_connector_oauth_connections(tenant_id,principal_id,id,adapter_id,profile_hash,state,credential_cipher,scopes,expires_at) VALUES($1,$2,$3,$4,$5,'active',$6,$7,$8) ON CONFLICT(tenant_id,application_id,principal_id,adapter_id) DO UPDATE SET state='active',version=app_connector_oauth_connections.version+1,profile_hash=EXCLUDED.profile_hash,credential_cipher=EXCLUDED.credential_cipher,scopes=EXCLUDED.scopes,expires_at=EXCLUDED.expires_at,call_id=NULL,updated_at=clock_timestamp() RETURNING version").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(cid).bind(p.id).bind(p.hash()?).bind(cipher).bind(scopes.iter().cloned().collect::<Vec<_>>()).bind(expires).fetch_one(tx.conn()).await?;
    if let Call::OAuthExchange { flow_id } = c {
        let empty = service
            .cipher
            .seal(&service.context(tx, *flow_id, "oauth-flow"), b"{}")?;
        sqlx::query("UPDATE app_connector_oauth_flows SET state='completed',content_cipher=$2 WHERE id=$1 AND state='queued'").bind(flow_id).bind(empty).execute(tx.conn()).await?;
    }
    Ok(json!({"id":cid,"state":"active","version":version,"scopes":scopes,"expires_at":expires}))
}
