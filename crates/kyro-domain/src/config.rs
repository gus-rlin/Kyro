use std::{env, fmt, net::SocketAddr};

use crate::{Environment, Error, Result};
use url::Url;

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_MAX_CONNECTIONS: u32 = 8;
const DEFAULT_WORKER_POLL_MS: u64 = 100;
const DEFAULT_LEASE_SECONDS: u64 = 10;
const DEFAULT_MAX_BODY_BYTES: usize = 262_144;

#[derive(Clone)]
pub struct Config {
    pub environment: Environment,
    /// Empty for a worker config created with `for_worker_from_env`.
    pub database_url: String,
    /// Empty for an API config created with `for_api_from_env`.
    pub worker_database_url: String,
    pub bind: SocketAddr,
    pub max_connections: u32,
    pub worker_poll_ms: u64,
    pub lease_seconds: u64,
    pub max_body_bytes: usize,
    pub synthetic_providers: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessRole {
    Api,
    Worker,
    CombinedDevelopment,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("environment", &self.environment)
            .field("database_url", &"[REDACTED]")
            .field("worker_database_url", &"[REDACTED]")
            .field("bind", &self.bind)
            .field("max_connections", &self.max_connections)
            .field("worker_poll_ms", &self.worker_poll_ms)
            .field("lease_seconds", &self.lease_seconds)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("synthetic_providers", &self.synthetic_providers)
            .finish()
    }
}

impl Config {
    /// Loads both database URLs for development tooling and tests.
    /// Production processes must use a role-specific constructor.
    pub fn from_env() -> Result<Self> {
        Self::load(ProcessRole::CombinedDevelopment)
    }

    /// Loads only the API database URL; production rejects worker credentials in this process.
    pub fn for_api_from_env() -> Result<Self> {
        Self::load(ProcessRole::Api)
    }

    /// Loads only the worker database URL; production rejects API credentials in this process.
    pub fn for_worker_from_env() -> Result<Self> {
        Self::load(ProcessRole::Worker)
    }

    fn load(role: ProcessRole) -> Result<Self> {
        let environment = match read_env("KYRO_ENV")?.as_deref().unwrap_or("development") {
            "development" => Environment::Development,
            "production" => Environment::Production,
            _ => return Err(Error::Invalid("invalid KYRO_ENV".to_owned())),
        };

        let synthetic_providers = synthetic_providers_enabled()?;
        if environment == Environment::Production && synthetic_providers {
            return Err(Error::Invalid(
                "synthetic providers are forbidden in production".to_owned(),
            ));
        }

        let (database_url, worker_database_url) = select_database_urls(
            role,
            environment,
            read_env("KYRO_DATABASE_URL")?,
            read_env("KYRO_WORKER_DATABASE_URL")?,
        )?;
        let bind = setting_for_environment("KYRO_BIND", environment, DEFAULT_BIND)?
            .parse()
            .map_err(|_| Error::Invalid("invalid KYRO_BIND".to_owned()))?;

        Ok(Self {
            environment,
            database_url,
            worker_database_url,
            bind,
            max_connections: bounded_for_environment(
                "KYRO_MAX_CONNECTIONS",
                environment,
                DEFAULT_MAX_CONNECTIONS,
                1,
                32,
            )?,
            worker_poll_ms: bounded_for_environment(
                "KYRO_WORKER_POLL_MS",
                environment,
                DEFAULT_WORKER_POLL_MS,
                10,
                10_000,
            )?,
            lease_seconds: bounded_for_environment(
                "KYRO_LEASE_SECONDS",
                environment,
                DEFAULT_LEASE_SECONDS,
                2,
                120,
            )?,
            max_body_bytes: bounded_for_environment(
                "KYRO_MAX_BODY_BYTES",
                environment,
                DEFAULT_MAX_BODY_BYTES,
                1,
                DEFAULT_MAX_BODY_BYTES,
            )?,
            synthetic_providers,
        })
    }
}

fn setting_for_environment(
    name: &'static str,
    environment: Environment,
    development_default: &'static str,
) -> Result<String> {
    match read_env(name)? {
        Some(value) => Ok(value),
        None if environment == Environment::Development => Ok(development_default.to_owned()),
        None => Err(Error::Invalid(format!("{name} is required in production"))),
    }
}

fn select_database_urls(
    role: ProcessRole,
    environment: Environment,
    api_url: Option<String>,
    worker_url: Option<String>,
) -> Result<(String, String)> {
    match role {
        ProcessRole::Api => {
            if environment == Environment::Production && worker_url.is_some() {
                return Err(Error::Invalid(
                    "worker database credential is not allowed in the API process".to_owned(),
                ));
            }
            Ok((
                parse_required_database_url("KYRO_DATABASE_URL", api_url, environment)?,
                String::new(),
            ))
        }
        ProcessRole::Worker => {
            if environment == Environment::Production && api_url.is_some() {
                return Err(Error::Invalid(
                    "API database credential is not allowed in the worker process".to_owned(),
                ));
            }
            Ok((
                String::new(),
                parse_required_database_url("KYRO_WORKER_DATABASE_URL", worker_url, environment)?,
            ))
        }
        ProcessRole::CombinedDevelopment => {
            if environment == Environment::Production {
                return Err(Error::Invalid(
                    "use a role-specific configuration in production".to_owned(),
                ));
            }
            Ok((
                parse_required_database_url("KYRO_DATABASE_URL", api_url, environment)?,
                parse_required_database_url("KYRO_WORKER_DATABASE_URL", worker_url, environment)?,
            ))
        }
    }
}

fn parse_required_database_url(
    name: &'static str,
    value: Option<String>,
    environment: Environment,
) -> Result<String> {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Invalid(format!("{name} is required")))?;
    parse_database_url(name, value, environment)
}

fn parse_database_url(
    name: &'static str,
    value: String,
    environment: Environment,
) -> Result<String> {
    let parsed = Url::parse(&value)
        .map_err(|_| Error::Invalid(format!("{name} must be a PostgreSQL URL")))?;
    let postgres_scheme = matches!(parsed.scheme(), "postgres" | "postgresql");
    let unix_socket_host = parsed
        .query_pairs()
        .any(|(key, host)| key == "host" && !host.trim().is_empty());
    if !postgres_scheme || (parsed.host_str().is_none() && !unix_socket_host) {
        return Err(Error::Invalid(format!("{name} must be a PostgreSQL URL")));
    }
    if environment == Environment::Production {
        let mut socket = parsed
            .host_str()
            .is_some_and(|host| host.to_ascii_lowercase().starts_with("%2f"));
        let mut ssl_mode = None;
        let mut root_cert = None;
        // Match SQLx's aliases and last-value precedence, including endpoint overrides.
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "host" => socket = value.starts_with('/'),
                "hostaddr" => socket = false,
                "sslmode" | "ssl-mode" => ssl_mode = Some(value.into_owned()),
                "sslrootcert" | "ssl-root-cert" | "ssl-ca" => root_cert = Some(value.into_owned()),
                _ => {}
            }
        }
        if !socket
            && (ssl_mode.as_deref() != Some("verify-full")
                || root_cert
                    .as_deref()
                    .is_none_or(|cert| cert.trim().is_empty()))
        {
            return Err(Error::Invalid(format!(
                "{name} requires verify-full TLS and a root certificate in production"
            )));
        }
    }
    Ok(value)
}

fn bounded_for_environment<T>(
    name: &'static str,
    environment: Environment,
    development_default: T,
    min: T,
    max: T,
) -> Result<T>
where
    T: Copy + Ord + std::str::FromStr,
{
    bounded_setting(
        name,
        environment,
        read_env(name)?.as_deref(),
        development_default,
        min,
        max,
    )
}

fn bounded_setting<T>(
    name: &'static str,
    environment: Environment,
    raw: Option<&str>,
    development_default: T,
    min: T,
    max: T,
) -> Result<T>
where
    T: Copy + Ord + std::str::FromStr,
{
    if raw.is_none() && environment == Environment::Production {
        return Err(Error::Invalid(format!("{name} is required in production")));
    }
    bounded_value(name, raw, development_default, min, max)
}

fn bounded_value<T>(name: &'static str, raw: Option<&str>, default: T, min: T, max: T) -> Result<T>
where
    T: Copy + Ord + std::str::FromStr,
{
    let Some(raw) = raw else {
        return Ok(default);
    };
    let value = raw
        .parse::<T>()
        .map_err(|_| Error::Invalid(format!("invalid {name}")))?;
    if !(min..=max).contains(&value) {
        return Err(Error::Invalid(format!(
            "{name} is outside the allowed range"
        )));
    }
    Ok(value)
}

fn synthetic_providers_enabled() -> Result<bool> {
    parse_synthetic_providers(read_env("KYRO_SYNTHETIC_PROVIDERS")?.as_deref())
}

fn parse_synthetic_providers(raw: Option<&str>) -> Result<bool> {
    match raw {
        None | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(_) => Err(Error::Invalid(
            "invalid KYRO_SYNTHETIC_PROVIDERS".to_owned(),
        )),
    }
}

fn read_env(name: &'static str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(Error::Invalid(format!("{name} is not valid Unicode")))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::{
        ProcessRole, bounded_setting, bounded_value, parse_database_url, parse_synthetic_providers,
        select_database_urls,
    };
    use crate::{Config, Environment, Error};

    #[test]
    fn numeric_configuration_bounds_reject_values_outside_the_limit() {
        assert!(matches!(
            bounded_value("KYRO_TEST_VALUE", Some("33"), 8_u32, 1, 32).expect_err("out of range"),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn unset_numeric_configuration_uses_the_development_default() {
        assert_eq!(
            bounded_value("KYRO_TEST_VALUE", None, 8_u32, 1, 32).expect("default"),
            8
        );
    }

    #[test]
    fn debug_redacts_database_urls() {
        let config = Config {
            environment: Environment::Development,
            database_url: "postgres://user:secret@localhost/kyro".to_owned(),
            worker_database_url: "postgres://worker:secret@localhost/kyro".to_owned(),
            bind: "127.0.0.1:8080"
                .parse::<SocketAddr>()
                .expect("valid address"),
            max_connections: 8,
            worker_poll_ms: 100,
            lease_seconds: 10,
            max_body_bytes: 262_144,
            synthetic_providers: false,
        };
        let debug = format!("{config:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn production_requires_explicit_resource_limits() {
        assert!(matches!(
            bounded_setting(
                "KYRO_TEST_VALUE",
                Environment::Production,
                None,
                8_u32,
                1,
                32
            )
            .expect_err("production value is required"),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn synthetic_providers_require_an_explicit_true_value() {
        assert!(!parse_synthetic_providers(None).expect("absent flag defaults off"));
        assert!(parse_synthetic_providers(Some("true")).expect("explicit true"));
        assert!(parse_synthetic_providers(Some("yes")).is_err());
    }

    #[test]
    fn database_urls_must_use_a_postgresql_scheme_and_endpoint() {
        assert!(
            parse_database_url(
                "KYRO_DATABASE_URL",
                "postgres://db/app".to_owned(),
                Environment::Development
            )
            .is_ok()
        );
        assert!(
            parse_database_url(
                "KYRO_DATABASE_URL",
                "https://db/app".to_owned(),
                Environment::Development
            )
            .is_err()
        );
        assert!(
            parse_database_url(
                "KYRO_DATABASE_URL",
                "postgres:///app".to_owned(),
                Environment::Development
            )
            .is_err()
        );
    }

    #[test]
    fn production_api_configuration_does_not_retain_worker_credentials() {
        let (api, worker) = select_database_urls(
            ProcessRole::Api,
            Environment::Production,
            Some("postgres://api@db/kyro?sslmode=verify-full&sslrootcert=/ca".to_owned()),
            None,
        )
        .expect("API-only credentials");
        assert_eq!(
            api,
            "postgres://api@db/kyro?sslmode=verify-full&sslrootcert=/ca"
        );
        assert!(worker.is_empty());
        assert!(
            select_database_urls(
                ProcessRole::Api,
                Environment::Production,
                Some("postgres://api@db/kyro?sslmode=verify-full&sslrootcert=/ca".to_owned()),
                Some("postgres://worker@db/kyro".to_owned()),
            )
            .is_err()
        );
    }

    #[test]
    fn production_tcp_database_urls_require_verified_tls_for_each_runtime_role() {
        for role in [ProcessRole::Api, ProcessRole::Worker] {
            for query in [
                "",
                "?sslmode=prefer",
                "?sslmode=require",
                "?sslmode=verify-ca",
                "?sslmode=verify-full",
                "?sslmode=verify-full&sslrootcert=",
                "?sslmode=verify-full&sslrootcert=/ca&ssl-mode=disable",
                "?sslmode=verify-full&sslrootcert=/ca&ssl-ca=",
                "?host=db",
                "?host=/tmp&hostaddr=127.0.0.1",
            ] {
                let url = Some(format!("postgres://runtime@db/kyro{query}"));
                let (api, worker) = if matches!(role, ProcessRole::Api) {
                    (url, None)
                } else {
                    (None, url)
                };
                assert!(
                    select_database_urls(role, Environment::Production, api, worker).is_err(),
                    "production URL must fail closed: {query}"
                );
            }
        }
        for url in [
            "postgres://api@db/kyro?sslmode=verify-full&sslrootcert=/ca",
            "postgres://api@db/kyro?ssl-mode=verify-full&ssl-ca=/ca",
            "postgres:///kyro?host=/var/run/postgresql",
        ] {
            assert!(
                select_database_urls(
                    ProcessRole::Api,
                    Environment::Production,
                    Some(url.into()),
                    None
                )
                .is_ok(),
                "valid production endpoint: {url}"
            );
        }
    }
}
