//! Offline Nemotron 3.5 Transcribe through NVIDIA's NeMo-Speech.cpp server.
//!
//! Meetily downloads NVIDIA's native runtime and a GGUF decoder with its shared
//! encoder, then runs `nemo-speech serve` on a loopback port with a random API
//! key. The server returns word times and speaker labels, so Enhance can send
//! whole recordings and still split rows where the speaker changes.
use crate::audio::transcription::{TranscriptResult, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
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
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::Mutex,
};
#[path = "nemotron_artifacts.rs"]
mod artifacts;
use artifacts::{
    Artifact, Decoder, DECODERS, ENCODER, REPO, REVISION, RUNTIME, RUNTIME_DIR, RUNTIME_URL,
    RUNTIME_VERSION,
};

/// The model being downloaded, if any.
static DOWNLOADING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static CANCEL: AtomicBool = AtomicBool::new(false);
static INSTALL_LOCK: Mutex<()> = Mutex::const_new(());

/// A connection that sends nothing for this long has failed.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Nemotron writes text nobody said on long requests. On a 17 minute meeting,
/// one 5 minute piece came back as an invented talk about database sharding,
/// and with a language hint another became a Python tutorial. Its alignment
/// still reported success. Every 1 minute piece of that meeting and another
/// matched the saved transcript.
const MAX_TEXT_SAMPLES: usize = 60 * 16000;
const RETRY_TEXT_SAMPLES: usize = 20 * 16000;
/// Speaker turns cover up to one hour per request, the server's limit, so
/// speaker numbers stay the same across a meeting.
const MAX_SPEAKER_SAMPLES: usize = 3600 * 16000;
const MIN_SAMPLES: usize = 1600;
/// Model loading reads up to 21 GB from disk.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}
fn root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(crate::model_storage::owned_models_root(app)?.join("nemotron"))
}
fn decoder(id: &str) -> Option<&'static Decoder> {
    DECODERS.iter().find(|decoder| decoder.id == id)
}
fn binary(path: &Path) -> PathBuf {
    path.join("runtime/bin/nemo-speech")
}
fn runtime_ready(path: &Path) -> bool {
    std::fs::read_to_string(path.join("runtime/.version"))
        .map(|version| version == RUNTIME_VERSION)
        .unwrap_or(false)
        && binary(path).is_file()
}
fn gguf(path: &Path, artifact: &Artifact) -> PathBuf {
    path.join("gguf").join(artifact.name)
}
/// Files are only renamed into place after their SHA-256 matched.
fn file_ready(path: &Path, artifact: &Artifact) -> bool {
    gguf(path, artifact)
        .metadata()
        .map(|m| m.len() == artifact.size)
        .unwrap_or(false)
}
fn ready(path: &Path, decoder: &Decoder) -> bool {
    runtime_ready(path) && file_ready(path, &ENCODER) && file_ready(path, &decoder.file)
}
pub fn validate_install<R: Runtime>(app: &AppHandle<R>, model: &str) -> Result<(), String> {
    if !supported() {
        return Err("Nemotron Transcribe requires macOS on Apple silicon.".into());
    }
    let decoder = decoder(model).ok_or("Select a Nemotron model in Settings.")?;
    if !ready(&root(app)?, decoder) {
        return Err("Download Nemotron in Settings first.".into());
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
    let downloading = DOWNLOADING.lock().unwrap().clone();
    Ok(DECODERS
        .iter()
        .map(|decoder| {
            let status = if downloading.as_deref() == Some(decoder.id) {
                json!({"Downloading":{"progress":0}})
            } else if supported() && ready(&path, decoder) {
                json!("Available")
            } else {
                json!("Missing")
            };
            json!({
                "name": decoder.id,
                "display_name": decoder.display_name,
                "size_mb": (decoder.file.size + ENCODER.size) / 1_000_000,
                "memory": decoder.memory,
                "supported": supported(),
                "status": status,
            })
        })
        .collect())
}
#[tauri::command]
pub async fn nemotron_has_available_models<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    let path = root(&app)?;
    Ok(supported() && DECODERS.iter().any(|decoder| ready(&path, decoder)))
}
fn cancelled() -> Result<(), String> {
    if CANCEL.load(Ordering::SeqCst) {
        Err("Download cancelled.".into())
    } else {
        Ok(())
    }
}
fn progress<R: Runtime>(app: &AppHandle<R>, model: &str, phase: &str, percent: u64) {
    let _ = app.emit(
        "nemotron-download-progress",
        json!({"model":model,"phase":phase,"percent":percent}),
    );
}
fn partial(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}
/// Downloads one file to `dest`. An unfinished `.part` file is resumed with an
/// HTTP range request. The file is hashed as it arrives and is only renamed
/// into place when its size and SHA-256 match.
async fn fetch(
    url: &str,
    dest: &Path,
    artifact: &Artifact,
    on_progress: impl Fn(u64),
) -> Result<(), String> {
    cancelled()?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let part = partial(dest);
    let mut have = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if have > artifact.size {
        let _ = tokio::fs::remove_file(&part).await;
        have = 0;
    }
    let mut hash = Sha256::new();
    if have > 0 {
        let mut file = tokio::fs::File::open(&part)
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0; 1024 * 1024];
        loop {
            cancelled()?;
            let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
    }
    if have < artifact.size {
        let client = reqwest::Client::builder()
            .connect_timeout(STALL_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        let mut request = client.get(url);
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let response = tokio::time::timeout(STALL_TIMEOUT, request.send())
            .await
            .map_err(|_| "The download server stopped responding.".to_string())?
            .and_then(|response| response.error_for_status())
            .map_err(|e| e.to_string())?;
        if have > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            // The server sent the whole file, so start again.
            have = 0;
            hash = Sha256::new();
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(have > 0)
            .truncate(have == 0)
            .open(&part)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = response.bytes_stream();
        loop {
            cancelled()?;
            let chunk = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Err(_) => return Err("The download stalled. Try again to resume it.".into()),
                Ok(None) => break,
                Ok(Some(chunk)) => chunk.map_err(|e| e.to_string())?,
            };
            have += chunk.len() as u64;
            if have > artifact.size {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(format!("{} is larger than expected.", artifact.name));
            }
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            hash.update(&chunk);
            on_progress(have);
        }
        file.flush().await.map_err(|e| e.to_string())?;
    }
    if have != artifact.size {
        return Err("The download ended early. Try again to resume it.".into());
    }
    if format!("{:x}", hash.finalize()) != artifact.sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!(
            "{} failed verification. Try the download again.",
            artifact.name
        ));
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| e.to_string())
}
fn model_url(artifact: &Artifact) -> String {
    format!(
        "https://huggingface.co/{REPO}/resolve/{REVISION}/{}",
        artifact.name
    )
}
async fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("Runtime setup failed ({status})."));
    }
    Ok(())
}
/// Unpacks the runtime archive into `runtime/`.
async fn install_runtime(path: &Path, archive: &Path) -> Result<(), String> {
    let staging = path.join("runtime-staging");
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| e.to_string())?;
    run(Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&staging))
    .await?;
    let unpacked = staging.join(RUNTIME_DIR);
    if !unpacked.join("bin/nemo-speech").is_file() {
        return Err("The runtime archive has no nemo-speech program.".into());
    }
    let _ = tokio::fs::remove_dir_all(path.join("runtime")).await;
    tokio::fs::rename(&unpacked, path.join("runtime"))
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(path.join("runtime/.version"), RUNTIME_VERSION)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = tokio::fs::remove_file(archive).await;
    Ok(())
}
async fn install<R: Runtime>(
    app: &AppHandle<R>,
    path: &Path,
    decoder: &Decoder,
) -> Result<(), String> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| e.to_string())?;
    let total = RUNTIME.size + ENCODER.size + decoder.file.size;
    let percent = |bytes: u64| (bytes * 100 / total).min(99);
    let mut done = 0;
    if !runtime_ready(path) {
        progress(app, decoder.id, "Downloading runtime", 0);
        let archive = path.join(RUNTIME.name);
        fetch(RUNTIME_URL, &archive, &RUNTIME, |bytes| {
            progress(app, decoder.id, "Downloading runtime", percent(bytes))
        })
        .await?;
        install_runtime(path, &archive).await?;
    }
    done += RUNTIME.size;
    for artifact in [&ENCODER, &decoder.file] {
        if !file_ready(path, artifact) {
            fetch(
                &model_url(artifact),
                &gguf(path, artifact),
                artifact,
                |bytes| progress(app, decoder.id, "Downloading model", percent(done + bytes)),
            )
            .await?;
        }
        done += artifact.size;
    }
    progress(app, decoder.id, "Download complete", 100);
    Ok(())
}
struct DownloadGuard;
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        *DOWNLOADING.lock().unwrap() = None;
    }
}
#[tauri::command]
pub async fn nemotron_download_model<R: Runtime>(
    app: AppHandle<R>,
    model: String,
) -> Result<(), String> {
    if !supported() {
        return Err("Nemotron Transcribe requires macOS on Apple silicon.".into());
    }
    let decoder = decoder(&model).ok_or("Unknown Nemotron model.")?;
    {
        let mut downloading = DOWNLOADING.lock().unwrap();
        if downloading.is_some() {
            return Err("A Nemotron download is already running.".into());
        }
        *downloading = Some(decoder.id.to_string());
    }
    let guard = DownloadGuard;
    let _lock = INSTALL_LOCK.lock().await;
    // Bound after the lock so the download clears before a waiting delete can take the lock.
    let _guard = guard;
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Stop transcription before downloading Nemotron.".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    let result = install(&app, &root(&app)?, decoder).await;
    let _ = app.emit(
        "nemotron-download-finished",
        json!({"model":decoder.id,"error":result.as_ref().err()}),
    );
    result
}
#[tauri::command]
pub async fn nemotron_cancel_download() {
    CANCEL.store(true, Ordering::SeqCst);
}
/// Removes a decoder. The last decoder also removes the shared encoder, the
/// runtime and every other file in the Nemotron folder.
async fn delete(path: &Path, decoder: &Decoder) -> Result<(), String> {
    let file = gguf(path, &decoder.file);
    for target in [partial(&file), file] {
        if target.exists() {
            tokio::fs::remove_file(&target)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let others = DECODERS.iter().any(|other| {
        let file = gguf(path, &other.file);
        other.id != decoder.id && (file.exists() || partial(&file).exists())
    });
    if !others && path.exists() {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
pub async fn nemotron_delete_model<R: Runtime>(
    app: AppHandle<R>,
    model: String,
) -> Result<(), String> {
    let decoder = decoder(&model).ok_or("Unknown Nemotron model.")?;
    let _lock = INSTALL_LOCK.lock().await;
    if DOWNLOADING.lock().unwrap().is_some() {
        return Err("Wait for the download to stop.".into());
    }
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Stop transcription before deleting Nemotron.".into());
    }
    delete(&root(&app)?, decoder).await
}

/// 16 kHz mono 16-bit PCM WAV.
fn wav(audio: &[f32]) -> Vec<u8> {
    let data = (audio.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16000u32.to_le_bytes());
    bytes.extend_from_slice(&32000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in audio {
        bytes.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    bytes
}

/// A word with times in seconds from the start of the request audio.
#[derive(Debug, Clone, PartialEq)]
struct Word {
    text: String,
    start: f64,
    end: f64,
}
/// One stretch of speaker activity, in seconds from the start of the request audio.
#[derive(Debug, Clone, PartialEq)]
struct Turn {
    speaker: String,
    start: f64,
    end: f64,
}
#[derive(Debug, Default, PartialEq)]
struct Reply {
    text: String,
    words: Vec<Word>,
    turns: Vec<Turn>,
}
fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}
fn parse_reply(body: &Value) -> Result<Reply, String> {
    let text = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("Nemotron reply has no transcript.")?
        .trim()
        .to_string();
    let transcription = body.get("transcription").cloned().unwrap_or(Value::Null);
    let words = transcription
        .get("words")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|word| {
            Some(Word {
                text: word.get("word")?.as_str()?.to_string(),
                start: number(word, "start")?,
                end: number(word, "end")?,
            })
        })
        .collect();
    Ok(Reply {
        text,
        words,
        turns: Vec::new(),
    })
}

fn parse_word_reply(body: &Value) -> Result<Reply, String> {
    let reply = parse_reply(body)?;
    let timed_text = reply
        .words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    // Silence is returned without a decoder run. Generated tokens with no
    // retained text are a failed decode, not evidence that this clip was silent.
    let empty_decode = reply.text.is_empty()
        && body
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
    if empty_decode
        || reply
            .text
            .split_whitespace()
            .ne(timed_text.split_whitespace())
        || reply
            .words
            .iter()
            .any(|word| !word.start.is_finite() || !word.end.is_finite() || word.end <= word.start)
    {
        return Err("Nemotron did not return valid times for the complete transcript. The saved transcript was kept.".into());
    }
    Ok(reply)
}

/// Only the times are used. Tagged text can have a different word order.
fn parse_speaker_word_reply(body: &Value) -> Reply {
    let words = body
        .pointer("/transcription/words")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|word| {
            let start = number(word, "start")?;
            let end = number(word, "end")?;
            if !start.is_finite() || !end.is_finite() {
                return None;
            }
            Some(Word {
                text: word.get("word")?.as_str()?.to_string(),
                start,
                end: end.max(start),
            })
        })
        .collect();
    Reply {
        words,
        ..Reply::default()
    }
}

/// Speaker turns from `/v1/audio/diarizations`.
fn parse_turns(body: &Value) -> Result<Vec<Turn>, String> {
    let segments = body
        .get("segments")
        .and_then(Value::as_array)
        .ok_or("Nemotron reply has no speaker turns.")?;
    Ok(segments
        .iter()
        .filter_map(|turn| {
            Some(Turn {
                speaker: turn.get("speaker")?.as_u64()?.to_string(),
                start: number(turn, "start")?,
                end: number(turn, "end")?,
            })
        })
        .collect())
}

/// What a request asks the server for.
#[derive(Clone, Copy, PartialEq)]
enum Job {
    Text,
    Words,
    /// Words with NVIDIA's speaker-tag instruction, used only for their times.
    SpeakerWords,
    Speakers,
}
/// NVIDIA's prompt guide instruction for "who said what, with punctuation".
/// Tagging speakers changes how the runtime lines words up with the audio:
/// on a checked meeting, plain requests packed four sentences seconds early
/// into the other person's turn, and tagged requests timed all four right.
/// Their words were less accurate, so only their times are used.
const SPEAKER_INSTRUCTION: &str = "Transcribe this audio with punctuation and capitalization. Include all spoken words and verbal fillers. Write numbers in their standard written form. Put speaker tags <spk:0> <spk:1> <spk:2> <spk:3> on the transcription.";
fn max_request_samples(job: Job) -> usize {
    if job == Job::Speakers {
        MAX_SPEAKER_SAMPLES
    } else {
        MAX_TEXT_SAMPLES
    }
}

/// Why a request failed, and whether its server can take the next request.
enum RequestError {
    /// The server rejected this audio and is still running.
    Segment(String),
    /// Text and word times disagree, or a decode produced no usable text.
    Alignment(String),
    /// The server stopped or cannot be reached. It must be replaced.
    Server(String),
}
struct Server {
    child: Child,
    base: String,
    key: String,
    client: reqwest::Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
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
impl Server {
    async fn spawn(path: &Path, decoder: &Decoder) -> Result<Self, String> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        // Other local programs could reach a loopback port. The key keeps them out.
        let key: String = rand::random::<[u8; 32]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let log_path = path.join("server.log");
        let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let child = Command::new(binary(path))
            .arg("serve")
            .arg("--asr-model")
            .arg(gguf(path, &decoder.file))
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--no-ui",
            ])
            .args(["--asr.model.name", "nemotron"])
            // Meetily sends one request at a time. One slot with a pool for an
            // hour of audio keeps the cache near 0.5 GB on unified memory.
            .args(["--asr.salm.max_sequences", "1"])
            .args(["--asr.salm.n_ctx_per_sequence", "73728"])
            .args([
                "--threads",
                "8",
                "--read-timeout",
                "600",
                "--write-timeout",
                "600",
            ])
            .env("NEMO_SPEECH_HTTP_API_KEY", &key)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let mut server = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            key,
            client,
        };
        server.wait_ready(&log_path).await?;
        Ok(server)
    }
    async fn wait_ready(&mut self, log_path: &Path) -> Result<(), String> {
        let started = tokio::time::Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "Nemotron stopped while loading ({status}): {}. See nemotron/server.log.",
                    last_log_line(log_path)
                ));
            }
            let ready = self
                .client
                .get(format!("{}/ready", self.base))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map(|response| response.status().is_success())
                .unwrap_or(false);
            if ready {
                return Ok(());
            }
            if started.elapsed() > LOAD_TIMEOUT {
                return Err("Nemotron took too long to load. See nemotron/server.log.".into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    async fn request(&mut self, audio: &[f32], job: Job) -> Result<Reply, RequestError> {
        // Decoding is slower than real time on some Macs.
        let timeout =
            Duration::from_secs(600) + Duration::from_secs_f64(audio.len() as f64 / 8000.0);
        let request = if job == Job::Speakers {
            let file = reqwest::multipart::Part::bytes(wav(audio))
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| RequestError::Segment(e.to_string()))?;
            self.client
                .post(format!("{}/v1/audio/diarizations", self.base))
                .multipart(reqwest::multipart::Form::new().part("file", file))
        } else {
            let mut content = vec![json!({"type": "input_audio", "input_audio": {"data": STANDARD.encode(wav(audio)), "format": "wav"}})];
            if job == Job::SpeakerWords {
                content.insert(0, json!({"type": "text", "text": SPEAKER_INSTRUCTION}));
            }
            self.client
                .post(format!("{}/v1/chat/completions", self.base))
                .json(&json!({
                    "model": "nemotron",
                    "word_timestamps": job != Job::Text,
                    "messages": [{"role": "user", "content": content}],
                }))
        };
        let response = request
            .bearer_auth(&self.key)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| RequestError::Server(format!("Nemotron request failed: {e}")))?;
        let status = response.status();
        let reply: Value = response
            .json()
            .await
            .map_err(|e| RequestError::Server(format!("Nemotron reply is invalid: {e}")))?;
        if !status.is_success() {
            let message = reply
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no details");
            let error = format!("Nemotron ({status}): {message}");
            return Err(match self.child.try_wait() {
                Ok(None) => RequestError::Segment(error),
                _ => RequestError::Server(error),
            });
        }
        if job == Job::Speakers {
            return parse_turns(&reply)
                .map(|turns| Reply {
                    turns,
                    ..Reply::default()
                })
                .map_err(RequestError::Segment);
        }
        if job == Job::SpeakerWords {
            Ok(parse_speaker_word_reply(&reply))
        } else if job == Job::Words {
            parse_word_reply(&reply).map_err(RequestError::Alignment)
        } else {
            parse_reply(&reply).map_err(RequestError::Segment)
        }
    }
}

/// A whole recording with speakers.
pub struct MeetingTranscript {
    /// Text, start and end in milliseconds.
    pub words: Vec<(String, f64, f64)>,
    /// Speaker key, start and end in seconds.
    pub turns: Vec<(String, f64, f64)>,
}

pub struct NemotronProvider {
    path: PathBuf,
    decoder: &'static Decoder,
    /// Empty after a server failure. The next request starts a new server.
    server: Mutex<Option<Server>>,
}
impl NemotronProvider {
    /// Counts as active until dropped, which blocks download and delete.
    fn new(path: PathBuf, decoder: &'static Decoder, server: Option<Server>) -> Self {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self {
            path,
            decoder,
            server: Mutex::new(server),
        }
    }
}
impl Drop for NemotronProvider {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}
fn lock_model_files_for_start(lock: &Mutex<()>) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
    lock.try_lock().map_err(|_| {
        "Nemotron is downloading or loading a model. Wait for it to finish, or cancel the download in Settings, then try again.".to_string()
    })
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
    let decoder = decoder(&selected).ok_or("Select a Nemotron model in Settings.")?;
    // A download can take many minutes. Report it instead of leaving Enhance
    // at "Loading Nemotron" while another model's download holds this lock.
    let _lock = lock_model_files_for_start(&INSTALL_LOCK)?;
    if ACTIVE.load(Ordering::SeqCst) > 0 {
        return Err("Nemotron is in use. Stop the current transcription first.".into());
    }
    let path = root(app)?;
    let server = Server::spawn(&path, decoder).await?;
    Ok(NemotronProvider::new(path, decoder, Some(server)))
}
async fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
impl NemotronProvider {
    /// Sends audio in pieces cut at quiet points. Each part is the request's
    /// reply and its offset in seconds. Cancelling stops the server.
    async fn requests(
        &self,
        audio: &[f32],
        job: Job,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<(Reply, f64)>, TranscriptionError> {
        if audio.len() < MIN_SAMPLES {
            return Err(TranscriptionError::AudioTooShort {
                samples: audio.len(),
                minimum: MIN_SAMPLES,
            });
        }
        if audio.iter().any(|s| !s.is_finite()) {
            return Err(TranscriptionError::EngineFailed(
                "Audio contains invalid samples.".into(),
            ));
        }
        let mut guard = self.server.lock().await;
        let mut parts = Vec::new();
        let mut pending: std::collections::VecDeque<_> =
            crate::audio::transcription::funasr_provider::request_ranges(audio, max_request_samples(job)).into();
        while let Some(range) = pending.pop_front() {
            let offset = range.start as f64 / 16000.0;
            // Taken out so a failed or cancelled request drops, and stops, its server.
            let mut server = match guard.take() {
                Some(server) => server,
                None => {
                    log::warn!("Restarting the Nemotron server after a failure");
                    Server::spawn(&self.path, self.decoder)
                        .await
                        .map_err(TranscriptionError::EngineFailed)?
                }
            };
            let piece = &audio[range.clone()];
            let result = match cancel {
                Some(flag) => tokio::select! {
                    result = server.request(piece, job) => result,
                    _ = wait_for(flag) => return Err(TranscriptionError::EngineFailed("Cancelled.".into())),
                },
                None => server.request(piece, job).await,
            };
            match result {
                Ok(reply) => {
                    *guard = Some(server);
                    parts.push((reply, offset));
                }
                Err(RequestError::Segment(error)) => {
                    *guard = Some(server);
                    return Err(TranscriptionError::EngineFailed(error));
                }
                Err(RequestError::Alignment(error)) => {
                    *guard = Some(server);
                    if piece.len() <= RETRY_TEXT_SAMPLES {
                        return Err(TranscriptionError::EngineFailed(error));
                    }
                    log::warn!("Nemotron returned incomplete word times; retrying this clip in shorter parts");
                    for retry in crate::audio::transcription::funasr_provider::request_ranges(piece, RETRY_TEXT_SAMPLES).into_iter().rev() {
                        pending.push_front(range.start + retry.start..range.start + retry.end);
                    }
                }
                Err(RequestError::Server(error)) => {
                    return Err(TranscriptionError::EngineFailed(error));
                }
            }
        }
        Ok(parts)
    }

    /// Words with start and end times in milliseconds from the start of `audio`.
    pub async fn transcribe_words(
        &self,
        audio: &[f32],
    ) -> Result<Vec<(String, f64, f64)>, TranscriptionError> {
        let parts = self.requests(audio, Job::Words, None).await?;
        Ok(words_on_timeline(parts, audio.len() as f64 / 16.0))
    }

    /// Transcribes a whole recording with word times and speakers. Speakers
    /// come from one request per hour, so their numbers hold across pieces
    /// of text.
    pub async fn transcribe_meeting(
        &self,
        audio: &[f32],
        cancel: &AtomicBool,
    ) -> Result<MeetingTranscript, TranscriptionError> {
        let duration_ms = audio.len() as f64 / 16.0;
        let speakers = self.requests(audio, Job::Speakers, Some(cancel)).await?;
        let text = self.requests(audio, Job::Words, Some(cancel)).await?;
        let mut words = words_on_timeline(text, duration_ms);
        match self.requests(audio, Job::SpeakerWords, Some(cancel)).await {
            Ok(timed) => words = times_from(words, &words_on_timeline(timed, duration_ms)),
            Err(error) if cancel.load(Ordering::SeqCst) => return Err(error),
            Err(error) => log::warn!("Nemotron kept its plain word times: {error}"),
        }
        Ok(MeetingTranscript {
            words,
            turns: bridge_turn_gaps(turns_on_timeline(speakers), duration_ms / 1000.0),
        })
    }
}

/// Moves request words onto one timeline in milliseconds, in order and
/// inside the audio.
fn words_on_timeline(parts: Vec<(Reply, f64)>, duration_ms: f64) -> Vec<(String, f64, f64)> {
    let mut words = Vec::new();
    let mut previous = 0.0;
    for (reply, offset) in parts {
        for word in reply.words {
            if !word.start.is_finite() || !word.end.is_finite() {
                continue;
            }
            let start = ((offset + word.start) * 1000.0).clamp(previous, duration_ms);
            let end = ((offset + word.end) * 1000.0).clamp(start, duration_ms);
            words.push((word.text, start, end));
            previous = start;
        }
    }
    words
}

/// Gives `words` the times of the same words in `timed`, matched in order.
/// Words without a match are spread between their matched neighbours.
/// The text and its order are unchanged.
fn times_from(
    mut words: Vec<(String, f64, f64)>,
    timed: &[(String, f64, f64)],
) -> Vec<(String, f64, f64)> {
    /// Words per matching block, which bounds memory on long recordings.
    const BLOCK: usize = 300;
    /// How far past a block the same words can be timed.
    const REACH_MS: f64 = 30_000.0;
    let key = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut matched: Vec<Option<usize>> = vec![None; words.len()];
    let mut cursor = 0;
    for first in (0..words.len()).step_by(BLOCK) {
        let last = (first + BLOCK).min(words.len());
        let lookahead = (last + BLOCK).min(words.len());
        let limit = words[last - 1].2 + REACH_MS;
        let end = (cursor..timed.len())
            .find(|&j| timed[j].1 > limit)
            .unwrap_or(timed.len());
        let a: Vec<String> = words[first..lookahead].iter().map(|w| key(&w.0)).collect();
        let b: Vec<String> = timed[cursor..end].iter().map(|w| key(&w.0)).collect();
        // Longest common subsequence, traced from the start.
        let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                table[i][j] = if !a[i].is_empty() && a[i] == b[j] {
                    table[i + 1][j + 1] + 1
                } else {
                    table[i + 1][j].max(table[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        // Include the next block in the LCS, but save only this block's matches.
        while i < last - first && j < b.len() {
            if !a[i].is_empty() && a[i] == b[j] {
                matched[first + i] = Some(cursor + j);
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        if let Some(last_match) = matched[first..last].iter().rev().flatten().next() {
            cursor = last_match + 1;
        }
    }
    let original = words.clone();
    for (i, found) in matched.iter().enumerate() {
        if let Some(j) = found {
            words[i].1 = timed[*j].1;
            words[i].2 = timed[*j].2;
        }
    }
    // Spread each run of unmatched words between its matched neighbours.
    let mut i = 0;
    while i < words.len() {
        if matched[i].is_some() {
            i += 1;
            continue;
        }
        let run = i..(i..words.len()).find(|&k| matched[k].is_some()).unwrap_or(words.len());
        let from = (run.start > 0).then(|| words[run.start - 1].2);
        let to = words.get(run.end).map(|w| w.1);
        if let (Some(from), Some(to)) = (from, to) {
            let step = (to - from).max(0.0) / run.len() as f64;
            for (n, k) in run.clone().enumerate() {
                words[k].1 = from + step * n as f64;
                words[k].2 = from + step * (n + 1) as f64;
            }
        } else {
            for k in run.clone() {
                words[k].1 = original[k].1;
                words[k].2 = original[k].2;
            }
        }
        i = run.end;
    }
    // Keep the timeline in order.
    let mut previous = 0.0;
    for word in &mut words {
        word.1 = word.1.max(previous);
        word.2 = word.2.max(word.1);
        previous = word.1;
    }
    words
}

/// Moves speaker turns onto one timeline in seconds. Speaker numbers are per
/// request, so each hour of a longer recording gets its own keys.
fn turns_on_timeline(parts: Vec<(Reply, f64)>) -> Vec<(String, f64, f64)> {
    let several = parts.len() > 1;
    let mut turns = Vec::new();
    for (index, (reply, offset)) in parts.into_iter().enumerate() {
        for turn in reply.turns {
            if turn.start.is_finite() && turn.end.is_finite() && turn.end > turn.start {
                let key = if several {
                    format!("nemotron-{}-{}", index + 1, turn.speaker)
                } else {
                    format!("nemotron-{}", turn.speaker)
                };
                turns.push((key, offset + turn.start, offset + turn.end));
            }
        }
    }
    turns
}

/// Word times and speaker turns come from different heads. A turn's first or
/// last word often ends exactly where the turn begins, or sits in the short
/// pause beside it, so it matched no speaker and became a row of its own.
/// Each pause goes to the turns on either side, up to one second each. The
/// first and last turns also reach up to one second toward the recording's
/// start and end.
fn bridge_turn_gaps(
    mut turns: Vec<(String, f64, f64)>,
    duration: f64,
) -> Vec<(String, f64, f64)> {
    const MAX_BRIDGE: f64 = 1.0;
    turns.sort_by(|a, b| a.1.total_cmp(&b.1));
    if let Some(first) = turns.first_mut() {
        first.1 = (first.1 - MAX_BRIDGE).max(0.0).min(first.1);
    }
    let mut latest: Option<usize> = None;
    for index in 0..turns.len() {
        if let Some(previous) = latest {
            let gap = turns[index].1 - turns[previous].2;
            if gap > 0.0 {
                let bridge = (gap / 2.0).min(MAX_BRIDGE);
                turns[previous].2 += bridge;
                turns[index].1 -= bridge;
            }
        }
        if latest.map_or(true, |previous| turns[index].2 > turns[previous].2) {
            latest = Some(index);
        }
    }
    if let Some(last) = latest {
        turns[last].2 = (turns[last].2 + MAX_BRIDGE).min(duration).max(turns[last].2);
    }
    turns
}

#[async_trait]
impl TranscriptionProvider for NemotronProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        _language: Option<String>,
    ) -> Result<TranscriptResult, TranscriptionError> {
        let parts = self.requests(&audio, Job::Text, None).await?;
        Ok(TranscriptResult {
            text: parts
                .into_iter()
                .map(|(reply, _)| reply.text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            confidence: None,
            is_partial: false,
        })
    }
    async fn is_model_loaded(&self) -> bool {
        // The install cannot change while a provider is active, and a stopped server restarts on use.
        true
    }
    async fn get_current_model(&self) -> Option<String> {
        Some(self.decoder.id.to_string())
    }
    fn provider_name(&self) -> &'static str {
        "Nemotron"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn sparse(path: &Path, artifact: &Artifact) {
        let file = gguf(path, artifact);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::File::create(file)
            .unwrap()
            .set_len(artifact.size)
            .unwrap();
    }
    fn install_runtime_stub(path: &Path) {
        std::fs::create_dir_all(path.join("runtime/bin")).unwrap();
        std::fs::write(binary(path), "").unwrap();
        std::fs::write(path.join("runtime/.version"), RUNTIME_VERSION).unwrap();
    }

    #[tokio::test]
    async fn starting_during_a_download_returns_an_error_without_waiting() {
        let lock = Mutex::new(());
        let download = lock.lock().await;
        let error = lock_model_files_for_start(&lock).unwrap_err();
        assert!(error.contains("cancel the download in Settings"));
        drop(download);
        assert!(lock_model_files_for_start(&lock).is_ok());
    }

    #[test]
    fn catalog_has_pinned_hashes_and_unique_models() {
        let mut ids = DECODERS.iter().map(|d| d.id).collect::<Vec<_>>();
        ids.dedup();
        assert_eq!(ids.len(), DECODERS.len());
        let prefix = ENCODER.name.trim_end_matches("-Encoder-F16.gguf");
        for artifact in DECODERS.iter().map(|d| &d.file).chain([&ENCODER, &RUNTIME]) {
            assert!(artifact.size > 0);
            assert_eq!(artifact.sha256.len(), 64);
            assert!(artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        for decoder in DECODERS {
            assert!(
                decoder.file.name.starts_with(prefix),
                "{}",
                decoder.file.name
            );
        }
        assert!(RUNTIME_URL.ends_with(RUNTIME.name));
        assert_eq!(REVISION.len(), 40);
    }

    #[test]
    fn a_model_is_ready_only_with_runtime_encoder_and_its_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let (q2, iq4) = (&DECODERS[0], &DECODERS[1]);
        sparse(dir.path(), &q2.file);
        sparse(dir.path(), &ENCODER);
        assert!(!ready(dir.path(), q2), "no runtime");
        install_runtime_stub(dir.path());
        assert!(ready(dir.path(), q2));
        assert!(!ready(dir.path(), iq4));
        std::fs::write(dir.path().join("runtime/.version"), "older").unwrap();
        assert!(!ready(dir.path(), q2), "an older runtime is not ready");
        install_runtime_stub(dir.path());
        std::fs::write(gguf(dir.path(), &ENCODER), "partial").unwrap();
        assert!(!ready(dir.path(), q2));
    }

    #[tokio::test]
    async fn deleting_keeps_shared_files_until_the_last_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nemotron");
        let (q2, iq4) = (&DECODERS[0], &DECODERS[1]);
        install_runtime_stub(&path);
        sparse(&path, &ENCODER);
        sparse(&path, &q2.file);
        std::fs::write(partial(&gguf(&path, &iq4.file)), "resume me").unwrap();
        delete(&path, q2).await.unwrap();
        assert!(!gguf(&path, &q2.file).exists());
        assert!(
            gguf(&path, &ENCODER).exists(),
            "a partial download still uses the encoder"
        );
        assert!(runtime_ready(&path));
        delete(&path, iq4).await.unwrap();
        assert!(!path.exists());
    }

    /// Serves `body` over HTTP, honouring `Range: bytes=N-` when `ranges` is
    /// true. Returns the address and the received request heads.
    async fn serve(
        body: Vec<u8>,
        ranges: bool,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/file", listener.local_addr().unwrap());
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = heads.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if socket.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_lowercase();
                seen.lock().unwrap().push(head.clone());
                let start = head
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|range| range.trim().trim_end_matches('-').parse::<usize>().ok())
                    .filter(|_| ranges);
                let (status, slice) = match start {
                    Some(start) => ("206 Partial Content", &body[start..]),
                    None => ("200 OK", &body[..]),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.write_all(slice).await.unwrap();
            }
        });
        (address, heads)
    }
    fn artifact_for(body: &[u8]) -> Artifact {
        Artifact {
            name: "test.gguf",
            size: body.len() as u64,
            sha256: Box::leak(format!("{:x}", Sha256::digest(body)).into_boxed_str()),
        }
    }

    #[tokio::test]
    async fn downloads_resume_from_a_partial_file_and_are_verified() {
        let body = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let artifact = artifact_for(&body);
        let (url, heads) = serve(body.clone(), true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("gguf/test.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(partial(&dest), &body[..70_000]).unwrap();
        let seen = std::sync::Mutex::new(0);
        fetch(&url, &dest, &artifact, |bytes| {
            *seen.lock().unwrap() = bytes
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!partial(&dest).exists());
        assert_eq!(*seen.lock().unwrap(), body.len() as u64);
        assert!(heads.lock().unwrap()[0].contains("range: bytes=70000-"));
    }

    #[tokio::test]
    async fn a_server_that_ignores_ranges_restarts_the_file() {
        let body = vec![7u8; 50_000];
        let artifact = artifact_for(&body);
        let (url, _) = serve(body.clone(), false).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("test.gguf");
        std::fs::write(partial(&dest), vec![7u8; 10_000]).unwrap();
        fetch(&url, &dest, &artifact, |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[tokio::test]
    async fn a_file_with_the_wrong_hash_is_removed() {
        let body = vec![1u8; 4096];
        let mut artifact = artifact_for(&body);
        artifact.sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
        let (url, _) = serve(body, true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("test.gguf");
        let error = fetch(&url, &dest, &artifact, |_| {}).await.unwrap_err();
        assert!(error.contains("failed verification"), "{error}");
        assert!(!dest.exists() && !partial(&dest).exists());
    }

    #[test]
    fn replies_give_text_and_word_times() {
        let body = json!({
            "choices": [{"message": {"content": " Hello John, how are you? "}}],
            "transcription": {"words": [
                {"word": "Hello", "start": 0.555, "end": 0.713, "speaker": "spk:0"},
                {"word": "John,", "start": 0.75, "end": 1.0},
                {"word": "broken"}
            ]}
        });
        let reply = parse_reply(&body).unwrap();
        assert_eq!(reply.text, "Hello John, how are you?");
        assert_eq!(
            reply.words[1],
            Word {
                text: "John,".into(),
                start: 0.75,
                end: 1.0
            }
        );
        assert_eq!(reply.words.len(), 2);
        let empty = parse_reply(&json!({"choices": [{"message": {"content": ""}}]})).unwrap();
        assert_eq!(empty, Reply::default());
        assert!(parse_reply(&json!({})).is_err());
    }

    #[test]
    fn incomplete_word_times_cannot_silently_drop_transcript_text() {
        let mut body = json!({
            "choices": [{"message": {"content": "Keep every word."}}],
            "transcription": {"words": [
                {"word": "Keep", "start": 0.1, "end": 0.3}
            ]}
        });
        assert!(parse_word_reply(&body)
            .unwrap_err()
            .contains("saved transcript was kept"));
        body["transcription"]["words"] = json!([]);
        assert!(parse_word_reply(&body).is_err());
        body["transcription"]["words"] = json!([
            {"word": "Keep", "start": 0.1, "end": 0.3},
            {"word": "every", "start": 0.3, "end": 0.5},
            {"word": "word.", "start": 0.5, "end": 0.7}
        ]);
        assert!(parse_word_reply(&body).is_ok());
        body["transcription"]["words"][1]["end"] = json!(0.2);
        assert!(parse_word_reply(&body).is_err());
    }

    #[test]
    fn failed_empty_decode_is_not_treated_as_silence() {
        let mut body = json!({"choices": [{"message": {"content": ""}}]});
        assert!(parse_word_reply(&body).is_ok());
        body["usage"] = json!({"completion_tokens": 2000});
        assert!(parse_word_reply(&body).is_err());
    }

    #[test]
    fn speaker_replies_give_turns() {
        let body = json!({"segments": [
            {"start": 8.08, "end": 9.32, "speaker": 1},
            {"start": 15.0, "end": 15.53, "speaker": 2},
            {"start": 1.0, "speaker": 3}
        ]});
        assert_eq!(
            parse_turns(&body).unwrap(),
            vec![
                Turn {
                    speaker: "1".into(),
                    start: 8.08,
                    end: 9.32
                },
                Turn {
                    speaker: "2".into(),
                    start: 15.0,
                    end: 15.53
                },
            ]
        );
        assert!(parse_turns(&json!({})).is_err());
    }

    #[test]
    fn words_and_turns_move_onto_one_timeline() {
        let word = |text: &str, start, end| Word {
            text: text.into(),
            start,
            end,
        };
        let words = words_on_timeline(
            vec![
                (
                    Reply {
                        words: vec![word("Hi.", 1.0, 1.5), word("x", f64::NAN, 1.0)],
                        ..Reply::default()
                    },
                    0.0,
                ),
                (
                    Reply {
                        words: vec![word("Bye.", 2.0, 99.0)],
                        ..Reply::default()
                    },
                    300.0,
                ),
            ],
            305_000.0,
        );
        assert_eq!(
            words,
            vec![
                ("Hi.".into(), 1000.0, 1500.0),
                ("Bye.".into(), 302_000.0, 305_000.0)
            ]
        );
        let turn = |speaker: &str, start, end| Turn {
            speaker: speaker.into(),
            start,
            end,
        };
        let one = turns_on_timeline(vec![(
            Reply {
                turns: vec![turn("1", 0.9, 1.6), turn("2", 3.0, 3.0)],
                ..Reply::default()
            },
            0.0,
        )]);
        assert_eq!(one, vec![("nemotron-1".into(), 0.9, 1.6)]);
        // Numbers restart in each hour, so they must not merge.
        let two = turns_on_timeline(vec![
            (
                Reply {
                    turns: vec![turn("1", 0.9, 1.6)],
                    ..Reply::default()
                },
                0.0,
            ),
            (
                Reply {
                    turns: vec![turn("1", 1.9, 2.5)],
                    ..Reply::default()
                },
                3600.0,
            ),
        ]);
        assert_eq!(
            two,
            vec![
                ("nemotron-1-1".into(), 0.9, 1.6),
                ("nemotron-2-1".into(), 3601.9, 3602.5)
            ]
        );
    }

    /// Runs the real server. Set NEMOTRON_TEST_ROOT to a folder with
    /// `runtime/` and `gguf/` laid out as the app installs them, and
    /// NEMOTRON_TEST_AUDIO to a recording.
    #[tokio::test]
    #[ignore]
    async fn real_server_transcribes_a_meeting_with_speakers() {
        let root = PathBuf::from(std::env::var("NEMOTRON_TEST_ROOT").unwrap());
        let audio = std::env::var("NEMOTRON_TEST_AUDIO").unwrap();
        let samples = crate::audio::decoder::decode_audio_file(Path::new(&audio))
            .unwrap()
            .to_whisper_format();
        let decoder = &DECODERS[0];
        assert!(ready(&root, decoder));
        let started = std::time::Instant::now();
        let server = Server::spawn(&root, decoder).await.unwrap();
        println!("loaded in {:.1}s", started.elapsed().as_secs_f64());
        let provider = NemotronProvider::new(root, decoder, Some(server));
        let started = std::time::Instant::now();
        let meeting = provider
            .transcribe_meeting(&samples, &AtomicBool::new(false))
            .await
            .unwrap();
        println!(
            "{:.0}s of audio in {:.1}s: {} words, {} turns",
            samples.len() as f64 / 16000.0,
            started.elapsed().as_secs_f64(),
            meeting.words.len(),
            meeting.turns.len()
        );
        let text = meeting.words.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ");
        let marks = text.matches(['.', '?', '!']).count();
        println!("sentence marks: {marks}\n{}", &text[..text.len().min(1500)]);
        let speakers = meeting.turns.iter().map(|t| t.0.clone()).collect::<std::collections::BTreeSet<_>>();
        println!("speakers: {speakers:?}");
        assert!(meeting.words.len() > 100 && marks > 20 && speakers.len() > 1);
        let short = provider.transcribe(samples[..16000 * 20].to_vec(), None).await.unwrap();
        println!("live-style 20 s: {}", short.text);
        assert!(!short.text.is_empty());
        assert!(meeting.words.first().unwrap().1 < 20_000.0, "the opening speech was dropped");
        let tail = provider.transcribe(samples[samples.len() - 16000 * 20..].to_vec(), None).await.unwrap();
        if !tail.text.is_empty() {
            assert!(meeting.words.last().unwrap().2 > samples.len() as f64 / 16.0 - 20_000.0,
                "the closing speech was dropped");
        }
        if let Ok(path) = std::env::var("NEMOTRON_TEST_TRANSCRIPT") {
            std::fs::write(path, json!({"text": text, "words": meeting.words, "turns": meeting.turns}).to_string()).unwrap();
        }
    }

    #[test]
    fn a_word_at_the_edge_of_a_turn_gets_the_nearest_speaker() {
        let turn = |speaker: &str, start, end| (speaker.to_string(), start, end);
        let turns = bridge_turn_gaps(
            vec![
                turn("2", 21.5, 26.0),
                turn("1", 3.3, 6.2),
                // Overlapping speech keeps both turns as they are.
                turn("1", 6.0, 20.0),
                turn("3", 30.0, 31.0),
            ],
            31.5,
        );
        assert_eq!(
            turns,
            vec![
                turn("1", 2.3, 6.2),
                turn("1", 6.0, 20.75),
                turn("2", 20.75, 27.0),
                turn("3", 29.0, 31.5),
            ]
        );
        // A single turn reaches toward both ends, inside the recording.
        assert_eq!(
            bridge_turn_gaps(vec![turn("1", 0.4, 9.0)], 12.0),
            vec![turn("1", 0.0, 10.0)]
        );
    }

    fn timed(words: &[(&str, f64, f64)]) -> Vec<(String, f64, f64)> {
        words.iter().map(|(text, start, end)| (text.to_string(), *start, *end)).collect()
    }

    /// The checked meeting at 09:02. A plain request packed "As of right
    /// now ... Sounds good." into 539.7-541.2 s; it was said over 542-547 s.
    #[test]
    fn words_keep_their_text_and_take_the_speaker_request_times() {
        let plain = timed(&[
            ("discussion.", 539_493.0, 539_640.0),
            ("As", 539_653.0, 539_933.0),
            ("of", 539_933.0, 539_973.0),
            ("right", 539_973.0, 540_053.0),
            ("now", 540_053.0, 540_070.0),
            ("it", 540_070.0, 540_133.0),
            ("seems", 540_133.0, 540_213.0),
            ("like", 540_213.0, 540_292.0),
            ("uh", 540_292.0, 540_372.0),
            ("pretty", 540_372.0, 540_452.0),
            ("pretty", 540_452.0, 540_532.0),
            ("good", 540_532.0, 540_612.0),
            ("bandwidth", 540_612.0, 540_932.0),
            ("so.", 540_932.0, 541_011.0),
            ("Sounds", 541_011.0, 541_171.0),
            ("good.", 541_171.0, 541_190.0),
            ("Okay.", 547_403.0, 548_362.0),
        ]);
        let tagged = timed(&[
            ("Yeah,", 540_290.0, 540_510.0),
            ("a", 540_532.0, 540_772.0),
            ("discussion.", 540_772.0, 541_190.0),
            ("as", 542_050.0, 542_090.0),
            ("for", 542_090.0, 542_529.0),
            ("now,", 542_529.0, 542_769.0),
            ("it", 542_769.0, 542_929.0),
            ("seems", 542_929.0, 543_249.0),
            ("like", 543_249.0, 543_488.0),
            ("a", 543_488.0, 543_808.0),
            ("pretty", 543_808.0, 544_287.0),
            ("pretty", 544_287.0, 544_607.0),
            ("good", 544_607.0, 544_767.0),
            ("bandwidth,", 544_767.0, 545_406.0),
            ("so", 545_406.0, 545_680.0),
            ("sounds", 546_844.0, 546_880.0),
            ("good.", 546_880.0, 547_380.0),
            ("Okay,", 547_403.0, 548_362.0),
        ]);
        let words = times_from(plain.clone(), &tagged);
        let texts = |words: &[(String, f64, f64)]| words.iter().map(|w| w.0.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&words), texts(&plain));
        let at = |text: &str| words.iter().find(|w| w.0 == text).unwrap().1;
        assert_eq!((at("As"), at("seems"), at("bandwidth")), (542_050.0, 542_929.0, 544_767.0));
        // "of right" and "uh" are not in the tagged words: they fall between
        // their matched neighbours.
        assert!(at("of") >= 542_090.0 && at("right") < 542_529.0, "{words:?}");
        assert!(at("uh") >= 543_488.0 && at("uh") < 543_808.0, "{words:?}");
        assert_eq!(at("Sounds"), 546_844.0);
        assert!(words.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    }

    #[test]
    fn a_block_keeps_matches_for_words_in_the_next_block() {
        let prefix: Vec<_> = (0..299)
            .map(|i| (format!("word{i}"), (i + 1) as f64 * 300.0, (i + 2) as f64 * 300.0))
            .collect();
        let mut plain = prefix.clone();
        plain.extend(timed(&[
            ("yes", 90_000.0, 91_000.0),
            ("we", 91_000.0, 92_000.0),
            ("agree", 92_000.0, 93_000.0),
            ("yes", 93_000.0, 94_000.0),
            ("done", 94_000.0, 95_000.0),
        ]));
        let mut tagged = prefix;
        tagged.extend_from_slice(&plain[300..]);

        let words = times_from(plain.clone(), &tagged);
        assert_eq!(words[..299], plain[..299]);
        assert_eq!(words[299..], plain[299..]);
    }

    #[test]
    fn speaker_words_keep_time_order_and_zero_length_words() {
        let body = json!({
            "choices": [{"message": {"content": "<spk:0> Proving So Yep. <spk:1> And Hm."}}],
            "transcription": {"words": [
                {"word": "Proving", "start": 0.48, "end": 0.48},
                {"word": "And", "start": 0.5, "end": 0.7},
                {"word": "So", "start": 0.6, "end": 0.8},
                {"word": "Yep.", "start": 1.0, "end": 1.2},
                {"word": "Hm.", "start": 1.1, "end": 1.3}
            ]}
        });
        let reply = parse_speaker_word_reply(&body);
        let words = words_on_timeline(vec![(reply, 0.0)], 2000.0);
        assert_eq!(
            words,
            timed(&[
                ("Proving", 480.0, 480.0),
                ("And", 500.0, 700.0),
                ("So", 600.0, 800.0),
                ("Yep.", 1000.0, 1200.0),
                ("Hm.", 1100.0, 1300.0),
            ])
        );
        assert!(parse_word_reply(&body).is_err());
    }

    #[test]
    fn speaker_words_allow_missing_times_and_clamp_reversed_times() {
        let empty = parse_speaker_word_reply(&json!({
            "choices": [{"message": {"content": "<spk:0> Proving"}}],
            "transcription": {"words": []}
        }));
        assert!(empty.words.is_empty());
        let plain = timed(&[("Proving", 100.0, 200.0)]);
        let times = words_on_timeline(vec![(empty, 0.0)], 1000.0);
        assert_eq!(times_from(plain.clone(), &times), plain);

        let reply = parse_speaker_word_reply(&json!({"transcription": {"words": [
            {"word": "Proving", "start": 0.48, "end": 0.4},
            {"word": "invalid", "start": null, "end": 0.8},
            {"word": "invalid", "start": 0.8, "end": null}
        ]}}));
        assert_eq!(
            reply.words,
            vec![Word {
                text: "Proving".into(),
                start: 0.48,
                end: 0.48,
            }]
        );
    }

    #[test]
    fn text_requests_stay_short_enough_to_follow_the_audio() {
        let audio = vec![0.1f32; 17 * 60 * 16000];
        for job in [Job::Text, Job::Words] {
            let ranges = crate::audio::transcription::funasr_provider::request_ranges(
                &audio,
                max_request_samples(job),
            );
            assert!(ranges.iter().all(|range| range.len() <= 60 * 16000));
            assert_eq!(ranges.last().unwrap().end, audio.len());
        }
        let speakers = crate::audio::transcription::funasr_provider::request_ranges(
            &audio,
            max_request_samples(Job::Speakers),
        );
        assert_eq!(speakers, vec![0..audio.len()], "speaker numbers must hold across the meeting");
    }

    #[test]
    fn audio_is_sent_as_16_khz_mono_pcm_wav() {
        let bytes = wav(&[0.0, 1.0, -2.0]);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 16000);
        assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 32767);
        assert_eq!(i16::from_le_bytes([bytes[48], bytes[49]]), -32767);
    }
}
