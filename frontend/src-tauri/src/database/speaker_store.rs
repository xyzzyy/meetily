use sqlx::{migrate::MigrateDatabase, Sqlite, SqlitePool};
use std::path::Path;

#[derive(Clone)]
pub struct SpeakerFeatureStore {
    pool: SqlitePool,
}

impl SpeakerFeatureStore {
    pub async fn new(path: &Path) -> Result<Self, sqlx::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
        }
        let url = path.to_string_lossy().to_string();
        if !path.exists() {
            Sqlite::create_database(&url).await?;
        }
        let pool = SqlitePool::connect(&url).await?;
        sqlx::migrate!("./speaker-migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn new_from_app_handle(
        app: &tauri::AppHandle,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let app_data_dir = crate::storage_safety::branch_app_data_dir(app)?;
        Ok(Self::new(&app_data_dir.join("speaker_recognition.sqlite")).await?)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
