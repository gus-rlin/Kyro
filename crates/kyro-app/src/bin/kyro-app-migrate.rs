use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = std::env::var("KYRO_APP_DATABASE_ADMIN_URL")
        .map_err(|_| "missing KYRO_APP_DATABASE_ADMIN_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|_| "application migration database unavailable")?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    println!("application migrations applied");
    Ok(())
}
