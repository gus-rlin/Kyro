#[path = "../migrate.rs"]
mod migrate;

use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use std::{env, process, time::Duration};

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("migration failed; database details withheld");
        process::exit(1);
    }
    println!("migrations are up to date");
}

async fn run() -> Result<(), ()> {
    let database_url = env::var("KYRO_DATABASE_ADMIN_URL").map_err(|_| ())?;
    let pool = migration_pool(&database_url).await.map_err(|_| ())?;
    let role = sqlx::query(
        "SELECT current_user AS role_name, r.rolsuper, r.rolbypassrls, r.rolcreaterole \
         FROM pg_catalog.pg_roles r WHERE r.rolname = current_user",
    )
    .fetch_one(&pool)
    .await
    .map_err(|_| ())?;

    let role_name: String = role.try_get("role_name").map_err(|_| ())?;
    let is_superuser: bool = role.try_get("rolsuper").map_err(|_| ())?;
    let bypasses_rls: bool = role.try_get("rolbypassrls").map_err(|_| ())?;
    let can_create_roles: bool = role.try_get("rolcreaterole").map_err(|_| ())?;
    if role_name == "kyro_api"
        || role_name == "kyro_worker"
        || !(is_superuser || bypasses_rls)
        || !(is_superuser || can_create_roles)
    {
        return Err(());
    }

    migrate::run_migrations(&pool).await.map_err(|_| ())
}

async fn migration_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    // Keep server/operator timeouts; runtime's 5s statement/2s lock limits do not apply to DDL.
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a dedicated PostgreSQL database in KYRO_TEST_DATABASE_ADMIN_URL"]
    async fn migration_pool_preserves_operator_timeouts_and_allows_long_statements() {
        let url = env::var("KYRO_TEST_DATABASE_ADMIN_URL").expect("dedicated admin database");
        let pool = migration_pool(&url).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW statement_timeout")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "0"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW lock_timeout")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "0"
        );
        sqlx::query("SELECT pg_sleep(5.1)")
            .execute(&pool)
            .await
            .expect("migration exceeds runtime's five-second limit");
        pool.close().await;
        let separator = if url.contains('?') { '&' } else { '?' };
        let configured =
            format!("{url}{separator}options[statement_timeout]=10000&options[lock_timeout]=7000");
        let pool = migration_pool(&configured).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW statement_timeout")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "10s"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW lock_timeout")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "7s"
        );
        let runtime = kyro_store::Store::connect(&url, 1).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW statement_timeout")
                .fetch_one(&runtime.pool)
                .await
                .unwrap(),
            "5s"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW lock_timeout")
                .fetch_one(&runtime.pool)
                .await
                .unwrap(),
            "2s"
        );
    }
}
