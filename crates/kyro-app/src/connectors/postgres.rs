//! One operator-selected, bounded PostgreSQL snapshot. No request supplies SQL.
use super::*;
use futures_util::TryStreamExt;
use sqlx::{
    ConnectOptions, Connection, PgConnection,
    postgres::{PgConnectOptions, PgSslMode},
};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresSource {
    pub database: String,
    pub username: String,
    pub schema: String,
    pub table: String,
    pub key_column: String,
    pub partition_column: String,
    /// A fixed external partition, configured by the operator for this app.
    pub partition_value: String,
    /// Source column -> writable target field. Unmapped columns never leave PG.
    pub mapping: BTreeMap<String, String>,
    pub target_entity: String,
    pub target_schema_version: i64,
    pub maximum_rows: u32,
    /// Public trust material only; the password comes from SecretVault.
    pub root_ca_pem: String,
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn quoted(s: &str) -> String {
    format!("\"{s}\"")
}
impl PostgresSource {
    pub(super) fn validate(&self, p: &Profile, url: &url::Url) -> AppResult<()> {
        let host = url
            .host_str()
            .ok_or(AppError::invalid("postgres_destination_invalid"))?;
        if url.scheme() != "postgres"
            || url.port() != Some(5432)
            || !matches!(url.path(), "" | "/")
            || host.parse::<IpAddr>().is_ok()
            || !p.allowed_hosts.contains(host)
            || host.len() > 253
            || host.split('.').any(|part| {
                part.is_empty()
                    || part.len() > 63
                    || part.starts_with('-')
                    || part.ends_with('-')
                    || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || p.secret_ref.is_none()
            || !(1..=5000).contains(&self.maximum_rows)
            || self.target_schema_version < 1
            || self.mapping.is_empty()
            || self.mapping.len() > 32
            || [
                &self.database,
                &self.username,
                &self.schema,
                &self.table,
                &self.key_column,
                &self.partition_column,
                &self.target_entity,
            ]
            .iter()
            .any(|s| !identifier(s))
            || self.schema.starts_with("pg_")
            || self.schema == "information_schema"
            || self
                .mapping
                .iter()
                .any(|(s, t)| !identifier(s) || !identifier(t))
            || self.mapping.values().collect::<BTreeSet<_>>().len() != self.mapping.len()
            || !bounded(&self.partition_value, 256)
            || self.root_ca_pem.len() > 16384
            || !self.root_ca_pem.contains("-----BEGIN CERTIFICATE-----")
        {
            return Err(AppError::invalid("postgres_configuration_invalid"));
        }
        Ok(())
    }
    fn columns(&self) -> BTreeSet<&str> {
        self.mapping
            .keys()
            .map(String::as_str)
            .chain([self.key_column.as_str(), self.partition_column.as_str()])
            .collect()
    }
}
pub(super) async fn validate_source(tx: &mut AppTx, source: &PostgresSource) -> AppResult<Vec<u8>> {
    crate::data::import_schema_binding(
        tx,
        &source.target_entity,
        source.target_schema_version,
        source.mapping.values().map(String::as_str),
    )
    .await
}
pub(super) async fn password(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
) -> AppResult<Zeroizing<Vec<u8>>> {
    let value = service
        .vault
        .resolve(
            tx,
            p.id,
            p.secret_ref.ok_or(AppError::Forbidden)?,
            "connector.send",
        )
        .await?;
    if !std::str::from_utf8(&value).is_ok_and(|s| bounded(s, 4096)) {
        return Err(AppError::Unavailable);
    }
    Ok(value)
}
async fn address(service: &ConnectorService, p: &Profile) -> AppResult<SocketAddr> {
    let url = url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?;
    let host = url.host_str().ok_or(AppError::Internal)?;
    #[cfg(feature = "test-support")]
    if let Some(a) = service.routes.get(host) {
        return Ok(*a);
    }
    let _ = service;
    let addresses: Vec<_> = tokio::net::lookup_host((host, 5432))
        .await
        .map_err(|_| AppError::Unavailable)?
        .collect();
    if addresses.is_empty()
        || addresses.len() > 32
        || addresses
            .iter()
            .any(|a| !crate::exchange::is_public_ip(a.ip()))
    {
        return Err(AppError::invalid("postgres_destination_denied"));
    }
    // This exact address is used by the relay; the driver does not resolve again.
    Ok(addresses[0])
}

/// The relay pins TCP while SQLx authenticates the original DNS name over TLS.
/// SQLx 0.8.6 applies MaybeUpgradeTls to Unix sockets as well as TCP sockets.
#[cfg(unix)]
struct Relay {
    directory: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
}
#[cfg(unix)]
impl Relay {
    async fn open(address: SocketAddr, cap: usize) -> AppResult<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let directory = std::env::temp_dir().join(format!("kyro-pg-{}", Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| AppError::Unavailable)?;
        let listener = match tokio::net::UnixListener::bind(directory.join(".s.PGSQL.5432")) {
            Ok(l) => l,
            Err(_) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(AppError::Unavailable);
            }
        };
        let task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let Ok((local, _)) = listener.accept().await else {
                return;
            };
            drop(listener); // one connection; no pool or implicit reconnection
            let Ok(remote) = tokio::net::TcpStream::connect(address).await else {
                return;
            };
            let (local_read, mut local_write) = local.into_split();
            let (remote_read, mut remote_write) = remote.into_split();
            let mut local_read = local_read.take(262144);
            let mut remote_read = remote_read.take(cap as u64 + 262144);
            tokio::select! {
                _ = tokio::io::copy(&mut local_read, &mut remote_write) => {},
                _ = tokio::io::copy(&mut remote_read, &mut local_write) => {},
            }
        });
        Ok(Self { directory, task })
    }
}
#[cfg(unix)]
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        // Only the two paths created above are removed. No recursive cleanup.
        let _ = std::fs::remove_file(self.directory.join(".s.PGSQL.5432"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

pub(super) async fn snapshot(
    service: &ConnectorService,
    p: &Profile,
    password: &[u8],
) -> AppResult<Value> {
    #[cfg(not(unix))]
    {
        let _ = (service, p, password);
        Err(AppError::Unavailable)
    }
    #[cfg(unix)]
    {
        tokio::time::timeout(Duration::from_millis(p.timeout_ms), async {
            let Provider::Postgres { settings } = &p.configuration else { return Err(AppError::Forbidden); };
            let address = address(service,p).await?;
            let relay = Relay::open(address, p.max_response_bytes).await?;
            let url = url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?;
            let limit = p.timeout_ms.to_string();
            // Defaults read PG* variables. Do not inherit an ambient client
            // identity, password file or SQL options into the external adapter.
            if ["PGPASSWORD", "PGSSLCERT", "PGSSLKEY", "PGOPTIONS"].iter().any(|name| std::env::var_os(name).is_some()) {
                return Err(AppError::invalid("postgres_ambient_credentials_denied"));
            }
            let options = PgConnectOptions::new_without_pgpass().host(url.host_str().ok_or(AppError::Internal)?).port(5432)
                .socket(&relay.directory).database(&settings.database).username(&settings.username)
                .password(std::str::from_utf8(password).map_err(|_| AppError::Unavailable)?)
                .ssl_mode(PgSslMode::VerifyFull).ssl_root_cert_from_pem(settings.root_ca_pem.as_bytes().to_vec())
                .application_name("kyro-external-snapshot").statement_cache_capacity(0)
                .options([("default_transaction_read_only","on"),("statement_timeout",limit.as_str()),("lock_timeout",limit.as_str()),("search_path","pg_catalog")])
                .disable_statement_logging();
            let mut connection = PgConnection::connect_with(&options).await.map_err(|_| AppError::Unavailable)?;
            let mut tx = connection.begin().await.map_err(|_| AppError::Unavailable)?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.map_err(|_| AppError::Unavailable)?;
            minimal_role(&mut tx, settings).await?;
            let mvcc: String = sqlx::query_scalar("SELECT pg_catalog.pg_current_snapshot()::text").fetch_one(&mut *tx).await.map_err(|_| AppError::Unavailable)?;
            let columns = settings.mapping.iter().map(|(column,field)| format!("'{field}',{}",quoted(column))).collect::<Vec<_>>().join(",");
            let query = format!("SELECT {}::text AS source_key,pg_catalog.jsonb_build_object({columns}) AS row_values FROM {}.{} WHERE {}::text=$1 ORDER BY {} LIMIT $2", quoted(&settings.key_column),quoted(&settings.schema),quoted(&settings.table),quoted(&settings.partition_column),quoted(&settings.key_column));
            let mut stream = sqlx::query(&query).bind(&settings.partition_value).bind(i64::from(settings.maximum_rows)+1).fetch(&mut *tx);
            let mut rows = Vec::new(); let mut keys = BTreeSet::new(); let mut bytes = 2usize;
            while let Some(row) = stream.try_next().await.map_err(|_| AppError::Unavailable)? {
                if rows.len() >= settings.maximum_rows as usize { return Err(AppError::invalid("postgres_row_limit")); }
                let key: String = row.try_get("source_key").map_err(|_| AppError::invalid("postgres_key_invalid"))?;
                if !bounded(&key,256) || !keys.insert(key) { return Err(AppError::invalid("postgres_duplicate_key")); }
                let values: Value = row.try_get("row_values").map_err(|_| AppError::invalid("postgres_row_invalid"))?;
                bytes = bytes.checked_add(serde_json::to_vec(&values).map_err(|_| AppError::Internal)?.len()+1).ok_or(AppError::Quota)?;
                if bytes > p.max_response_bytes { return Err(AppError::invalid("postgres_byte_limit")); }
                crate::governance::validate_shape(&values)?;
                rows.push(values);
            }
            drop(stream);
            tx.commit().await.map_err(|_| AppError::Unavailable)?;
            connection.close().await.map_err(|_| AppError::Unavailable)?;
            Ok(json!({"rows":rows,"mvcc_snapshot_hash":protocols::hex(&Sha256::digest(mvcc.as_bytes())),"bytes":bytes}))
        }).await.map_err(|_| AppError::Unavailable)?
    }
}

async fn minimal_role(connection: &mut PgConnection, source: &PostgresSource) -> AppResult<()> {
    let safe: bool = sqlx::query_scalar("SELECT NOT r.rolsuper AND NOT r.rolinherit AND NOT r.rolcreaterole AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member=r.oid) AND NOT pg_catalog.has_database_privilege(current_user,current_database(),'CREATE') AND NOT pg_catalog.has_database_privilege(current_user,current_database(),'TEMP') AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_namespace n WHERE pg_catalog.has_schema_privilege(current_user,n.oid,'CREATE')) FROM pg_catalog.pg_roles r WHERE r.rolname=current_user").fetch_one(&mut *connection).await.map_err(|_| AppError::Unavailable)?;
    if !safe {
        return Err(AppError::invalid("postgres_role_too_broad"));
    }
    let table = sqlx::query("SELECT c.oid::bigint AS oid,c.relkind::text AS kind,c.relowner=(SELECT oid FROM pg_catalog.pg_roles WHERE rolname=current_user) AS owned,c.relrowsecurity FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2").bind(&source.schema).bind(&source.table).fetch_optional(&mut *connection).await.map_err(|_| AppError::Unavailable)?.ok_or(AppError::NotFound)?;
    let oid: i64 = table.try_get("oid")?;
    if table.try_get::<String, _>("kind")? != "r"
        || table.try_get::<bool, _>("owned")?
        || !table.try_get::<bool, _>("relrowsecurity")?
    {
        return Err(AppError::invalid("postgres_source_table_denied"));
    }
    let required: Vec<String> = source.columns().into_iter().map(str::to_owned).collect();
    // Column privileges, including inherited PUBLIC grants, must be limited to
    // precisely this table/mapping. Views, FDWs, ownership and custom types fail.
    let broad: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND c.relkind IN ('r','p','v','m','f') AND ((pg_catalog.has_column_privilege(current_user,c.oid,a.attnum,'SELECT') AND (c.oid::bigint<>$1 OR NOT a.attname=ANY($2))) OR pg_catalog.has_table_privilege(current_user,c.oid,'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR pg_catalog.has_column_privilege(current_user,c.oid,a.attnum,'INSERT,UPDATE,REFERENCES')))").bind(oid).bind(&required).fetch_one(&mut *connection).await.map_err(|_| AppError::Unavailable)?;
    if broad {
        return Err(AppError::invalid("postgres_role_too_broad"));
    }
    let allowed: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type t ON t.oid=a.atttypid JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace WHERE a.attrelid::bigint=$1 AND a.attnum>0 AND NOT a.attisdropped AND a.attname=ANY($2) AND n.nspname='pg_catalog' AND t.typname IN ('text','varchar','bpchar','uuid','bool','int2','int4','int8','date','timestamptz') AND pg_catalog.has_column_privilege(current_user,a.attrelid,a.attnum,'SELECT')").bind(oid).bind(&required).fetch_one(&mut *connection).await.map_err(|_| AppError::Unavailable)?;
    if allowed != required.len() as i64 {
        return Err(AppError::invalid("postgres_columns_denied"));
    }
    Ok(())
}

pub(super) async fn apply(
    tx: &mut AppTx,
    p: &Profile,
    id: Uuid,
    response: &Value,
) -> AppResult<Value> {
    let Provider::Postgres { settings } = &p.configuration else {
        return Err(AppError::Forbidden);
    };
    let schema_hash = validate_source(tx, settings).await?;
    let rows = response["rows"].as_array().ok_or(AppError::Internal)?;
    let fingerprint = protocols::hex(&Sha256::digest(
        serde_json::to_vec(rows).map_err(|_| AppError::Internal)?,
    ));
    let provenance = json!({"adapter_id":p.id,"profile_hash":protocols::hex(&p.hash()?),"schema_hash":protocols::hex(&schema_hash),"source_fingerprint":fingerprint,"mvcc_snapshot_hash":response["mvcc_snapshot_hash"],"row_count":rows.len(),"read_only":true});
    if rows.is_empty() {
        return Ok(json!({"state":"empty","provenance":provenance}));
    }
    let preview =
        crate::data::preview_external_import(tx, id, &settings.target_entity, rows, &provenance)
            .await?;
    Ok(json!({"import_id":id,"preview":preview,"provenance":provenance}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sql_identifier_language_is_closed() {
        for name in [
            "",
            "bad-name",
            "public.table",
            "\"escape",
            "name;select",
            "équipe",
            "A",
            "_a",
        ] {
            assert!(!identifier(name));
        }
        for name in ["name", "table_1", "tenant_id"] {
            assert!(identifier(name));
        }
    }
}
