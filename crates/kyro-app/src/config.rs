use std::net::SocketAddr;

use url::Url;

use crate::{AppError, AppResult};

const MIN_SIGNING_KEY_BYTES: usize = 32;
const MAX_SIGNING_KEY_BYTES: usize = 4096;

#[derive(Clone)]
pub struct SessionTokenConfig {
    pub(crate) signing_key: Vec<u8>,
    pub(crate) issuer: String,
    pub(crate) audience: String,
}

impl SessionTokenConfig {
    pub fn new(
        signing_key: impl AsRef<[u8]>,
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> AppResult<Self> {
        let issuer = issuer.into();
        let audience = audience.into();
        let signing_key = signing_key.as_ref();
        if !(MIN_SIGNING_KEY_BYTES..=MAX_SIGNING_KEY_BYTES).contains(&signing_key.len()) {
            return Err(AppError::invalid("invalid_session_signing_key"));
        }
        if issuer.is_empty() || issuer.len() > 200 || audience.is_empty() || audience.len() > 200 {
            return Err(AppError::invalid("invalid_session_token_config"));
        }
        Ok(Self {
            signing_key: signing_key.to_vec(),
            issuer,
            audience,
        })
    }
}

impl std::fmt::Debug for SessionTokenConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTokenConfig")
            .field("signing_key", &"[REDACTED]")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish()
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub(crate) database_url: String,
    pub(crate) bind_address: SocketAddr,
    pub(crate) session_tokens: SessionTokenConfig,
    pub(crate) composition: Option<crate::composition::CompositionRuntime>,
}

impl AppConfig {
    pub fn new(
        database_url: impl Into<String>,
        bind_address: SocketAddr,
        session_tokens: SessionTokenConfig,
    ) -> AppResult<Self> {
        let database_url = database_url.into();
        let url =
            Url::parse(&database_url).map_err(|_| AppError::invalid("invalid_database_url"))?;
        if !matches!(url.scheme(), "postgres" | "postgresql") || url.host_str().is_none() {
            return Err(AppError::invalid("invalid_database_url"));
        }
        Ok(Self {
            database_url,
            bind_address,
            session_tokens,
            composition: None,
        })
    }

    pub fn from_env() -> AppResult<Self> {
        let database_url = std::env::var("KYRO_APP_DATABASE_URL")
            .map_err(|_| AppError::invalid("missing_configuration"))?;
        let key = std::env::var("KYRO_APP_SESSION_HMAC_KEY")
            .map_err(|_| AppError::invalid("missing_configuration"))?;
        let issuer = std::env::var("KYRO_APP_TOKEN_ISSUER")
            .map_err(|_| AppError::invalid("missing_configuration"))?;
        let audience = std::env::var("KYRO_APP_TOKEN_AUDIENCE")
            .map_err(|_| AppError::invalid("missing_configuration"))?;
        let bind_address = std::env::var("KYRO_APP_LISTEN_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:8081".to_owned())
            .parse()
            .map_err(|_| AppError::invalid("invalid_listen_address"))?;
        let config = Self::new(
            database_url,
            bind_address,
            SessionTokenConfig::new(key.as_bytes(), issuer, audience)?,
        )?;
        #[cfg(feature = "factory-artifact")]
        let config = {
            let lock = serde_json::from_str(include_str!("../factory-plan.json"))
                .map_err(|_| AppError::invalid("compiled_composition_missing"))?;
            config.with_composition(crate::composition::CompositionRuntime::new(lock)?)
        };
        Ok(config)
    }

    pub fn with_composition(mut self, composition: crate::composition::CompositionRuntime) -> Self {
        self.composition = Some(composition);
        self
    }

    pub fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }
}
