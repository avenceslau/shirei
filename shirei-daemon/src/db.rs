use anyhow::Result;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::path::PathBuf;

pub struct Db {
    pub pool: SqlitePool,
}

impl Db {
    pub async fn new(state_dir: PathBuf) -> Result<Self> {
        let db_path = state_dir.join("shirei.db");
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./src/migrations").run(&pool).await?;
        Ok(Self { pool })
    }
}
