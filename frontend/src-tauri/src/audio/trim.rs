use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection, SqlitePool};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex, Weak,
    },
};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

const STAGED_AUDIO: &str = ".trim-audio.mp4";
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static SUMMARY_LOCKS: LazyLock<Mutex<HashMap<String, Weak<RwLock<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn summary_lock(meeting_id: &str) -> Result<Arc<RwLock<()>>, String> {
    check_ready(meeting_id)?;
    let mut locks = SUMMARY_LOCKS
        .lock()
        .map_err(|_| "Could not lock the recording")?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    Ok(locks
        .get(meeting_id)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            let lock = Arc::new(RwLock::new(()));
            locks.insert(meeting_id.into(), Arc::downgrade(&lock));
            lock
        }))
}

/// Summary work can run together, but it must finish before audio is removed.
pub(crate) fn try_summary(meeting_id: &str) -> Result<OwnedRwLockReadGuard<()>, String> {
    summary_lock(meeting_id)?
        .try_read_owned()
        .map_err(|_| "Wait for the trim to finish before creating or saving a summary.".into())
}

pub(crate) fn try_trim(meeting_id: &str) -> Result<OwnedRwLockWriteGuard<()>, String> {
    summary_lock(meeting_id)?
        .try_write_owned()
        .map_err(|_| "Wait for summary work to finish before trimming this recording.".into())
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct TrimRange {
    pub start_seconds: f64,
    pub end_seconds: f64,
}

impl TrimRange {
    fn validate(self, duration: f64) -> Result<()> {
        if !self.start_seconds.is_finite()
            || !self.end_seconds.is_finite()
            || !duration.is_finite()
            || self.start_seconds < 0.0
            || self.end_seconds <= self.start_seconds
            || self.end_seconds > duration
        {
            bail!(
                "Set a start and end time within the recording. The end must be after the start."
            );
        }
        if self.start_seconds == 0.0 && self.end_seconds == duration {
            bail!("Change the start or end time to trim the recording.");
        }
        Ok(())
    }
}

pub(crate) fn check_ready(meeting_id: &str) -> Result<(), String> {
    if PENDING
        .lock()
        .map_err(|_| "Could not check the saved trim")?
        .contains(meeting_id)
    {
        return Err("Restart Meetily Branch to finish the saved trim before you open or change this meeting.".into());
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct TrimIntent {
    meeting_id: String,
    folder_path: String,
    original_filename: String,
    audio_hash: String,
    duration_seconds: f64,
}

async fn durable_connection(pool: &SqlitePool) -> Result<SqliteConnection> {
    Ok(SqliteConnection::connect_with(
        &pool
            .connect_options()
            .as_ref()
            .clone()
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
    )
    .await?)
}

async fn meeting_folder(core: &SqlitePool, meeting_id: &str) -> Result<PathBuf> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(core)
            .await?;
    let folder = stored
        .flatten()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| anyhow!("This meeting has no saved recording folder."))?;
    let folder = fs::canonicalize(folder).context("The recording folder is not available")?;
    // Imported databases can contain two rows for the same folder. Do not
    // change audio that another meeting still uses.
    let others: Vec<String> = sqlx::query_scalar(
        "SELECT folder_path FROM meetings WHERE id <> ? AND folder_path IS NOT NULL",
    )
    .bind(meeting_id)
    .fetch_all(core)
    .await?;
    if others
        .iter()
        .any(|other| fs::canonicalize(other).ok().as_ref() == Some(&folder))
    {
        bail!("Another meeting uses this recording folder. Use a separate folder before trimming.");
    }
    Ok(folder)
}

fn source_audio(folder: &Path) -> Result<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(STAGED_AUDIO) {
            continue;
        }
        let supported = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                super::constants::AUDIO_EXTENSIONS
                    .contains(&extension.to_ascii_lowercase().as_str())
            });
        if supported {
            if !fs::symlink_metadata(&path)?.file_type().is_file() {
                bail!("The saved audio must be a regular file.");
            }
            files.push(path);
        }
    }
    match files.len() {
        0 => bail!("This meeting has no saved audio."),
        1 => Ok(files.remove(0)),
        _ => bail!(
            "This folder contains more than one audio file. Move the extra files before trimming."
        ),
    }
}

fn hash_file(path: &Path) -> Result<String> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        bail!("The saved audio must be a regular file.");
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn sync_folder(folder: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(folder)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = folder;
    Ok(())
}

fn remove_saved_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?, // Remove links without following them.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn write_metadata(folder: &Path, intent: &TrimIntent) -> Result<()> {
    let path = folder.join("metadata.json");
    let mut metadata = if path.exists() {
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            bail!("The recording metadata must be a regular file.");
        }
        serde_json::from_slice::<serde_json::Value>(&fs::read(&path)?)?
    } else {
        serde_json::json!({"meeting_id": intent.meeting_id, "version": "1.0"})
    };
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| anyhow!("The recording metadata is not an object."))?;
    object.insert("audio_file".into(), serde_json::json!("audio.mp4"));
    object.insert(
        "duration_seconds".into(),
        serde_json::json!(intent.duration_seconds),
    );
    object.insert(
        "trimmed_at".into(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    object.insert("status".into(), serde_json::json!("needs_transcription"));
    object.remove("detected_summary_language");
    object.remove("retranscribed_at");
    object.remove("transcript_file");
    let temporary = folder.join(".trim-metadata.json.tmp");
    remove_saved_path(&temporary)?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    sync_folder(folder)
}

fn finish_files(intent: &TrimIntent) -> Result<()> {
    let folder = Path::new(&intent.folder_path);
    let mut components = Path::new(&intent.original_filename).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("The saved trim has an invalid audio filename.");
    }
    let staged = folder.join(STAGED_AUDIO);
    let target = folder.join("audio.mp4");
    if staged.try_exists()? {
        if hash_file(&staged)? != intent.audio_hash {
            bail!("The staged trim changed. The saved audio was kept.");
        }
        // The staged audio and its recovery record are durable. Remove the
        // original before publication: audio.MP4 can name the same file as
        // audio.mp4 on a case-insensitive filesystem. After publication, never
        // remove an original filename that could now point at the new audio.
        remove_saved_path(&folder.join(&intent.original_filename))?;
        fs::rename(&staged, &target)?;
        sync_folder(folder)?;
    } else if hash_file(&target)? != intent.audio_hash {
        bail!("The trimmed audio is missing or changed. Trim recovery stopped.");
    }
    // These files can contain audio or text from outside the selected range.
    // New speaker work must use the trimmed mixed recording.
    for name in [
        ".diarization",
        ".checkpoints",
        "transcripts.json",
        ".transcripts.json.tmp",
    ] {
        remove_saved_path(&folder.join(name))?;
    }
    write_metadata(folder, intent)?;
    sync_folder(folder)
}

async fn finish_intent(core: &SqlitePool, feature: &SqlitePool, intent: &TrimIntent) -> Result<()> {
    let folder = meeting_folder(core, &intent.meeting_id).await?;
    if folder != Path::new(&intent.folder_path) {
        bail!("The meeting folder changed. Trim recovery stopped.");
    }
    let files = TrimIntent {
        meeting_id: intent.meeting_id.clone(),
        folder_path: intent.folder_path.clone(),
        original_filename: intent.original_filename.clone(),
        audio_hash: intent.audio_hash.clone(),
        duration_seconds: intent.duration_seconds,
    };
    tokio::task::spawn_blocking(move || finish_files(&files)).await??;
    let mut connection = durable_connection(core).await?;
    let mut transaction = connection.begin().await?;
    for table in ["transcript_chunks", "summary_processes", "transcripts"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE meeting_id = ?"))
            .bind(&intent.meeting_id)
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query("UPDATE meetings SET updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now())
        .bind(&intent.meeting_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    let mut connection = durable_connection(feature).await?;
    let mut transaction = connection.begin().await?;
    sqlx::query("DELETE FROM transcript_apply_rows WHERE run_id IN (SELECT id FROM diarization_runs WHERE meeting_id = ?)")
        .bind(&intent.meeting_id).execute(&mut *transaction).await?;
    for table in [
        "enhancement_publications",
        "diarization_runs",
        "transcript_speakers",
        "speaker_segments",
        "meeting_diarization_status",
        "recording_trims",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE meeting_id = ?"))
            .bind(&intent.meeting_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    PENDING
        .lock()
        .map_err(|_| anyhow!("Could not clear the saved trim state"))?
        .remove(&intent.meeting_id);
    Ok(())
}

pub(crate) async fn recover(core: &SqlitePool, feature: &SqlitePool) -> Result<()> {
    let intents: Vec<TrimIntent> = sqlx::query_as("SELECT * FROM recording_trims")
        .fetch_all(feature)
        .await?;
    for intent in intents {
        finish_intent(core, feature, &intent).await?;
    }
    Ok(())
}

/// The caller owns the meeting edit and retranscription guards. Before the
/// durable intent, failure/cancellation keeps the original. After it, recovery
/// always finishes the trim; transcription failure cannot restore removed text.
pub(crate) async fn trim_recording(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    range: TrimRange,
    cancelled: &'static AtomicBool,
) -> Result<PathBuf> {
    check_ready(meeting_id).map_err(|error| anyhow!(error))?;
    let folder = meeting_folder(core, meeting_id).await?;
    let prepare_folder = folder.clone();
    let (filename, audio_hash, duration) = tokio::task::spawn_blocking(move || -> Result<_> {
        let source = source_audio(&prepare_folder)?;
        let info = super::import::validate_audio_file(&source)?;
        range.validate(info.duration_seconds)?;
        // Check metadata before the destructive part of the operation.
        let metadata = prepare_folder.join("metadata.json");
        if metadata.exists() {
            if !fs::symlink_metadata(&metadata)?.file_type().is_file() {
                bail!("The recording metadata must be a regular file.");
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(metadata)?)?;
            if !value.is_object() {
                bail!("The recording metadata is not an object.");
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            bail!("Trim cancelled. The saved recording was kept.");
        }
        let staged = prepare_folder.join(STAGED_AUDIO);
        remove_saved_path(&staged)?;
        let ffmpeg =
            super::ffmpeg::find_ffmpeg_path().ok_or_else(|| anyhow!("FFmpeg is not available."))?;
        let result = (|| -> Result<_> {
            let output = Command::new(ffmpeg)
                .stdin(Stdio::null())
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
                .arg("-i")
                .arg(&source)
                .args(["-map", "0:a:0", "-vn", "-af"])
                .arg(format!(
                    "atrim=start={}:end={},asetpts=PTS-STARTPTS",
                    range.start_seconds, range.end_seconds
                ))
                .args([
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-map_metadata",
                    "-1",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&staged)
                .output()?;
            if !output.status.success() {
                bail!(
                    "Could not trim the recording: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let actual = super::import::validate_audio_file(&staged)?.duration_seconds;
            let expected = range.end_seconds - range.start_seconds;
            if !actual.is_finite() || actual <= 0.0 || (actual - expected).abs() > 0.1 {
                bail!("The trimmed audio has an invalid duration. The saved recording was kept.");
            }
            if cancelled.load(Ordering::SeqCst) {
                bail!("Trim cancelled. The saved recording was kept.");
            }
            // Windows requires a writable handle to flush file buffers.
            File::options().write(true).open(&staged)?.sync_all()?;
            sync_folder(&prepare_folder)?;
            Ok((
                source.file_name().unwrap().to_string_lossy().into_owned(),
                hash_file(&staged)?,
                actual,
            ))
        })();
        if result.is_err() {
            let _ = remove_saved_path(&staged);
        }
        result
    })
    .await??;
    let intent = TrimIntent {
        meeting_id: meeting_id.into(),
        folder_path: folder.to_string_lossy().into_owned(),
        original_filename: filename,
        audio_hash,
        duration_seconds: duration,
    };
    let mut connection = durable_connection(feature).await?;
    sqlx::query("INSERT INTO recording_trims (meeting_id, folder_path, original_filename, audio_hash, duration_seconds) VALUES (?, ?, ?, ?, ?)")
        .bind(meeting_id).bind(&intent.folder_path).bind(&intent.original_filename).bind(&intent.audio_hash).bind(duration)
        .execute(&mut connection).await?;
    PENDING
        .lock()
        .map_err(|_| anyhow!("Could not mark the saved trim"))?
        .insert(meeting_id.into());
    finish_intent(core, feature, &intent)
        .await
        .context("The trim is saved but could not finish. Restart Meetily Branch to finish it")?;
    Ok(folder)
}

#[tauri::command]
pub async fn get_recording_trim_info<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<f64, String> {
    check_ready(&meeting_id)?;
    let state = app.state::<crate::state::AppState>();
    let folder = meeting_folder(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || -> Result<f64> {
        Ok(super::import::validate_audio_file(&source_audio(&folder)?)?.duration_seconds)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);
    static CANCELLED: AtomicBool = AtomicBool::new(true);

    struct Fixture {
        _directory: tempfile::TempDir,
        folder: PathBuf,
        core: SqlitePool,
        feature: SqlitePool,
        id: String,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let folder = directory.path().join("meeting");
            fs::create_dir(&folder).unwrap();
            let folder = fs::canonicalize(folder).unwrap();
            let open = |name: &str| {
                SqliteConnectOptions::new()
                    .filename(directory.path().join(name))
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
            };
            let core = SqlitePoolOptions::new()
                .connect_with(open("core.sqlite"))
                .await
                .unwrap();
            let feature = SqlitePoolOptions::new()
                .connect_with(open("feature.sqlite"))
                .await
                .unwrap();
            sqlx::migrate!("./migrations").run(&core).await.unwrap();
            sqlx::migrate!("./speaker-migrations")
                .run(&feature)
                .await
                .unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, 'Keep this title', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, ?)")
                .bind(&id).bind(folder.to_str().unwrap()).execute(&core).await.unwrap();
            sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp) VALUES ('old', ?, 'Unwanted office talk', '00:00')")
                .bind(&id).execute(&core).await.unwrap();
            sqlx::query("INSERT INTO summary_processes (meeting_id, status, created_at, updated_at, result, result_backup) VALUES (?, 'completed', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, 'Old summary', 'Old backup')")
                .bind(&id).execute(&core).await.unwrap();
            sqlx::query("INSERT INTO transcript_chunks (meeting_id, transcript_text, model, model_name, created_at) VALUES (?, 'Old source', 'test', 'test', CURRENT_TIMESTAMP)")
                .bind(&id).execute(&core).await.unwrap();
            sqlx::query("INSERT INTO diarization_runs (id, meeting_id, status, method, transcript_backup) VALUES ('run', ?, 'applying', 'test', 'Old transcript backup')")
                .bind(&id).execute(&feature).await.unwrap();
            sqlx::query("INSERT INTO transcript_apply_rows (run_id, row_kind, ordinal, transcript_id, transcript_json) VALUES ('run', 'live', 0, 'old', 'Old text')")
                .execute(&feature).await.unwrap();
            sqlx::query("INSERT INTO transcript_speakers (transcript_id, meeting_id, speaker_id) VALUES ('old', ?, 'S1')")
                .bind(&id).execute(&feature).await.unwrap();
            sqlx::query("INSERT INTO speaker_segments (id, meeting_id, source, start_time, end_time, diarization_status) VALUES ('s1', ?, 'mixed', 0, 3, 'final')")
                .bind(&id).execute(&feature).await.unwrap();
            sqlx::query("INSERT INTO enhancement_publications (meeting_id, original_rows, proposed_rows, core_backup) VALUES (?, 'Old text', 'New text', 'Backup text')")
                .bind(&id).execute(&feature).await.unwrap();
            fs::write(folder.join("metadata.json"), br#"{"meeting_name":"Keep this title","audio_file":"original.wav","detected_summary_language":"en","summary_language":"fr"}"#).unwrap();
            for name in [".diarization", ".checkpoints"] {
                fs::create_dir(folder.join(name)).unwrap();
                fs::write(folder.join(name).join("retained-audio"), b"original audio").unwrap();
            }
            fs::write(folder.join("transcripts.json"), b"Old text").unwrap();
            fs::write(folder.join(".transcripts.json.tmp"), b"Old backup text").unwrap();
            Self {
                _directory: directory,
                folder,
                core,
                feature,
                id,
            }
        }

        fn audio(&self) {
            // Three distinct tones. Only the middle tone must remain.
            let ffmpeg = super::super::ffmpeg::find_ffmpeg_path()
                .expect("FFmpeg must be available for trim tests");
            let output = Command::new(ffmpeg).args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "aevalsrc=0.3*sin(2*PI*if(lt(t\\,1)\\,440\\,if(lt(t\\,2)\\,880\\,1320))*t):s=16000:d=3", "-y"])
                .arg(self.folder.join("original.wav")).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        async fn intent(&self) -> TrimIntent {
            fs::write(self.folder.join("original.wav"), b"original audio").unwrap();
            fs::write(self.folder.join(STAGED_AUDIO), b"trimmed audio").unwrap();
            let intent = TrimIntent {
                meeting_id: self.id.clone(),
                folder_path: self.folder.to_str().unwrap().into(),
                original_filename: "original.wav".into(),
                audio_hash: hash_file(&self.folder.join(STAGED_AUDIO)).unwrap(),
                duration_seconds: 1.0,
            };
            sqlx::query("INSERT INTO recording_trims VALUES (?, ?, ?, ?, ?)")
                .bind(&intent.meeting_id)
                .bind(&intent.folder_path)
                .bind(&intent.original_filename)
                .bind(&intent.audio_hash)
                .bind(intent.duration_seconds)
                .execute(&self.feature)
                .await
                .unwrap();
            intent
        }

        async fn assert_cleared(&self) {
            for table in ["transcripts", "summary_processes", "transcript_chunks"] {
                let count: i64 = sqlx::query_scalar(&format!(
                    "SELECT COUNT(*) FROM {table} WHERE meeting_id = ?"
                ))
                .bind(&self.id)
                .fetch_one(&self.core)
                .await
                .unwrap();
                assert_eq!(count, 0, "{table}");
            }
            for table in [
                "recording_trims",
                "enhancement_publications",
                "diarization_runs",
                "transcript_speakers",
                "speaker_segments",
                "meeting_diarization_status",
            ] {
                let count: i64 = sqlx::query_scalar(&format!(
                    "SELECT COUNT(*) FROM {table} WHERE meeting_id = ?"
                ))
                .bind(&self.id)
                .fetch_one(&self.feature)
                .await
                .unwrap();
                assert_eq!(count, 0, "{table}");
            }
            let apply: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transcript_apply_rows")
                .fetch_one(&self.feature)
                .await
                .unwrap();
            assert_eq!(apply, 0);
            for name in [
                "original.wav",
                STAGED_AUDIO,
                ".diarization",
                ".checkpoints",
                "transcripts.json",
                ".transcripts.json.tmp",
            ] {
                assert!(!self.folder.join(name).exists(), "{name} was retained");
            }
            let metadata: serde_json::Value =
                serde_json::from_slice(&fs::read(self.folder.join("metadata.json")).unwrap())
                    .unwrap();
            assert_eq!(metadata["audio_file"], "audio.mp4");
            assert_eq!(metadata["status"], "needs_transcription");
            assert_eq!(metadata["summary_language"], "fr");
            assert!(metadata.get("detected_summary_language").is_none());
            assert!(metadata.get("transcript_file").is_none());
            let title: String = sqlx::query_scalar("SELECT title FROM meetings WHERE id = ?")
                .bind(&self.id)
                .fetch_one(&self.core)
                .await
                .unwrap();
            assert_eq!(title, "Keep this title");
            assert!(check_ready(&self.id).is_ok());
        }
    }

    #[tokio::test]
    async fn trims_both_ends_and_removes_old_audio_text_and_recovery_data() {
        for format in ["wav", "mp4"] {
            let fixture = Fixture::new().await;
            fixture.audio();
            if format == "mp4" {
                let output = Command::new(super::super::ffmpeg::find_ffmpeg_path().unwrap())
                    .args(["-hide_banner", "-loglevel", "error", "-i"])
                    .arg(fixture.folder.join("original.wav"))
                    .args(["-c:a", "aac", "-y"])
                    .arg(fixture.folder.join("audio.mp4"))
                    .output()
                    .unwrap();
                assert!(output.status.success());
                fs::remove_file(fixture.folder.join("original.wav")).unwrap();
            }
            trim_recording(
                &fixture.core,
                &fixture.feature,
                &fixture.id,
                TrimRange {
                    start_seconds: 1.0,
                    end_seconds: 2.0,
                },
                &NOT_CANCELLED,
            )
            .await
            .unwrap();
            fixture.assert_cleared().await;
            let decoded =
                super::super::decoder::decode_audio_file(&fixture.folder.join("audio.mp4"))
                    .unwrap();
            assert!((decoded.duration_seconds - 1.0).abs() < 0.1);
            // Skip AAC boundary padding. Neither the 440 Hz start nor the 1320 Hz
            // end may remain in either half of the retained audio.
            let rate = decoded.sample_rate as usize;
            for fraction in [0.1, 0.6] {
                let start = (fraction * rate as f64) as usize;
                let samples = &decoded.samples[start..start + rate / 4];
                let crossings = samples
                    .windows(2)
                    .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
                    .count();
                let frequency = crossings as f64 * 4.0;
                assert!(
                    (frequency - 880.0).abs() < 25.0,
                    "retained frequency: {frequency}"
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_or_unchanged_ranges() {
        for (start_seconds, end_seconds) in [
            (f64::NAN, 2.0),
            (0.0, f64::INFINITY),
            (-1.0, 2.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (0.0, 3.1),
            (0.0, 3.0),
        ] {
            assert!(TrimRange {
                start_seconds,
                end_seconds
            }
            .validate(3.0)
            .is_err());
        }
        assert!(TrimRange {
            start_seconds: 0.5,
            end_seconds: 2.5
        }
        .validate(3.0)
        .is_ok());
    }

    #[test]
    fn summaries_can_run_together_but_cannot_overlap_a_trim_of_the_same_meeting() {
        let first = try_summary("trim-summary-lock").unwrap();
        let second = try_summary("trim-summary-lock").unwrap();
        assert!(try_trim("trim-summary-lock").is_err());
        assert!(try_trim("other-trim-summary-lock").is_ok());
        drop(first);
        assert!(try_trim("trim-summary-lock").is_err());
        drop(second);
        let trim = try_trim("trim-summary-lock").unwrap();
        assert!(try_summary("trim-summary-lock").is_err());
        drop(trim);
        assert!(try_summary("trim-summary-lock").is_ok());
    }

    #[tokio::test]
    async fn cancel_and_invalid_range_keep_the_original_files_and_transcript() {
        let fixture = Fixture::new().await;
        fixture.audio();
        let before = hash_file(&fixture.folder.join("original.wav")).unwrap();
        for (range, cancelled) in [
            (
                TrimRange {
                    start_seconds: 0.0,
                    end_seconds: 4.0,
                },
                &NOT_CANCELLED,
            ),
            (
                TrimRange {
                    start_seconds: 1.0,
                    end_seconds: 2.0,
                },
                &CANCELLED,
            ),
        ] {
            assert!(trim_recording(
                &fixture.core,
                &fixture.feature,
                &fixture.id,
                range,
                cancelled
            )
            .await
            .is_err());
            assert_eq!(
                hash_file(&fixture.folder.join("original.wav")).unwrap(),
                before
            );
            assert!(fixture.folder.join("transcripts.json").exists());
            assert!(!fixture.folder.join(STAGED_AUDIO).exists());
            let text: String =
                sqlx::query_scalar("SELECT transcript FROM transcripts WHERE meeting_id = ?")
                    .bind(&fixture.id)
                    .fetch_one(&fixture.core)
                    .await
                    .unwrap();
            assert_eq!(text, "Unwanted office talk");
        }
    }

    #[tokio::test]
    async fn startup_finishes_each_interrupted_file_transition_before_speaker_recovery() {
        for step in 0..4 {
            let fixture = Fixture::new().await;
            let intent = fixture.intent().await;
            if step == 1 || step == 2 {
                fs::remove_file(fixture.folder.join("original.wav")).unwrap();
            }
            if step == 2 {
                fs::rename(
                    fixture.folder.join(STAGED_AUDIO),
                    fixture.folder.join("audio.mp4"),
                )
                .unwrap();
            } else if step == 3 {
                finish_files(&intent).unwrap();
            }
            crate::diarization::finalize::reconcile_interrupted_runs(
                &fixture.core,
                &fixture.feature,
            )
            .await
            .unwrap();
            fixture.assert_cleared().await;
            assert_eq!(
                fs::read(fixture.folder.join("audio.mp4")).unwrap(),
                b"trimmed audio"
            );
        }
    }

    #[tokio::test]
    async fn database_failure_keeps_intent_and_blocks_edits_until_recovery_finishes() {
        let fixture = Fixture::new().await;
        fixture.audio();
        sqlx::raw_sql("CREATE TRIGGER fail_trim BEFORE DELETE ON recording_trims BEGIN SELECT RAISE(FAIL, 'test disk failure'); END;")
            .execute(&fixture.feature).await.unwrap();
        let error = trim_recording(
            &fixture.core,
            &fixture.feature,
            &fixture.id,
            TrimRange {
                start_seconds: 1.0,
                end_seconds: 2.0,
            },
            &NOT_CANCELLED,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Restart Meetily Branch"));
        assert!(crate::diarization::operations::try_meeting_edit(&fixture.id).is_err());
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recording_trims")
            .fetch_one(&fixture.feature)
            .await
            .unwrap();
        assert_eq!(pending, 1);
        sqlx::raw_sql("DROP TRIGGER fail_trim")
            .execute(&fixture.feature)
            .await
            .unwrap();
        recover(&fixture.core, &fixture.feature).await.unwrap();
        fixture.assert_cleared().await;
    }

    #[tokio::test]
    async fn recovery_does_not_mistake_original_audio_for_a_missing_trim() {
        let fixture = Fixture::new().await;
        fixture.intent().await;
        fs::rename(
            fixture.folder.join("original.wav"),
            fixture.folder.join("audio.mp4"),
        )
        .unwrap();
        fs::remove_file(fixture.folder.join(STAGED_AUDIO)).unwrap();
        assert!(recover(&fixture.core, &fixture.feature).await.is_err());
        assert_eq!(
            fs::read(fixture.folder.join("audio.mp4")).unwrap(),
            b"original audio"
        );
        assert!(fixture.folder.join("transcripts.json").exists());
    }

    #[tokio::test]
    async fn uppercase_import_extension_does_not_remove_the_trimmed_output() {
        let fixture = Fixture::new().await;
        fixture.intent().await;
        fs::rename(
            fixture.folder.join("original.wav"),
            fixture.folder.join("audio.MP4"),
        )
        .unwrap();
        sqlx::query(
            "UPDATE recording_trims SET original_filename = 'audio.MP4' WHERE meeting_id = ?",
        )
        .bind(&fixture.id)
        .execute(&fixture.feature)
        .await
        .unwrap();
        recover(&fixture.core, &fixture.feature).await.unwrap();
        assert_eq!(
            fs::read(fixture.folder.join("audio.mp4")).unwrap(),
            b"trimmed audio"
        );
        let media: Vec<_> = fs::read_dir(&fixture.folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
            })
            .collect();
        assert_eq!(media.len(), 1);
        // Recovery after the audio rename must not remove the output either.
        recover(&fixture.core, &fixture.feature).await.unwrap();
        fixture.assert_cleared().await;
        assert_eq!(
            fs::read(fixture.folder.join("audio.mp4")).unwrap(),
            b"trimmed audio"
        );
    }

    #[tokio::test]
    async fn changed_staged_audio_is_rejected_before_the_original_is_removed() {
        let fixture = Fixture::new().await;
        fixture.intent().await;
        fs::write(fixture.folder.join(STAGED_AUDIO), b"unexpected contents").unwrap();
        assert!(recover(&fixture.core, &fixture.feature).await.is_err());
        assert_eq!(
            fs::read(fixture.folder.join("original.wav")).unwrap(),
            b"original audio"
        );
        assert!(fixture.folder.join("transcripts.json").exists());
    }

    #[tokio::test]
    async fn refuses_shared_folders_extra_audio_and_paths_outside_the_meeting() {
        let fixture = Fixture::new().await;
        fixture.audio();
        fs::write(fixture.folder.join("extra.wav"), b"another recording").unwrap();
        assert!(source_audio(&fixture.folder)
            .unwrap_err()
            .to_string()
            .contains("more than one"));
        fs::remove_file(fixture.folder.join("extra.wav")).unwrap();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES ('other', 'Other', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, ?)")
            .bind(fixture.folder.to_str().unwrap()).execute(&fixture.core).await.unwrap();
        assert!(meeting_folder(&fixture.core, &fixture.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("Another meeting"));
        let mut intent = fixture.intent().await;
        intent.original_filename = "../outside.wav".into();
        assert!(finish_files(&intent).is_err());
        assert!(fixture.folder.join(STAGED_AUDIO).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn links_do_not_read_or_remove_audio_outside_the_meeting() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new().await;
        let outside = fixture._directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("audio.wav"), b"external audio").unwrap();
        symlink(
            outside.join("audio.wav"),
            fixture.folder.join("original.wav"),
        )
        .unwrap();
        assert!(source_audio(&fixture.folder).is_err());
        fs::remove_file(fixture.folder.join("original.wav")).unwrap();
        fixture.intent().await;
        fs::remove_dir_all(fixture.folder.join(".diarization")).unwrap();
        symlink(&outside, fixture.folder.join(".diarization")).unwrap();
        recover(&fixture.core, &fixture.feature).await.unwrap();
        assert_eq!(
            fs::read(outside.join("audio.wav")).unwrap(),
            b"external audio"
        );
        assert!(!fixture.folder.join(".diarization").exists());
    }
}
