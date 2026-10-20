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
        let store = Self::new(&app_data_dir.join("speaker_recognition.sqlite")).await?;
        // Earlier builds created a new remembered voice each time; fold any
        // same-name duplicates into one so attributing adds to a single voice.
        // Only the app's own store: temporary stores used during schema
        // conversion must stay untouched.
        match super::repositories::speaker_profile::SpeakerProfilesRepository::merge_duplicate_names(store.pool()).await {
            Ok(0) => {}
            Ok(removed) => log::info!("Merged {removed} duplicate remembered voice(s)"),
            Err(error) => log::warn!("Could not merge duplicate remembered voices: {error}"),
        }
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reopening_a_previously_converted_store_repairs_missing_voice_history_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("speakers.sqlite");
        let store = SpeakerFeatureStore::new(&path).await.unwrap();
        sqlx::query("INSERT INTO speaker_profiles (id, name, embedding, model_version, sample_count, speech_seconds) VALUES ('p1', 'Alice', x'0000803f', 'v1', 2, 12)")
            .execute(store.pool()).await.unwrap();
        sqlx::query("INSERT INTO transcript_speakers (transcript_id, meeting_id, speaker_id, speaker_profile_id, diarization_method) VALUES ('t1', 'm1', 's1', 'p1', 'manual'), ('t2', 'm2', 's2', 'p1', 'wespeaker')")
            .execute(store.pool()).await.unwrap();
        // Simulate a database converted before the repair migration existed.
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 20261021000000")
            .execute(store.pool()).await.unwrap();
        store.pool().close().await;
        let store = SpeakerFeatureStore::new(&path).await.unwrap();
        let sample: (String, Vec<u8>, i64, f64) = sqlx::query_as(
            "SELECT meeting_id, embedding, sample_count, speech_seconds FROM speaker_profile_samples",
        ).fetch_one(store.pool()).await.unwrap();
        assert_eq!(sample, ("m1".into(), vec![0, 0, 128, 63], 2, 12.0));
        // A later manual correction is not another contribution. Even a repeat
        // of the repair must preserve the existing sample and its voiceprint.
        sqlx::query("UPDATE transcript_speakers SET diarization_method = 'manual'")
            .execute(store.pool()).await.unwrap();
        sqlx::raw_sql(include_str!("../../speaker-migrations/20261021000000_recover_legacy_profile_samples.sql"))
            .execute(store.pool()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM speaker_profile_samples")
            .fetch_one(store.pool()).await.unwrap();
        assert_eq!(count, 1);
        store.pool().close().await;
        let store = SpeakerFeatureStore::new(&path).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM speaker_profile_samples")
            .fetch_one(store.pool()).await.unwrap();
        assert_eq!(count, 1);
    }
}
