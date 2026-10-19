use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{collections::BTreeMap, fs::File, io::Read, path::{Path, PathBuf}};

const LEGACY_MIGRATIONS: [i64; 2] = [20260626000100, 20260714000000];
const MARKER_FILE: &str = "speaker-recognition-migration-state.json";

#[derive(Debug, Serialize, Deserialize)]
struct MigrationMarker {
    backup_path: PathBuf,
    source_database_hash: String,
    meeting_count: i64,
    transcript_count: i64,
    feature_row_counts: BTreeMap<String, i64>,
    phase: String,
}

pub async fn prepare_upstream_compatible_core(core_path: &Path) -> Result<(), String> {
    let parent = core_path.parent().ok_or_else(|| "Core database has no parent".to_string())?;
    let marker_path = parent.join(MARKER_FILE);
    let temp_feature_path = parent.join("speaker_recognition.sqlite.tmp");
    let feature_path = parent.join("speaker_recognition.sqlite");

    if marker_path.exists() {
        recover_interrupted_conversion(
            core_path,
            &marker_path,
            &temp_feature_path,
            &feature_path,
        )
        .await?;
    }

    if !core_path.exists() {
        return Ok(());
    }
    let url = core_path.to_string_lossy().to_string();
    let pool = SqlitePool::connect(&url).await.map_err(stringify)?;
    if !has_legacy_feature_schema(&pool).await? {
        pool.close().await;
        return Ok(());
    }
    if feature_path.exists() {
        pool.close().await;
        return Err(format!(
            "The branch core database still contains legacy speaker data, but {} already exists. No automatic merge was attempted.",
            feature_path.display()
        ));
    }

    sqlx::query("PRAGMA wal_checkpoint(FULL)")
        .execute(&pool)
        .await
        .map_err(stringify)?;
    let meeting_count = count(&pool, "meetings").await?;
    let transcript_count = count(&pool, "transcripts").await?;
    let source_database_hash = hash_file(core_path)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup_path = parent.join(format!(
        "meeting_minutes.before-speaker-separation-{timestamp}.sqlite"
    ));
    online_backup(&pool, &backup_path).await?;
    verify_database(&backup_path).await?;

    let mut marker = MigrationMarker {
        backup_path,
        source_database_hash,
        meeting_count,
        transcript_count,
        feature_row_counts: BTreeMap::new(),
        phase: "backup_verified".to_string(),
    };
    write_marker(&marker_path, &marker)?;

    if temp_feature_path.exists() {
        std::fs::remove_file(&temp_feature_path).map_err(stringify)?;
    }
    let feature_store = super::speaker_store::SpeakerFeatureStore::new(&temp_feature_path)
        .await
        .map_err(stringify)?;
    let expected_feature_counts = expected_legacy_feature_counts(&pool).await?;
    copy_legacy_feature_data(&pool, feature_store.pool()).await?;
    marker.feature_row_counts = feature_counts(feature_store.pool()).await?;
    if marker.feature_row_counts != expected_feature_counts {
        return Err(format!(
            "Speaker-data row-count verification failed. Expected {:?}, copied {:?}. The branch core database remains unchanged and its verified backup is at {}.",
            expected_feature_counts,
            marker.feature_row_counts,
            marker.backup_path.display()
        ));
    }
    verify_pool(feature_store.pool()).await?;
    feature_store.pool().close().await;
    marker.phase = "feature_verified".to_string();
    write_marker(&marker_path, &marker)?;

    clean_core_schema(&pool).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|error| format!("Official Meetily migration verification failed: {error}"))?;
    verify_pool(&pool).await?;
    if count(&pool, "meetings").await? != marker.meeting_count
        || count(&pool, "transcripts").await? != marker.transcript_count
    {
        pool.close().await;
        restore_backup(core_path, &marker.backup_path)?;
        return Err("Core row counts changed during speaker-data separation; the verified backup was restored.".to_string());
    }
    marker.phase = "core_cleaned".to_string();
    write_marker(&marker_path, &marker)?;
    pool.close().await;

    std::fs::rename(&temp_feature_path, &feature_path).map_err(stringify)?;
    verify_database(&feature_path).await?;
    marker.phase = "installed".to_string();
    write_marker(&marker_path, &marker)?;
    std::fs::remove_file(&marker_path).map_err(stringify)?;
    Ok(())
}

async fn has_legacy_feature_schema(pool: &SqlitePool) -> Result<bool, String> {
    let custom_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'diarization_settings'",
    )
    .fetch_one(pool)
    .await
    .map_err(stringify)?;
    let custom_migration: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version IN (?, ?)",
    )
    .bind(LEGACY_MIGRATIONS[0])
    .bind(LEGACY_MIGRATIONS[1])
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    Ok(custom_table > 0 || custom_migration > 0)
}

async fn copy_legacy_feature_data(core: &SqlitePool, feature: &SqlitePool) -> Result<(), String> {
    if table_exists(core, "diarization_settings").await? {
        copy_rows(
            core,
            feature,
            "SELECT id, enabled, mode, show_provisional_labels, post_call_refinement_enabled, overlap_handling, speaker_review_enabled, updated_at FROM diarization_settings",
            "INSERT OR REPLACE INTO diarization_settings (id, enabled, mode, show_provisional_labels, post_call_refinement_enabled, overlap_handling, speaker_review_enabled, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        ).await?;
    }
    if table_exists(core, "meeting_diarization_status").await? {
        copy_table_via_json(core, feature, "meeting_diarization_status").await?;
    }
    if table_exists(core, "speaker_segments").await? {
        copy_table_via_json(core, feature, "speaker_segments").await?;
    }
    if table_exists(core, "speaker_profiles").await? {
        copy_table_via_json(core, feature, "speaker_profiles").await?;
    }
    if table_exists(core, "diarization_runs").await? {
        copy_legacy_runs(core, feature).await?;
    }

    let columns = table_columns(core, "transcripts").await?;
    if columns.iter().any(|column| column == "speaker_id") {
        let value = |name: &str, fallback: &str| {
            if columns.iter().any(|column| column == name) { name.to_string() } else { fallback.to_string() }
        };
        let query = format!(
            "SELECT id, meeting_id, audio_start_time, audio_end_time, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM transcripts",
            value("speaker_id", "NULL"), value("speaker_label", "NULL"),
            value("speaker_profile_id", "NULL"), value("speaker_source", "NULL"),
            value("speaker_color", "NULL"), value("is_overlap", "0"),
            value("candidate_speaker_ids", "NULL"), value("diarization_status", "'none'"),
            value("diarization_method", "NULL"), value("diarization_confidence", "NULL")
        );
        let rows = sqlx::query(&query).fetch_all(core).await.map_err(stringify)?;
        for row in rows {
            sqlx::query(
                "INSERT OR REPLACE INTO transcript_speakers (
                    transcript_id, meeting_id, audio_start_time, audio_end_time,
                    speaker_id, speaker_label, speaker_profile_id, speaker_source,
                    speaker_color, is_overlap, candidate_speaker_ids, diarization_status,
                    diarization_method, diarization_confidence, published
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(row.try_get::<String, _>(0).map_err(stringify)?)
            .bind(row.try_get::<String, _>(1).map_err(stringify)?)
            .bind(row.try_get::<Option<f64>, _>(2).map_err(stringify)?)
            .bind(row.try_get::<Option<f64>, _>(3).map_err(stringify)?)
            .bind(row.try_get::<Option<String>, _>(4).map_err(stringify)?)
            .bind(row.try_get::<Option<String>, _>(5).map_err(stringify)?)
            .bind(row.try_get::<Option<String>, _>(6).map_err(stringify)?)
            .bind(row.try_get::<Option<String>, _>(7).map_err(stringify)?)
            .bind(row.try_get::<Option<String>, _>(8).map_err(stringify)?)
            .bind(row.try_get::<i64, _>(9).unwrap_or(0))
            .bind(row.try_get::<Option<String>, _>(10).map_err(stringify)?)
            .bind(row.try_get::<String, _>(11).unwrap_or_else(|_| "none".to_string()))
            .bind(row.try_get::<Option<String>, _>(12).map_err(stringify)?)
            .bind(row.try_get::<Option<f64>, _>(13).map_err(stringify)?)
            .execute(feature)
            .await
            .map_err(stringify)?;
        }
    }
    Ok(())
}

// These legacy tables have compatible column names. JSON object copying keeps
// this converter tolerant of the first and second legacy migrations having
// been applied independently.
async fn copy_table_via_json(core: &SqlitePool, feature: &SqlitePool, table: &str) -> Result<(), String> {
    let source_columns = table_columns(core, table).await?;
    let target_columns = table_columns(feature, table).await?;
    let columns = source_columns.into_iter().filter(|column| target_columns.contains(column)).collect::<Vec<_>>();
    if columns.is_empty() { return Ok(()); }
    let names = columns.join(", ");
    let core_path: String = sqlx::query("PRAGMA database_list")
        .fetch_all(core).await.map_err(stringify)?
        .into_iter()
        .find_map(|row| {
            (row.try_get::<String, _>("name").ok().as_deref() == Some("main"))
                .then(|| row.try_get::<String, _>("file").ok())
                .flatten()
        })
        .ok_or_else(|| "Could not resolve the legacy branch database path".to_string())?;
    let mut connection = feature.acquire().await.map_err(stringify)?;
    sqlx::query("ATTACH DATABASE ? AS legacy_core")
        .bind(core_path).execute(&mut *connection).await.map_err(stringify)?;
    let insert = format!(
        "INSERT OR REPLACE INTO main.{table} ({names}) SELECT {names} FROM legacy_core.{table}"
    );
    let result = sqlx::query(&insert).execute(&mut *connection).await.map_err(stringify);
    let detach = sqlx::query("DETACH DATABASE legacy_core").execute(&mut *connection).await.map_err(stringify);
    result?;
    detach?;
    Ok(())
}

async fn copy_legacy_runs(core: &SqlitePool, feature: &SqlitePool) -> Result<(), String> {
    copy_table_via_json(core, feature, "diarization_runs").await?;
    sqlx::query(
        "UPDATE diarization_runs SET status = CASE status
            WHEN 'completed' THEN 'applied'
            WHEN 'running' THEN 'prepared'
            WHEN 'processing' THEN 'prepared'
            WHEN 'cancelled' THEN 'failed'
            WHEN 'prepared' THEN 'prepared'
            WHEN 'applying' THEN 'applying'
            WHEN 'applied' THEN 'applied'
            WHEN 'fallback_to_live' THEN 'fallback_to_live'
            WHEN 'needs_review' THEN 'needs_review'
            ELSE 'failed' END",
    )
    .execute(feature)
    .await
    .map_err(stringify)?;
    Ok(())
}

async fn clean_core_schema(pool: &SqlitePool) -> Result<(), String> {
    let mut transaction = pool.begin().await.map_err(stringify)?;
    for index in ["idx_transcripts_meeting_diarization", "idx_speaker_segments_meeting_time", "idx_diarization_runs_meeting_started", "idx_speaker_profiles_name"] {
        sqlx::query(&format!("DROP INDEX IF EXISTS {index}")).execute(&mut *transaction).await.map_err(stringify)?;
    }
    for table in ["diarization_settings", "meeting_diarization_status", "speaker_segments", "speaker_profiles", "diarization_runs"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {table}")).execute(&mut *transaction).await.map_err(stringify)?;
    }
    sqlx::query(
        "CREATE TABLE transcripts_upstream_compat (
            id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, transcript TEXT NOT NULL,
            timestamp TEXT NOT NULL, summary TEXT, action_items TEXT, key_points TEXT,
            audio_start_time REAL, audio_end_time REAL, duration REAL, speaker TEXT,
            FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
         )",
    ).execute(&mut *transaction).await.map_err(stringify)?;
    sqlx::query(
        "INSERT INTO transcripts_upstream_compat (
            id, meeting_id, transcript, timestamp, summary, action_items, key_points,
            audio_start_time, audio_end_time, duration, speaker
         ) SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
                  audio_start_time, audio_end_time, duration, speaker FROM transcripts",
    ).execute(&mut *transaction).await.map_err(stringify)?;
    sqlx::query("DROP TABLE transcripts").execute(&mut *transaction).await.map_err(stringify)?;
    sqlx::query("ALTER TABLE transcripts_upstream_compat RENAME TO transcripts").execute(&mut *transaction).await.map_err(stringify)?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version IN (?, ?)")
        .bind(LEGACY_MIGRATIONS[0]).bind(LEGACY_MIGRATIONS[1])
        .execute(&mut *transaction).await.map_err(stringify)?;
    transaction.commit().await.map_err(stringify)
}

async fn recover_interrupted_conversion(core_path: &Path, marker_path: &Path, temp_feature: &Path, feature: &Path) -> Result<(), String> {
    let marker: MigrationMarker = serde_json::from_slice(&std::fs::read(marker_path).map_err(stringify)?).map_err(stringify)?;
    verify_database(&marker.backup_path).await?;
    let pool = SqlitePool::connect(&core_path.to_string_lossy()).await.map_err(stringify)?;
    let legacy = has_legacy_feature_schema(&pool).await?;
    pool.close().await;
    if !legacy {
        if feature.exists() {
            verify_database(feature).await?;
            std::fs::remove_file(marker_path).map_err(stringify)?;
            return Ok(());
        }
        if temp_feature.exists() && verify_database(temp_feature).await.is_ok() {
            std::fs::rename(temp_feature, feature).map_err(stringify)?;
            std::fs::remove_file(marker_path).map_err(stringify)?;
            return Ok(());
        }
        restore_backup(core_path, &marker.backup_path)?;
        return Err("An interrupted speaker-data conversion had no valid feature database. The verified core backup was restored; startup stopped without discarding speaker data.".to_string());
    }
    // The core transaction never committed. Keep the core untouched and allow
    // the normal conversion path to rebuild the temporary feature database.
    if temp_feature.exists() { std::fs::remove_file(temp_feature).map_err(stringify)?; }
    std::fs::remove_file(marker_path).map_err(stringify)?;
    Ok(())
}

async fn online_backup(pool: &SqlitePool, backup: &Path) -> Result<(), String> {
    sqlx::query("VACUUM INTO ?").bind(backup.to_string_lossy().to_string()).execute(pool).await.map_err(stringify)?;
    Ok(())
}

fn restore_backup(core: &Path, backup: &Path) -> Result<(), String> {
    std::fs::copy(backup, core).map_err(stringify)?;
    Ok(())
}

async fn verify_database(path: &Path) -> Result<(), String> {
    let pool = SqlitePool::connect(&path.to_string_lossy()).await.map_err(stringify)?;
    let result = verify_pool(&pool).await;
    pool.close().await;
    result
}

async fn verify_pool(pool: &SqlitePool) -> Result<(), String> {
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check").fetch_one(pool).await.map_err(stringify)?;
    if integrity != "ok" { return Err(format!("SQLite integrity check failed: {integrity}")); }
    let foreign_keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check").fetch_one(pool).await.map_err(stringify)?;
    if foreign_keys != 0 { return Err(format!("SQLite foreign-key check found {foreign_keys} errors")); }
    Ok(())
}

async fn feature_counts(pool: &SqlitePool) -> Result<BTreeMap<String, i64>, String> {
    let mut counts = BTreeMap::new();
    for table in ["transcript_speakers", "speaker_segments", "speaker_profiles", "diarization_runs", "meeting_diarization_status"] {
        counts.insert(table.to_string(), count(pool, table).await?);
    }
    Ok(counts)
}

async fn expected_legacy_feature_counts(pool: &SqlitePool) -> Result<BTreeMap<String, i64>, String> {
    let mut counts = BTreeMap::new();
    for (target, source) in [
        ("speaker_segments", "speaker_segments"),
        ("speaker_profiles", "speaker_profiles"),
        ("diarization_runs", "diarization_runs"),
        ("meeting_diarization_status", "meeting_diarization_status"),
    ] {
        let value = if table_exists(pool, source).await? { count(pool, source).await? } else { 0 };
        counts.insert(target.to_string(), value);
    }
    let transcript_count = if table_columns(pool, "transcripts").await?.iter().any(|column| column == "speaker_id") {
        count(pool, "transcripts").await?
    } else { 0 };
    counts.insert("transcript_speakers".to_string(), transcript_count);
    Ok(counts)
}

async fn count(pool: &SqlitePool, table: &str) -> Result<i64, String> {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}")).fetch_one(pool).await.map_err(stringify)
}

async fn table_exists(pool: &SqlitePool, table: &str) -> Result<bool, String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?").bind(table).fetch_one(pool).await.map_err(stringify)?;
    Ok(count > 0)
}

async fn table_columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>, String> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})")).fetch_all(pool).await.map_err(stringify)?;
    rows.into_iter().map(|row| row.try_get::<String, _>("name").map_err(stringify)).collect()
}

async fn copy_rows(_core: &SqlitePool, _feature: &SqlitePool, _select: &str, _insert: &str) -> Result<(), String> {
    // diarization_settings is covered by the generic copier; this signature is
    // retained only to keep the call site explicit about the singleton row.
    copy_table_via_json(_core, _feature, "diarization_settings").await
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(stringify)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(stringify)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_marker(path: &Path, marker: &MigrationMarker) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(marker).map_err(stringify)?).map_err(stringify)?;
    std::fs::rename(temporary, path).map_err(stringify)
}

fn stringify(error: impl std::fmt::Display) -> String { error.to_string() }

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{migrate::MigrateDatabase, Sqlite};

    #[tokio::test]
    async fn separates_legacy_speaker_schema_and_accepts_future_core_migration() {
        let temp = tempfile::tempdir().unwrap();
        let core_path = temp.path().join("meeting_minutes.sqlite");
        let url = core_path.to_string_lossy().to_string();
        Sqlite::create_database(&url).await.unwrap();
        let pool = SqlitePool::connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'Test', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        ).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker)
             VALUES ('t1', 'm1', 'hello', '00:01', 1.0, 2.0, 1.0, 'mic')",
        ).execute(&pool).await.unwrap();
        for statement in [
            "ALTER TABLE transcripts ADD COLUMN speaker_id TEXT",
            "ALTER TABLE transcripts ADD COLUMN speaker_label TEXT",
            "ALTER TABLE transcripts ADD COLUMN speaker_color TEXT",
            "ALTER TABLE transcripts ADD COLUMN is_overlap INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE transcripts ADD COLUMN diarization_status TEXT NOT NULL DEFAULT 'none'",
            "ALTER TABLE transcripts ADD COLUMN diarization_method TEXT",
            "ALTER TABLE transcripts ADD COLUMN diarization_confidence REAL",
            "ALTER TABLE transcripts ADD COLUMN speaker_profile_id TEXT",
            "ALTER TABLE transcripts ADD COLUMN speaker_source TEXT",
            "ALTER TABLE transcripts ADD COLUMN candidate_speaker_ids TEXT",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        sqlx::query(
            "UPDATE transcripts SET speaker_id='speaker-1', speaker_label='Alice', speaker_source='microphone', diarization_status='final' WHERE id='t1'",
        ).execute(&pool).await.unwrap();
        sqlx::query(
            "CREATE TABLE diarization_settings (
                id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0,
                mode TEXT NOT NULL DEFAULT 'live_plus_post_call',
                show_provisional_labels INTEGER NOT NULL DEFAULT 1,
                post_call_refinement_enabled INTEGER NOT NULL DEFAULT 1,
                overlap_handling TEXT NOT NULL DEFAULT 'multiple_speakers',
                speaker_review_enabled INTEGER NOT NULL DEFAULT 1,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        ).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO diarization_settings (id, enabled) VALUES ('1', 1)")
            .execute(&pool).await.unwrap();
        for table in [
            "CREATE TABLE meeting_diarization_status (meeting_id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0, live_enabled INTEGER NOT NULL DEFAULT 0, post_call_enabled INTEGER NOT NULL DEFAULT 0, current_status TEXT NOT NULL DEFAULT 'none', quality_flags TEXT, processed_at DATETIME, updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, run_id TEXT, progress INTEGER NOT NULL DEFAULT 0, method TEXT, failure_reason TEXT, assigned_coverage REAL, transcription_success_rate REAL)",
            "CREATE TABLE speaker_segments (id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, source TEXT NOT NULL, start_time REAL NOT NULL, end_time REAL NOT NULL, speaker_id TEXT, speaker_label TEXT, confidence REAL, is_overlap INTEGER NOT NULL DEFAULT 0, diarization_status TEXT NOT NULL, diarization_method TEXT, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, speaker_profile_id TEXT, candidate_speaker_ids TEXT)",
            "CREATE TABLE speaker_profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL, embedding BLOB NOT NULL, model_version TEXT NOT NULL, sample_count INTEGER NOT NULL DEFAULT 0, speech_seconds REAL NOT NULL DEFAULT 0, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, last_used_at DATETIME)",
            "CREATE TABLE diarization_runs (id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, status TEXT NOT NULL, method TEXT NOT NULL, progress INTEGER NOT NULL DEFAULT 0, quality_flags TEXT, error TEXT, transcript_backup TEXT, assigned_coverage REAL, transcription_success_rate REAL, started_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, completed_at DATETIME)",
        ] {
            sqlx::query(table).execute(&pool).await.unwrap();
        }
        for version in LEGACY_MIGRATIONS {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)
                 VALUES (?, 'branch-only', CURRENT_TIMESTAMP, 1, x'00', 0)",
            ).bind(version).execute(&pool).await.unwrap();
        }
        pool.close().await;

        prepare_upstream_compatible_core(&core_path).await.unwrap();

        let core = SqlitePool::connect(&url).await.unwrap();
        let columns = table_columns(&core, "transcripts").await.unwrap();
        assert_eq!(columns, vec![
            "id", "meeting_id", "transcript", "timestamp", "summary", "action_items",
            "key_points", "audio_start_time", "audio_end_time", "duration", "speaker",
        ]);
        let custom_migrations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM _sqlx_migrations WHERE version IN (?, ?)",
        ).bind(LEGACY_MIGRATIONS[0]).bind(LEGACY_MIGRATIONS[1]).fetch_one(&core).await.unwrap();
        assert_eq!(custom_migrations, 0);
        let row: (String, String) = sqlx::query_as(
            "SELECT transcript, speaker FROM transcripts WHERE id='t1'",
        ).fetch_one(&core).await.unwrap();
        assert_eq!(row, ("hello".to_string(), "mic".to_string()));

        // A future official migration can use the untouched core history.
        sqlx::query("ALTER TABLE meetings ADD COLUMN upstream_050_test TEXT")
            .execute(&core).await.unwrap();
        assert!(table_columns(&core, "meetings").await.unwrap().contains(&"upstream_050_test".to_string()));
        core.close().await;

        let feature_path = temp.path().join("speaker_recognition.sqlite");
        let feature = SqlitePool::connect(&feature_path.to_string_lossy()).await.unwrap();
        let metadata: (String, String, String) = sqlx::query_as(
            "SELECT transcript_id, speaker_label, speaker_source FROM transcript_speakers WHERE transcript_id='t1'",
        ).fetch_one(&feature).await.unwrap();
        assert_eq!(metadata, ("t1".to_string(), "Alice".to_string(), "microphone".to_string()));
        feature.close().await;
        assert!(!temp.path().join(MARKER_FILE).exists());
    }
}
