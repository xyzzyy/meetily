use sqlx::{migrate::MigrateDatabase, Result, Sqlite, SqlitePool, Transaction};
use std::fs;
use std::path::Path;

use super::recovery;

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
            // A WAL next to a missing database belongs to an earlier file. SQLite
            // would apply it to the new one, so move it aside first.
            if let Some(moved) = recovery::set_aside_orphaned_wal(Path::new(tauri_db_path))
                .map_err(sqlx::Error::Io)?
            {
                log::warn!(
                    "Moved a WAL left from an earlier database file to {}",
                    moved.display()
                );
            }
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

        if let Err(error) = sqlx::migrate!("./migrations").run(&pool).await {
            // Close before returning so no connection still has the file open if
            // the caller goes on to inspect or repair it.
            pool.close().await;
            return Err(error.into());
        }

        Ok(DatabaseManager { pool })
    }

    /// Open the core database. When that fails, check a copy of the files to
    /// tell a damaged database apart from other failures.
    pub(crate) async fn open(database: &Path, backend_db_path: &str) -> Result<Self> {
        let database_path = database.to_string_lossy().to_string();
        let open_error = match Self::new(&database_path, backend_db_path).await {
            Ok(manager) => return Ok(manager),
            Err(error) => error,
        };
        if !database.exists() {
            return Err(open_error);
        }
        match recovery::is_readable(database).await {
            Ok(true) => {
                log::error!("Database connection failed; the database files are readable: {}", open_error);
                Err(open_error)
            }
            Ok(false) => {
                log::error!("The database file is damaged: {}", open_error);
                Err(sqlx::Error::Protocol(recovery::damaged_message(database, &open_error)))
            }
            Err(check_error) => {
                log::error!("Could not check the database files ({}): {}", check_error, open_error);
                Err(open_error)
            }
        }
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

        log::info!("Tauri DB path: {}", tauri_db_path);
        log::info!("Legacy backend DB path: {}", backend_db_path);

        let manager = Self::open(Path::new(&tauri_db_path), &backend_db_path).await?;
        log::info!("Database opened successfully");
        Ok(manager)
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
    use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
    use sqlx::{ConnectOptions, Connection};
    use std::path::PathBuf;

    async fn connect(path: &Path) -> SqliteConnection {
        SqliteConnectOptions::new().filename(path).connect().await.unwrap()
    }

    async fn run(connection: &mut SqliteConnection, sql: &str) {
        sqlx::query(sql).execute(&mut *connection).await.unwrap();
    }

    async fn add_meetings(connection: &mut SqliteConnection, from: usize, count: usize) {
        for index in from..from + count {
            sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, '2026-01-01', '2026-01-01')")
                .bind(format!("meeting-{index}"))
                .bind(format!("Meeting {index} {}", "x".repeat(200)))
                .execute(&mut *connection)
                .await
                .unwrap();
        }
    }

    /// A migrated app database in WAL mode, as an upgraded install has.
    async fn app_database(path: &Path) {
        let missing_legacy = path.with_extension("missing");
        let manager = DatabaseManager::new(&path.to_string_lossy(), &missing_legacy.to_string_lossy())
            .await
            .unwrap();
        manager.pool.close().await;
        let mut connection = connect(path).await;
        run(&mut connection, "PRAGMA journal_mode=WAL").await;
        connection.close().await.unwrap();
    }

    async fn meeting_count(manager: &DatabaseManager) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM meetings").fetch_one(manager.pool()).await.unwrap()
    }

    async fn quick_check(manager: &DatabaseManager) -> String {
        sqlx::query_scalar("PRAGMA quick_check").fetch_one(manager.pool()).await.unwrap()
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Leaves `database` deleted with its uncheckpointed WAL still next to it, as
    /// happens when a user deletes the file to start over or to re-import.
    async fn delete_database_leaving_its_wal(database: &Path) -> Vec<u8> {
        app_database(database).await;
        let mut connection = connect(database).await;
        add_meetings(&mut connection, 0, 50).await;
        run(&mut connection, "PRAGMA wal_autocheckpoint=0").await;
        run(&mut connection, "PRAGMA wal_checkpoint(TRUNCATE)").await;
        run(&mut connection, "CREATE TABLE added_later (value TEXT)").await;
        add_meetings(&mut connection, 50, 300).await;
        let wal = fs::read(recovery::wal_path(database)).unwrap();
        connection.close().await.unwrap();
        fs::remove_file(database).unwrap();
        fs::write(recovery::wal_path(database), &wal).unwrap();
        fs::write(PathBuf::from(format!("{}-shm", database.display())), b"stale index").unwrap();
        wal
    }

    fn orphaned_wal(dir: &Path) -> PathBuf {
        let names = file_names(dir);
        let name = names
            .iter()
            .find(|name| name.starts_with("meeting_minutes.orphaned-") && name.ends_with(".sqlite-wal"))
            .unwrap_or_else(|| panic!("the old WAL is kept: {names:?}"));
        dir.join(name)
    }

    #[tokio::test]
    async fn a_wal_from_a_deleted_database_is_not_applied_to_the_imported_copy() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("meeting_minutes.sqlite");
        let legacy = dir.path().join("meeting_minutes.db");
        let old_wal = delete_database_leaving_its_wal(&database).await;
        app_database(&legacy).await;
        let mut connection = connect(&legacy).await;
        add_meetings(&mut connection, 0, 3).await;
        connection.close().await.unwrap();

        let manager = DatabaseManager::open(&database, &legacy.to_string_lossy()).await.unwrap();
        assert_eq!(quick_check(&manager).await, "ok");
        assert_eq!(meeting_count(&manager).await, 3, "the database is the imported copy");
        manager.pool.close().await;

        assert_eq!(fs::read(orphaned_wal(dir.path())).unwrap(), old_wal, "the old WAL is kept unchanged");
        assert!(!dir.path().join("meeting_minutes.sqlite-shm").exists() || {
            // A new connection may create its own index; it must not be the stale one.
            fs::read(dir.path().join("meeting_minutes.sqlite-shm")).unwrap() != b"stale index"
        });
    }

    #[tokio::test]
    async fn a_wal_from_a_deleted_database_is_not_applied_to_a_new_database() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("meeting_minutes.sqlite");
        let old_wal = delete_database_leaving_its_wal(&database).await;

        let manager = DatabaseManager::open(&database, &dir.path().join("none.db").to_string_lossy())
            .await
            .unwrap();
        assert_eq!(quick_check(&manager).await, "ok");
        assert_eq!(meeting_count(&manager).await, 0);
        manager.pool.close().await;
        assert_eq!(fs::read(orphaned_wal(dir.path())).unwrap(), old_wal);
    }

    #[tokio::test]
    async fn a_damaged_database_is_reported_and_nothing_is_added_on_any_launch() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("meeting_minutes.sqlite");
        let backend = dir.path().join("meeting_minutes.db").to_string_lossy().to_string();
        app_database(&database).await;
        let mut connection = connect(&database).await;
        add_meetings(&mut connection, 0, 300).await;
        connection.close().await.unwrap();

        // Damage the schema page, which every open reads.
        let mut main = fs::read(&database).unwrap();
        main[100..2048].fill(0xAB);
        fs::write(&database, &main).unwrap();

        let mut names = None;
        for _ in 0..3 {
            let error = DatabaseManager::open(&database, &backend)
                .await
                .err()
                .expect("a damaged database cannot open");
            let message = error.to_string();
            assert!(message.contains("damaged") && message.contains("did not delete or replace it"), "{message}");
            assert!(message.contains(&database.display().to_string()), "{message}");
            assert!(database.exists());
            let now = file_names(dir.path());
            assert!(now.iter().all(|name| !name.contains("orphaned") && !name.contains("corrupt")), "{now:?}");
            assert_eq!(names.get_or_insert_with(|| now.clone()), &now, "repeated launches add nothing");
        }
    }

    #[tokio::test]
    async fn a_readable_database_that_fails_for_another_reason_reports_that_failure() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("meeting_minutes.sqlite");
        let backend = dir.path().join("meeting_minutes.db").to_string_lossy().to_string();
        app_database(&database).await;
        let mut connection = connect(&database).await;
        run(&mut connection, "UPDATE _sqlx_migrations SET checksum = X'00' WHERE version = (SELECT MIN(version) FROM _sqlx_migrations)").await;
        connection.close().await.unwrap();
        let main = fs::read(&database).unwrap();
        let before = file_names(dir.path());

        let error = DatabaseManager::open(&database, &backend).await.err().unwrap();
        assert!(!error.to_string().contains("damaged"), "{error}");
        assert_eq!(fs::read(&database).unwrap(), main);
        assert_eq!(file_names(dir.path()), before);
    }
}
