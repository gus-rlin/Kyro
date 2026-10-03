#[path = "../migrate.rs"]
mod migrate;

use kyro_store::Store;
use sqlx::Row;
use std::{env, process};

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
    let store = Store::connect(&database_url, 1).await.map_err(|_| ())?;
    let role = sqlx::query(
        "SELECT current_user AS role_name, r.rolsuper, r.rolbypassrls, r.rolcreaterole \
         FROM pg_catalog.pg_roles r WHERE r.rolname = current_user",
    )
    .fetch_one(&store.pool)
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

    migrate::run_migrations(&store.pool).await.map_err(|_| ())
}
