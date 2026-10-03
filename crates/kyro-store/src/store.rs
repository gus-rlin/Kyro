use std::time::Duration;

use kyro_domain::{Environment, Error, Result};
use sqlx::{
    PgConnection, PgPool, Postgres, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};
use uuid::Uuid;

const MAX_POOL_CONNECTIONS: u32 = 32;
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
    pub environment: Environment,
}

impl Store {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self> {
        if !(1..=MAX_POOL_CONNECTIONS).contains(&max_connections) {
            return Err(Error::Invalid(
                "database pool size is outside the allowed range".to_owned(),
            ));
        }

        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET lock_timeout = '2s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await
            .map_err(|_| Error::Unavailable)?;

        Ok(Self {
            pool,
            environment: Environment::Development,
        })
    }

    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    pub async fn begin_actor(&self, actor_id: Uuid) -> Result<Transaction<'static, Postgres>> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        sqlx::query("SELECT set_config('kyro.actor_id', $1, true), set_config('kyro.environment', $2, true)")
            .bind(actor_id.to_string())
            .bind(self.environment.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        Ok(transaction)
    }

    pub async fn check_ready(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|_| Error::Unavailable)?;

        let row: PgRow = sqlx::query(
            "SELECT r.rolname IN ('kyro_api', 'kyro_worker') AS runtime_role, \
                    r.rolsuper AS superuser, \
                    r.rolbypassrls AS bypasses_rls, \
                    EXISTS ( \
                        SELECT 1 FROM pg_class c \
                        JOIN pg_namespace n ON n.oid = c.relnamespace \
                        WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') \
                          AND c.relname IN ('projects', 'jobs', 'events') \
                          AND (NOT c.relrowsecurity OR NOT c.relforcerowsecurity) \
                    ) AS invalid_security_table, \
                    (SELECT COUNT(*) = 3 FROM pg_class c \
                        JOIN pg_namespace n ON n.oid = c.relnamespace \
                        WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') \
                          AND c.relname IN ('projects', 'jobs', 'events') \
                          AND c.relrowsecurity AND c.relforcerowsecurity) AS schema_ready, \
                    EXISTS ( \
                        SELECT 1 FROM pg_class c \
                        JOIN pg_namespace n ON n.oid = c.relnamespace \
                        WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
                          AND n.nspname NOT LIKE 'pg_toast%' \
                          AND c.relkind IN ('r', 'p') AND c.relrowsecurity \
                          AND pg_has_role(current_user, c.relowner, 'MEMBER') \
                    ) AS owns_rls_table \
             FROM pg_roles r WHERE r.rolname = current_user",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| Error::Unavailable)?;

        let runtime_role: bool = row
            .try_get("runtime_role")
            .map_err(|_| Error::Unavailable)?;
        let superuser: bool = row.try_get("superuser").map_err(|_| Error::Unavailable)?;
        let bypasses_rls: bool = row
            .try_get("bypasses_rls")
            .map_err(|_| Error::Unavailable)?;
        let invalid_security_table: bool = row
            .try_get("invalid_security_table")
            .map_err(|_| Error::Unavailable)?;
        let schema_ready: bool = row
            .try_get("schema_ready")
            .map_err(|_| Error::Unavailable)?;
        let owns_rls_table: bool = row
            .try_get("owns_rls_table")
            .map_err(|_| Error::Unavailable)?;

        if !runtime_role
            || superuser
            || bypasses_rls
            || invalid_security_table
            || !schema_ready
            || owns_rls_table
        {
            return Err(Error::Unavailable);
        }

        Ok(())
    }

    pub async fn append_event(
        connection: &mut PgConnection,
        project_id: Uuid,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<kyro_domain::Event> {
        let row = sqlx::query(
            "SELECT project_id, sequence, type, payload, actor_id, created_at \
             FROM public.kyro_append_event($1, $2, $3)",
        )
        .bind(project_id)
        .bind(kind)
        .bind(payload)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_database_error)?;
        Ok(kyro_domain::Event {
            project_id: row.try_get("project_id").map_err(map_database_error)?,
            sequence: row.try_get("sequence").map_err(map_database_error)?,
            kind: row.try_get("type").map_err(map_database_error)?,
            payload: row.try_get("payload").map_err(map_database_error)?,
            actor_id: row.try_get("actor_id").map_err(map_database_error)?,
            created_at: row.try_get("created_at").map_err(map_database_error)?,
        })
    }
}

pub async fn append_event(
    connection: &mut PgConnection,
    project_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
) -> Result<kyro_domain::Event> {
    Store::append_event(connection, project_id, kind, payload).await
}

pub(crate) fn map_database_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(database_error) => match database_error.code().as_deref() {
            Some("P0002") => Error::NotFound,
            Some("42501") => Error::Forbidden,
            Some("40001") => Error::Conflict("concurrent database change".to_owned()),
            Some("23505") => Error::Conflict("resource already exists".to_owned()),
            Some("23514") => Error::Invalid("request violates a data constraint".to_owned()),
            _ => Error::Internal,
        },
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
            Error::Unavailable
        }
        _ => Error::Internal,
    }
}
