//! Managed, offline MLX worker for the full Nemotron 3.5 Transcribe model.
use crate::audio::transcription::{TranscriptResult, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
#[path = "nemotron_artifacts.rs"]
mod artifacts;
use artifacts::ARTIFACTS;

pub const MODEL: &str = "nemotron-3.5-transcribe-30b-a4b-mlx-4bit";
const REPO: &str = "nativ-community/Nemotron-3.5-Transcribe-30B-A4B-EA-4bit";
const REVISION: &str = "c47153ca2ce4024d43e216b81e6db4f67b886545";
const UV_URL: &str =
    "https://github.com/astral-sh/uv/releases/download/0.11.18/uv-aarch64-apple-darwin.tar.gz";
const UV_SHA: &str = "1a7adf8dadae3b55853115d13a8bf564d219597ad13824b93b213706933863e5";
const MLX_URL: &str =
    "https://github.com/Blaizzy/mlx-audio/archive/92decb7577878ef8e9274c1a7e33084760be03aa.tar.gz";
const MLX_SHA: &str = "e8d717772d94b2bfdc946606511361804da52eb824e42269dfdc9f76dbb02ea2";
const RUNTIME_VERSION: &str = "mlx-audio-92decb7;mlx-0.32.4;python-3.12.12";
const CONSTRAINTS: &str = include_str!("../../../nemotron-helper/constraints.txt");
const WORKER: &str = include_str!("../../../nemotron-helper/main.py");
const DOWNLOADER: &str = include_str!("../../../nemotron-helper/download.py");
static BUSY: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static CANCEL: AtomicBool = AtomicBool::new(false);
static INSTALL_LOCK: Mutex<()> = Mutex::const_new(());

fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}
fn root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(crate::model_storage::owned_models_root(app)?.join("nemotron"))
}
fn runtime_ready(path: &Path) -> bool {
    std::fs::read_to_string(path.join("runtime-ready"))
        .map(|value| value == RUNTIME_VERSION)
        .unwrap_or(false)
}
fn ready(path: &Path) -> bool {
    runtime_ready(path)
        && path.join("venv/bin/python").is_file()
        && ARTIFACTS.iter().all(|(name, size, _)| {
            path.join("model")
                .join(name)
                .metadata()
                .map(|m| m.len() == *size)
                .unwrap_or(false)
        })
}
pub fn validate_install<R: Runtime>(app: &AppHandle<R>, model: &str) -> Result<(), String> {
    if !supported() {
        return Err("Nemotron Transcribe requires macOS on Apple silicon.".into());
    }
    if model != MODEL {
        return Err("Unknown Nemotron model.".into());
    }
    if !ready(&root(app)?) {
        return Err("Download Nemotron and its runtime in Settings first.".into());
    }
    Ok(())
}
#[tauri::command]
pub async fn nemotron_init() -> Result<(), String> {
    Ok(())
}
#[tauri::command]
pub async fn nemotron_get_available_models<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<Value>, String> {
    let path = root(&app)?;
    let status = if BUSY.load(Ordering::SeqCst) {
        json!({"Downloading":{"progress":0}})
    } else if ready(&path) && supported() {
        json!("Available")
    } else {
        json!("Missing")
    };
    Ok(vec![
        json!({"name":MODEL, "display_name":"Nemotron 3.5 Transcribe · MLX 4-bit", "size_mb":ARTIFACTS.iter().map(|(_,s,_)|s).sum::<u64>() / 1_000_000,
        "supported":supported(), "status":status}),
    ])
}
#[tauri::command]
pub async fn nemotron_has_available_models<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    Ok(supported() && ready(&root(&app)?))
}
fn cancelled() -> Result<(), String> {
    if CANCEL.load(Ordering::SeqCst) {
        Err("Download cancelled.".into())
    } else {
        Ok(())
    }
}
fn progress<R: Runtime>(app: &AppHandle<R>, phase: &str, percent: u64) {
    let _ = app.emit(
        "nemotron-download-progress",
        json!({"phase":phase,"percent":percent}),
    );
}
async fn digest(path: &Path) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        cancelled()?;
        let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Fetches the small uv installer. uv and huggingface_hub fetch everything else.
async fn download(url: &str, dest: &Path, sha: &str) -> Result<(), String> {
    cancelled()?;
    if dest.is_file() && digest(dest).await? == sha {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = client
        .get(url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&bytes)) != sha {
        return Err("Download verification failed. Try the download again.".into());
    }
    tokio::fs::write(dest, bytes)
        .await
        .map_err(|e| e.to_string())
}
/// Partial files that huggingface_hub leaves when its process is stopped.
fn partial_downloads(model: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(model.join(".cache/huggingface/download"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "incomplete"))
        .collect()
}
fn remove_partial_downloads(model: &Path) {
    for path in partial_downloads(model) {
        let _ = std::fs::remove_file(path);
    }
}
fn downloaded_bytes(model: &Path) -> u64 {
    let file_len = |path: &Path| path.metadata().map(|m| m.len()).unwrap_or(0);
    let complete: u64 = ARTIFACTS
        .iter()
        .map(|(name, _, _)| file_len(&model.join(name)))
        .sum();
    complete
        + partial_downloads(model)
            .iter()
            .map(|path| file_len(path))
            .sum::<u64>()
}
fn last_log_line(path: &Path) -> String {
    let log = std::fs::read_to_string(path).unwrap_or_default();
    let line = log.lines().rev().find(|line| !line.trim().is_empty());
    line.unwrap_or("no details")
        .trim()
        .chars()
        .take(300)
        .collect()
}
/// Downloads the model files with huggingface_hub, which retries and resumes
/// within one run. A new run starts unfinished files again.
async fn download_model(path: &Path, on_progress: impl Fn(u64)) -> Result<(), String> {
    let model = path.join("model");
    // A stopped run's partial files cannot be reused, so they only take space.
    remove_partial_downloads(&model);
    let log_path = path.join("download.log");
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut child = Command::new(path.join("venv/bin/python"))
        .args(["-I", "-c", DOWNLOADER, REPO, REVISION])
        .arg(&model)
        .args(ARTIFACTS.iter().map(|(name, _, _)| name))
        .env("HF_HOME", path.join("huggingface"))
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("HF_HUB_DISABLE_PROGRESS_BARS", "1")
        .env_remove("HF_HUB_OFFLINE")
        .env_remove("HF_ENDPOINT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|e| e.to_string())?,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if CANCEL.load(Ordering::SeqCst) {
                    let _ = child.kill().await;
                    remove_partial_downloads(&model);
                    return Err("Download cancelled.".into());
                }
                on_progress(downloaded_bytes(&model));
            }
        }
    };
    if !status.success() {
        remove_partial_downloads(&model);
        return Err(format!(
            "Model download failed: {}. See nemotron/download.log.",
            last_log_line(&log_path)
        ));
    }
    Ok(())
}
/// Checks each model file against its pinned SHA-256 and removes a bad file
/// so the next download fetches it again.
async fn verify_model(model: &Path, on_progress: impl Fn(u64)) -> Result<(), String> {
    let mut checked = 0;
    for (name, size, sha) in ARTIFACTS {
        let file = model.join(name);
        if digest(&file).await.map_err(|e| format!("{name}: {e}"))? != *sha {
            let _ = tokio::fs::remove_file(&file).await;
            return Err(format!(
                "{name} failed verification. Try the download again."
            ));
        }
        checked += size;
        on_progress(checked);
    }
    Ok(())
}
async fn run(command: &mut Command) -> Result<(), String> {
    cancelled()?;
    let status = command
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    cancelled()?;
    if !status.success() {
        return Err(format!(
            "Runtime setup failed ({status}). See nemotron/runtime.log."
        ));
    }
    Ok(())
}
async fn install<R: Runtime>(app: &AppHandle<R>, path: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(path.join("model"))
        .await
        .map_err(|e| e.to_string())?;
    let total = ARTIFACTS.iter().map(|(_, s, _)| s).sum::<u64>();
    if !runtime_ready(path) {
        progress(app, "Downloading runtime installer", 0);
        download(UV_URL, &path.join("uv.tar.gz"), UV_SHA).await?;
        run(Command::new("/usr/bin/tar")
            .args(["-xzf"])
            .arg(path.join("uv.tar.gz"))
            .arg("-C")
            .arg(path))
        .await?;
        // uv manages its own Python. No system Python or package state is changed.
        let uv = path.join("uv-aarch64-apple-darwin/uv");
        let log = std::fs::File::create(path.join("runtime.log")).map_err(|e| e.to_string())?;
        progress(
            app,
            "Installing Python and MLX (cancel takes effect after this step)",
            0,
        );
        run(Command::new(&uv)
            .arg("--no-config")
            .env("UV_CACHE_DIR", path.join("cache"))
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .args(["venv", "--python", "3.12.12", "--managed-python", "--clear"])
            .arg(path.join("venv"))
            .env("UV_PYTHON_INSTALL_DIR", path.join("python"))
            .stdout(Stdio::null())
            .stderr(log.try_clone().map_err(|e| e.to_string())?))
        .await?;
        tokio::fs::write(path.join("constraints.txt"), CONSTRAINTS)
            .await
            .map_err(|e| e.to_string())?;
        // uv fetches the MLX source archive and checks its pinned hash.
        tokio::fs::write(
            path.join("requirements.txt"),
            format!("mlx-audio[stt] @ {MLX_URL} --hash=sha256:{MLX_SHA}\n"),
        )
        .await
        .map_err(|e| e.to_string())?;
        // uv 0.11 cannot open a --constraint path that contains a space, and the
        // macOS app data path does ("Application Support"). Pass both files
        // relative to the install folder.
        run(Command::new(&uv)
            .current_dir(path)
            .arg("--no-config")
            .env("UV_CACHE_DIR", path.join("cache"))
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .args(["pip", "install", "--constraint", "constraints.txt"])
            .arg("--python")
            .arg(path.join("venv/bin/python"))
            .args(["--requirement", "requirements.txt"])
            .arg("mlx==0.32.4")
            .stdout(Stdio::null())
            .stderr(log))
        .await?;
        run(Command::new(path.join("venv/bin/python"))
            .args(["-c", "from mlx_audio.stt import load; import numpy"])
            .stdout(Stdio::null()))
        .await?;
        tokio::fs::write(path.join("runtime-ready"), RUNTIME_VERSION.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    let percent = |bytes: u64| (bytes * 100 / total.max(1)).min(99);
    progress(app, "Downloading model", 0);
    download_model(path, |bytes| {
        progress(app, "Downloading model", percent(bytes))
    })
    .await?;
    verify_model(&path.join("model"), |bytes| {
        progress(app, "Checking model files", percent(bytes))
    })
    .await?;
    progress(app, "Download complete", 100);
    Ok(())
}
struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}
#[tauri::command]
pub async fn nemotron_download_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if !supported() {
        return Err("Nemotron Transcribe requires macOS on Apple silicon.".into());
    }
    if BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A Nemotron download is already running.".into());
    }
    let busy = BusyGuard;
    let _lock = INSTALL_LOCK.lock().await;
    // Bound after the lock so BUSY clears before a waiting delete can take the lock.
    let _busy = busy;
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Stop transcription before downloading Nemotron.".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    let result = install(&app, &root(&app)?).await;
    let _ = app.emit(
        "nemotron-download-finished",
        json!({"error":result.as_ref().err()}),
    );
    result
}
#[tauri::command]
pub async fn nemotron_cancel_download() {
    CANCEL.store(true, Ordering::SeqCst);
}
#[tauri::command]
pub async fn nemotron_delete_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().await;
    if BUSY.load(Ordering::SeqCst) {
        return Err("Wait for the download to stop.".into());
    }
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Stop transcription before deleting Nemotron.".into());
    }
    let path = root(&app)?;
    if path.exists() {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Why a segment failed, and whether its worker can take the next segment.
enum RequestError {
    /// The worker rejected this segment and is waiting for the next request.
    Segment(String),
    /// The worker stopped or lost protocol sync. It must be replaced.
    Worker(String),
}
struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
impl Worker {
    /// Reads one protocol message. Any error leaves the worker unusable.
    async fn read(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(600), self.output.read_line(&mut line))
            .await
            .map_err(|_| "Nemotron request timed out.")?
            .map_err(|e| e.to_string())?;
        if n == 0 || line.len() > 4 * 1024 * 1024 {
            return Err("Nemotron worker stopped or sent an invalid reply.".into());
        }
        serde_json::from_str(&line).map_err(|e| e.to_string())
    }
    async fn transcribe(&mut self, audio: &[f32]) -> Result<String, RequestError> {
        let bytes: Vec<u8> = audio.iter().flat_map(|v| v.to_le_bytes()).collect();
        let request = json!({"audio":STANDARD.encode(bytes)}).to_string() + "\n";
        self.input
            .write_all(request.as_bytes())
            .await
            .map_err(|e| RequestError::Worker(e.to_string()))?;
        self.input
            .flush()
            .await
            .map_err(|e| RequestError::Worker(e.to_string()))?;
        let reply = self.read().await.map_err(RequestError::Worker)?;
        if let Some(error) = reply.get("error") {
            return Err(RequestError::Segment(format!("Nemotron: {error}")));
        }
        reply
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| RequestError::Worker("Nemotron reply has no transcript.".into()))
    }
    async fn spawn(path: &Path) -> Result<Self, String> {
        let log = std::fs::File::create(path.join("worker.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(path.join("venv/bin/python"))
            .args(["-u", "-c", WORKER])
            .arg(path.join("model"))
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().ok_or("Nemotron input is unavailable.")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or("Nemotron output is unavailable.")?,
        );
        let mut worker = Self {
            child,
            input,
            output,
        };
        let ready = worker.read().await?;
        if let Some(error) = ready.get("error") {
            return Err(format!("Nemotron: {error}"));
        }
        if ready.get("ready") != Some(&json!(true)) {
            return Err("Nemotron did not start.".into());
        }
        Ok(worker)
    }
}
pub struct NemotronProvider {
    path: PathBuf,
    /// Empty after a worker failure. The next segment starts a new worker.
    worker: Mutex<Option<Worker>>,
}
impl NemotronProvider {
    /// Counts as active until dropped, which blocks download and delete.
    fn new(path: PathBuf, worker: Option<Worker>) -> Self {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self {
            path,
            worker: Mutex::new(worker),
        }
    }
}
impl Drop for NemotronProvider {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}
pub async fn configured_provider<R: Runtime>(
    app: &AppHandle<R>,
    requested: Option<&str>,
) -> Result<NemotronProvider, String> {
    let selected = match requested {
        Some(s) => s.to_string(),
        None => {
            let config = crate::api::api::api_get_transcript_config(app.clone(), app.state(), None)
                .await?
                .ok_or("Select a transcription model.")?;
            if config.provider != "nemotron" {
                return Err("The saved model is not Nemotron.".into());
            }
            config.model
        }
    };
    validate_install(app, &selected)?;
    let _lock = INSTALL_LOCK.lock().await;
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Nemotron is in use. Stop the current transcription first.".into());
    }
    let path = root(app)?;
    let worker = Worker::spawn(&path).await?;
    Ok(NemotronProvider::new(path, Some(worker)))
}
#[async_trait]
impl TranscriptionProvider for NemotronProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        _language: Option<String>,
    ) -> Result<TranscriptResult, TranscriptionError> {
        if audio.len() < 1600 {
            return Err(TranscriptionError::AudioTooShort {
                samples: audio.len(),
                minimum: 1600,
            });
        }
        if audio.iter().any(|s| !s.is_finite()) {
            return Err(TranscriptionError::EngineFailed(
                "Audio contains invalid samples.".into(),
            ));
        }
        let mut guard = self.worker.lock().await;
        let mut text = Vec::new();
        // The full model is offline. Each request is a complete audio segment.
        for range in
            crate::audio::transcription::funasr_provider::request_ranges(&audio, 30 * 16000)
        {
            let piece = &audio[range];
            if piece.len() < 1600 {
                return Err(TranscriptionError::EngineFailed(
                    "Audio split is too short.".into(),
                ));
            }
            // Taken out so a failed or cancelled request drops, and stops, its worker.
            let mut worker = match guard.take() {
                Some(worker) => worker,
                None => {
                    log::warn!("Restarting the Nemotron worker after a failure");
                    Worker::spawn(&self.path)
                        .await
                        .map_err(TranscriptionError::EngineFailed)?
                }
            };
            match worker.transcribe(piece).await {
                Ok(part) => {
                    *guard = Some(worker);
                    text.push(part);
                }
                Err(RequestError::Segment(error)) => {
                    *guard = Some(worker);
                    return Err(TranscriptionError::EngineFailed(error));
                }
                Err(RequestError::Worker(error)) => {
                    return Err(TranscriptionError::EngineFailed(error));
                }
            }
        }
        Ok(TranscriptResult {
            text: text.join(" "),
            confidence: None,
            is_partial: false,
        })
    }
    async fn is_model_loaded(&self) -> bool {
        // The install cannot change while a provider is active, and a stopped worker restarts on use.
        true
    }
    async fn get_current_model(&self) -> Option<String> {
        Some(MODEL.to_string())
    }
    fn provider_name(&self) -> &'static str {
        "Nemotron"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_install_is_not_available() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("runtime-ready"), "ready").unwrap();
        assert!(!ready(dir.path()));
    }
    #[test]
    fn incomplete_or_changed_model_files_are_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("venv/bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("model")).unwrap();
        std::fs::write(dir.path().join("venv/bin/python"), "").unwrap();
        std::fs::write(dir.path().join("runtime-ready"), RUNTIME_VERSION).unwrap();
        // Sparse test files check the catalog without downloading model weights.
        for (name, size, _) in ARTIFACTS {
            std::fs::File::create(dir.path().join("model").join(name))
                .unwrap()
                .set_len(*size)
                .unwrap();
        }
        assert!(ready(dir.path()));
        std::fs::write(dir.path().join("model/config.json"), "incomplete").unwrap();
        assert!(!ready(dir.path()));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_model_download_reports_the_error_and_removes_partial_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("venv/bin/python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        // Stands in for huggingface_hub: leaves a partial file, then fails.
        std::fs::write(
            &python,
            r#"#!/bin/sh
echo "$4 $5" > "$(dirname "$0")/args"
mkdir -p "$6/.cache/huggingface/download"
echo partial > "$6/.cache/huggingface/download/shard.1234.incomplete"
echo "Traceback (most recent call last):" >&2
echo "httpx2.ConnectError: network is unreachable" >&2
exit 1
"#,
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = download_model(dir.path(), |_| {}).await.unwrap_err();
        assert!(error.contains("network is unreachable"), "{error}");
        assert!(partial_downloads(&dir.path().join("model")).is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("venv/bin/args")).unwrap(),
            format!("{REPO} {REVISION}\n")
        );
    }
    #[tokio::test]
    async fn verification_removes_a_changed_model_file() {
        let dir = tempfile::tempdir().unwrap();
        let (name, _, _) = ARTIFACTS[0];
        std::fs::write(dir.path().join(name), "changed").unwrap();
        let error = verify_model(dir.path(), |_| {}).await.unwrap_err();
        assert!(error.contains("failed verification"), "{error}");
        assert!(!dir.path().join(name).exists());
    }
    #[test]
    fn progress_counts_complete_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join(".cache/huggingface/download");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(dir.path().join(ARTIFACTS[0].0), [0; 10]).unwrap();
        std::fs::write(partial.join("shard.ab12.incomplete"), [0; 5]).unwrap();
        std::fs::write(partial.join("shard.metadata"), [0; 100]).unwrap();
        assert_eq!(downloaded_bytes(dir.path()), 15);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn segment_errors_keep_the_worker_and_stopped_workers_restart() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("venv/bin/python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        // Stands in for the helper: rejects request 1, exits on request 2, then transcribes.
        std::fs::write(
            &python,
            r#"#!/bin/sh
state=$(dirname "$0")
echo start >> "$state/spawns"
echo '{"ready":true}'
while IFS= read -r line; do
  echo request >> "$state/requests"
  case $(wc -l < "$state/requests" | tr -d ' ') in
    1) echo '{"error":"The transcript reached the output limit"}' ;;
    2) exit 1 ;;
    *) echo '{"text":"hello"}' ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spawns = || {
            std::fs::read_to_string(dir.path().join("venv/bin/spawns"))
                .unwrap()
                .lines()
                .count()
        };
        let worker = Worker::spawn(dir.path()).await.unwrap();
        let provider = NemotronProvider::new(dir.path().to_path_buf(), Some(worker));
        let audio = vec![0.0; 1600];

        let rejected = provider.transcribe(audio.clone(), None).await.unwrap_err();
        assert!(rejected.to_string().contains("output limit"));
        assert!(provider.worker.lock().await.is_some());

        assert!(provider.transcribe(audio.clone(), None).await.is_err());
        assert!(provider.worker.lock().await.is_none());
        assert!(provider.is_model_loaded().await);
        assert_eq!(spawns(), 1);

        let result = provider.transcribe(audio, None).await.unwrap();
        assert_eq!(result.text, "hello");
        assert_eq!(spawns(), 2);
    }
    #[test]
    fn catalog_has_pinned_hashes_and_complete_shards() {
        assert_eq!(
            ARTIFACTS
                .iter()
                .filter(|(n, _, _)| n.ends_with(".safetensors"))
                .count(),
            4
        );
        for (_, size, sha) in ARTIFACTS {
            assert!(*size > 0);
            assert_eq!(sha.len(), 64);
            assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
