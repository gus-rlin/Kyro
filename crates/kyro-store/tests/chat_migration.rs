//! Installation and upgrade use disposable databases, never the chat or P1 runtime.
use kyro_store::migrate::MIGRATOR;
use sqlx::{
    Row,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{borrow::Cow, str::FromStr};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable kyro_p1_test_* admin database"]
async fn postgres_chat_migration_installs_and_upgrades_without_resetting_runtime_state() {
    let options =
        PgConnectOptions::from_str(&std::env::var("KYRO_TEST_DATABASE_ADMIN_URL").unwrap())
            .unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let source: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert!(
        source.starts_with("kyro_p1_test_"),
        "migration fixture requires a dedicated test database"
    );
    for upgrade in [false, true] {
        let name = format!("kyro_p1_test_chat_migration_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone().database(&name))
            .await
            .unwrap();
        let mut previous = Vec::new();
        if upgrade {
            // SQLx 0.8.6 is locked. Retain the original migration objects and checksums.
            let legacy = Migrator {
                migrations: Cow::Owned(
                    MIGRATOR
                        .iter()
                        .filter(|m| m.version < 18)
                        .cloned()
                        .collect(),
                ),
                ..Migrator::DEFAULT
            };
            legacy.run(&pool).await.unwrap();
            let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(version, 17);
            previous = sqlx::query_as::<_, (i64, Vec<u8>)>(
                "SELECT version,checksum FROM _sqlx_migrations ORDER BY version",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE runtime_control SET external_sends_enabled=false WHERE id=1")
                .execute(&pool)
                .await
                .unwrap();
        }
        MIGRATOR.run(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 18);
        if upgrade {
            assert_eq!(previous,sqlx::query_as::<_,(i64,Vec<u8>)>("SELECT version,checksum FROM _sqlx_migrations WHERE version<18 ORDER BY version").fetch_all(&pool).await.unwrap());
            let enabled: bool =
                sqlx::query_scalar("SELECT external_sends_enabled FROM runtime_control WHERE id=1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(!enabled, "upgrade cannot re-enable provider sends");
        }
        let row=sqlx::query("SELECT relrowsecurity,relforcerowsecurity,has_table_privilege('kyro_api','public.chat_stream_chunks','INSERT') AS api_insert,has_table_privilege('kyro_worker','public.chat_stream_chunks','UPDATE') AS worker_update FROM pg_class WHERE oid='public.chat_stream_chunks'::regclass").fetch_one(&pool).await.unwrap();
        assert!(row.get::<bool, _>("relrowsecurity") && row.get::<bool, _>("relforcerowsecurity"));
        assert!(!row.get::<bool, _>("api_insert") && !row.get::<bool, _>("worker_update"));
        pool.close().await;
        // The name was generated here, after asserting the source is a test database.
        sqlx::query(&format!("DROP DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
    }
}
