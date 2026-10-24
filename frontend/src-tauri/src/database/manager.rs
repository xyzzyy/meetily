use sqlx::{migrate::MigrateDatabase, Result, Sqlite, SqlitePool, Transaction};
use std::fs;
use std::path::Path;

/// Keep a copy of the database and move its WAL/SHM files aside, named with
/// a unique `.corrupt-<timestamp>-<id>` suffix, so earlier backups are preserved.
fn quarantine_database_files(
    db_path: &Path,
    wal_path: &Path,
    shm_path: &Path,
) -> std::io::Result<()> {
    let stamp = format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        uuid::Uuid::new_v4()
    );
    let with_suffix = |path: &Path| {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".corrupt-{stamp}"));
        std::path::PathBuf::from(name)
    };

    if db_path.exists() {
        let backup = with_suffix(db_path);
        fs::copy(db_path, &backup)?;
        log::warn!("Copied database to {}", backup.display());
    }
    for path in [wal_path, shm_path] {
        if path.exists() {
            let moved = with_suffix(path);
            fs::rename(path, &moved)?;
            log::warn!("Moved {} to {}", path.display(), moved.display());
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct DatabaseManager {
    pool: SqlitePool,
}

impl DatabaseManager {
    pub async fn new(tauri_db_path: &str, backend_db_path: &str) -> Result<Self> {
        if let Some(parent_dir) = Path::new(tauri_db_path).parent() {
            if !parent_dir.exists() {
                fs::create_dir_all(parent_dir).map_err(|e| sqlx::Error::Io(e))?;
            }
        }

        if !Path::new(tauri_db_path).exists() {
            if Path::new(backend_db_path).exists() {
                log::info!(
                    "Copying database from {} to {}",
                    backend_db_path,
                    tauri_db_path
                );
                fs::copy(backend_db_path, tauri_db_path).map_err(|e| sqlx::Error::Io(e))?;
            } else {
                log::info!("Creating database at {}", tauri_db_path);
                Sqlite::create_database(tauri_db_path).await?;
            }
        }

        super::compatibility::prepare_upstream_compatible_core(Path::new(tauri_db_path))
            .await
            .map_err(sqlx::Error::Protocol)?;

        let pool = SqlitePool::connect(tauri_db_path).await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(DatabaseManager { pool })
    }

    // NOTE: So for the first time users they needs to start the application
    // after they can just delete the existing .sqlite file and then copy the existing .db file to
    // the current app dir, So the system detects legacy db and copy it and starts with that data
    // (Newly created .sqlite with the copied content from .db)
    pub async fn new_from_app_handle(app_handle: &tauri::AppHandle) -> Result<Self> {
        // Resolve the app's data directory
        let app_data_dir = crate::storage_safety::branch_app_data_dir(app_handle)
            .map_err(|message| sqlx::Error::Protocol(message))?;
        if !app_data_dir.exists() {
            fs::create_dir_all(&app_data_dir).map_err(|e| sqlx::Error::Io(e))?;
        }

        // Define database paths
        let tauri_db_path = app_data_dir
            .join("meeting_minutes.sqlite")
            .to_string_lossy()
            .to_string();
        // Legacy backend DB path (for auto-migration if exists)
        let backend_db_path = app_data_dir
            .join("meeting_minutes.db")
            .to_string_lossy()
            .to_string();

        // WAL file paths, set aside if the database cannot be opened
        let wal_path = app_data_dir.join("meeting_minutes.sqlite-wal");
        let shm_path = app_data_dir.join("meeting_minutes.sqlite-shm");

        log::info!("Tauri DB path: {}", tauri_db_path);
        log::info!("Legacy backend DB path: {}", backend_db_path);

        // Try to open database with defensive WAL handling
        match Self::new(&tauri_db_path, &backend_db_path).await {
            Ok(db_manager) => {
                log::info!("Database opened successfully");
                Ok(db_manager)
            }
            Err(e) => {
                // Check if error is due to corrupted WAL file
                let error_msg = e.to_string();
                if error_msg.contains("malformed") || error_msg.contains("corrupt") {
                    log::warn!("Database appears corrupted. Attempting recovery...");
                    log::warn!("Error details: {}", error_msg);

                    // The WAL can hold committed changes that were never checkpointed,
                    // so move it aside instead of deleting it.
                    if let Err(quarantine_err) = quarantine_database_files(
                        Path::new(&tauri_db_path),
                        &wal_path,
                        &shm_path,
                    ) {
                        log::error!(
                            "Could not finish setting the database files aside; stopping recovery: {}",
                            quarantine_err
                        );
                        return Err(e);
                    }

                    // Retry connection without WAL files
                    log::info!("Retrying database connection without the WAL...");
                    match Self::new(&tauri_db_path, &backend_db_path).await {
                        Ok(db_manager) => {
                            log::info!("Database opened successfully after WAL recovery (original files kept alongside)");
                            Ok(db_manager)
                        }
                        Err(retry_err) => {
                            log::error!("Database connection failed even without the WAL: {}", retry_err);
                            Err(retry_err)
                        }
                    }
                } else {
                    // Not a WAL-related error, propagate original error
                    log::error!("Database connection failed: {}", error_msg);
                    Err(e)
                }
            }
        }
    }

    /// Check if this is the first launch (sqlite database doesn't exist yet)
    pub async fn is_first_launch(app_handle: &tauri::AppHandle) -> Result<bool> {
        let app_data_dir = crate::storage_safety::branch_app_data_dir(app_handle)
            .map_err(|message| sqlx::Error::Protocol(message))?;

        let tauri_db_path = app_data_dir.join("meeting_minutes.sqlite");

        Ok(!tauri_db_path.exists())
    }

    /// Import a legacy database from the specified path and initialize
    pub async fn import_legacy_database(
        app_handle: &tauri::AppHandle,
        legacy_db_path: &str,
    ) -> Result<Self> {
        let app_data_dir = crate::storage_safety::branch_app_data_dir(app_handle)
            .map_err(|message| sqlx::Error::Protocol(message))?;

        if !app_data_dir.exists() {
            fs::create_dir_all(&app_data_dir).map_err(|e| sqlx::Error::Io(e))?;
        }

        // Copy legacy database to app data directory as meeting_minutes.db
        let target_legacy_path = app_data_dir.join("meeting_minutes.db");
        log::info!(
            "Copying legacy database from {} to {}",
            legacy_db_path,
            target_legacy_path.display()
        );

        fs::copy(legacy_db_path, &target_legacy_path).map_err(|e| sqlx::Error::Io(e))?;

        // Now use the standard initialization which will detect and migrate the legacy db
        Self::new_from_app_handle(app_handle).await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn with_transaction<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Transaction<'_, Sqlite>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut tx = self.pool.begin().await?;
        let result = f(&mut tx).await;

        match result {
            Ok(val) => {
                tx.commit().await?;
                Ok(val)
            }
            Err(err) => {
                tx.rollback().await?;
                Err(err)
            }
        }
    }

    /// Cleanup database connection and checkpoint WAL
    /// This should be called on application shutdown to ensure:
    /// - All WAL changes are written to the main database file
    /// - The .wal and .shm files are deleted
    /// - Connection pool is gracefully closed
    pub async fn cleanup(&self) -> Result<()> {
        log::info!("Starting database cleanup...");

        // Force checkpoint of WAL to main database file and remove WAL file
        // TRUNCATE mode: checkpoints all pages AND deletes the WAL file
        match sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
        {
            Ok(_) => log::info!("WAL checkpoint completed successfully"),
            Err(e) => log::warn!("WAL checkpoint failed (non-fatal): {}", e),
        }

        // Close the connection pool gracefully
        self.pool.close().await;
        log::info!("Database connection pool closed");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_keeps_every_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("meeting_minutes.sqlite");
        let wal = dir.path().join("meeting_minutes.sqlite-wal");
        let shm = dir.path().join("meeting_minutes.sqlite-shm");
        fs::write(&db, b"main").unwrap();
        fs::write(&wal, b"unflushed changes").unwrap();
        fs::write(&shm, b"index").unwrap();

        quarantine_database_files(&db, &wal, &shm).unwrap();

        assert!(db.exists(), "the database stays in place for the retry");
        assert!(!wal.exists() && !shm.exists());
        let saved: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".corrupt-"))
            .collect();
        assert_eq!(saved.len(), 3, "main, wal and shm are all preserved: {saved:?}");
        let wal_copy = saved.iter().find(|name| name.contains("-wal.corrupt-")).unwrap();
        assert_eq!(fs::read(dir.path().join(wal_copy)).unwrap(), b"unflushed changes");
    }

    #[test]
    fn quarantine_tolerates_missing_wal_and_shm() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("meeting_minutes.sqlite");
        fs::write(&db, b"main").unwrap();

        quarantine_database_files(
            &db,
            &dir.path().join("meeting_minutes.sqlite-wal"),
            &dir.path().join("meeting_minutes.sqlite-shm"),
        )
        .unwrap();
    }

    #[test]
    fn repeated_quarantine_preserves_each_backup() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("meeting_minutes.sqlite");
        let wal = dir.path().join("meeting_minutes.sqlite-wal");
        let shm = dir.path().join("meeting_minutes.sqlite-shm");
        for contents in [b"first", b"later"] {
            for path in [&db, &wal, &shm] {
                fs::write(path, contents).unwrap();
            }
            quarantine_database_files(&db, &wal, &shm).unwrap();
        }
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().contains(".corrupt-"))
            .map(|path| fs::read(path).unwrap())
            .collect();
        assert_eq!(backups.len(), 6);
        assert_eq!(backups.iter().filter(|contents| *contents == b"first").count(), 3);
        assert_eq!(backups.iter().filter(|contents| *contents == b"later").count(), 3);
    }
}
