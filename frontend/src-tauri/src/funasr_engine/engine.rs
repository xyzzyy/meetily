use super::model::{
    artifact_url, decoder_relative_path, encoder_relative_path, model_spec, ArtifactSpec,
    DownloadProgress, ModelInfo, ModelStatus, ENCODER, MODELS, MODEL_DIR,
};
use super::sidecar::FunAsrSidecar;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::time::Instant;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// A download fails if no data arrives for this long (same as Parakeet).
const DOWNLOAD_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub type ProgressCallback = Arc<dyn Fn(DownloadProgress) + Send + Sync>;

struct ActiveDownload {
    token: CancellationToken,
    progress: Arc<AtomicU8>,
}

fn partial_path(path: &Path) -> PathBuf {
    path.with_extension("gguf.part")
}

pub struct FunAsrEngine {
    owned_models_root: PathBuf,
    official_models_root: Option<PathBuf>,
    download_base_url: Option<String>,
    current_model: RwLock<Option<String>>,
    sidecar: Mutex<Option<FunAsrSidecar>>,
    model_lifecycle_lock: Mutex<()>,
    cancellation_tokens: Mutex<HashMap<String, ActiveDownload>>,
    encoder_download_lock: Mutex<()>,
    decoder_download_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl FunAsrEngine {
    pub fn new(owned_models_root: PathBuf, official_models_root: Option<PathBuf>) -> Result<Self> {
        std::fs::create_dir_all(owned_models_root.join(MODEL_DIR).join("decoders"))?;
        Ok(Self {
            owned_models_root,
            official_models_root,
            download_base_url: None,
            current_model: RwLock::new(None),
            sidecar: Mutex::new(None),
            model_lifecycle_lock: Mutex::new(()),
            cancellation_tokens: Mutex::new(HashMap::new()),
            encoder_download_lock: Mutex::new(()),
            decoder_download_locks: Mutex::new(HashMap::new()),
        })
    }

    /// A separate inference session shares model files, not model state or a helper.
    pub(crate) fn new_session(&self) -> Result<Self> {
        Self::new(
            self.owned_models_root.clone(),
            self.official_models_root.clone(),
        )
    }

    pub async fn load_selected_model(&self, model_id: &str) -> Result<()> {
        self.load_model(model_id).await.map_err(|error| anyhow!(
            "Could not load selected FUN-ASR model '{model_id}': {error}. Download this model or select another model in Settings."
        ))
    }

    fn download_url(&self, artifact: ArtifactSpec) -> String {
        self.download_base_url
            .as_ref()
            .map(|base| format!("{}/{}", base.trim_end_matches('/'), artifact.filename))
            .unwrap_or_else(|| artifact_url(artifact))
    }

    pub fn owned_directory(&self) -> PathBuf {
        self.owned_models_root.join(MODEL_DIR)
    }

    fn resolve_for_read(&self, relative: &Path) -> PathBuf {
        let owned = self.owned_models_root.join(relative);
        if owned.is_file() {
            return owned;
        }
        if let Some(official) = &self.official_models_root {
            let candidate = official.join(relative);
            if candidate.is_file() {
                return candidate;
            }
        }
        owned
    }

    fn encoder_path(&self) -> PathBuf {
        self.resolve_for_read(&encoder_relative_path())
    }

    fn decoder_path(&self, model_id: &str) -> Result<PathBuf> {
        let spec =
            model_spec(model_id).ok_or_else(|| anyhow!("Unknown FUN-ASR model: {model_id}"))?;
        Ok(self.resolve_for_read(&decoder_relative_path(spec)))
    }

    /// A completed branch encoder can outlive its last decoder if deletion
    /// overlaps another download. Keep it reachable for cleanup, but never
    /// treat an official encoder or one needed by a decoder as an orphan.
    fn has_orphaned_encoder_files(&self) -> bool {
        self.owned_models_root
            .join(encoder_relative_path())
            .is_file()
            && !MODELS.iter().any(|spec| {
                self.resolve_for_read(&decoder_relative_path(spec))
                    .is_file()
                    || partial_path(&self.owned_models_root.join(decoder_relative_path(spec)))
                        .is_file()
            })
    }

    pub async fn discover_models(&self) -> Vec<ModelInfo> {
        let encoder_ready = validate_artifact(&self.encoder_path(), ENCODER, false)
            .await
            .is_ok();
        let active: HashMap<String, u8> = self
            .cancellation_tokens
            .lock()
            .await
            .iter()
            .map(|(name, download)| (name.clone(), download.progress.load(Ordering::Relaxed)))
            .collect();
        let encoder_partial =
            partial_path(&self.owned_models_root.join(encoder_relative_path())).is_file();
        let has_orphaned_encoder = active.is_empty()
            && self.get_current_model().await.is_none()
            && self.has_orphaned_encoder_files();
        let mut models = Vec::with_capacity(MODELS.len());
        for spec in MODELS {
            let decoder_path = self.resolve_for_read(&decoder_relative_path(spec));
            let decoder_ready = validate_artifact(&decoder_path, spec.decoder, false)
                .await
                .is_ok();
            models.push(ModelInfo {
                name: spec.id.to_string(),
                display_name: spec.display_name.to_string(),
                path: decoder_path,
                encoder_available: encoder_ready,
                has_orphaned_encoder,
                has_partial_download: encoder_partial
                    || partial_path(&self.owned_models_root.join(decoder_relative_path(spec)))
                        .is_file(),
                size_mb: (ENCODER.size + spec.decoder.size) / 1_000_000,
                decoder_size_mb: spec.decoder.size / 1_000_000,
                encoder_size_mb: ENCODER.size / 1_000_000,
                description: spec.description.to_string(),
                recommended: spec.id == super::model::DEFAULT_FUNASR_MODEL,
                status: if let Some(progress) = active.get(spec.id) {
                    ModelStatus::Downloading {
                        progress: *progress,
                    }
                } else if encoder_ready && decoder_ready {
                    ModelStatus::Available
                } else {
                    ModelStatus::Missing
                },
            });
        }
        models
    }

    pub async fn validate_model(&self, model_id: &str) -> Result<(PathBuf, PathBuf)> {
        let spec =
            model_spec(model_id).ok_or_else(|| anyhow!("Unknown FUN-ASR model: {model_id}"))?;
        let encoder = self.encoder_path();
        let decoder = self.decoder_path(model_id)?;
        // Downloads verify the full SHA-256 before a file is installed. Loading
        // checks size and GGUF header only: re-hashing about 950 MB on every
        // recording start and Enhance run took seconds each time, and the
        // Whisper and Parakeet engines don't re-hash on load either.
        validate_artifact(&encoder, ENCODER, false)
            .await
            .context("FUN-ASR encoder validation failed")?;
        validate_artifact(&decoder, spec.decoder, false)
            .await
            .with_context(|| format!("FUN-ASR decoder validation failed for {model_id}"))?;
        Ok((encoder, decoder))
    }

    pub async fn load_model(&self, model_id: &str) -> Result<()> {
        let _lifecycle_guard = self.model_lifecycle_lock.lock().await;
        let already_selected = self.current_model.read().await.as_deref() == Some(model_id);
        if already_selected && self.sidecar.lock().await.is_some() {
            return Ok(());
        }

        let (encoder, decoder) = self.validate_model(model_id).await?;
        self.unload_model_inner().await;
        let sidecar = FunAsrSidecar::spawn(&encoder, &decoder).await?;
        *self.sidecar.lock().await = Some(sidecar);
        *self.current_model.write().await = Some(model_id.to_string());
        Ok(())
    }

    pub async fn unload_model(&self) -> bool {
        let _lifecycle_guard = self.model_lifecycle_lock.lock().await;
        self.unload_model_inner().await
    }

    async fn unload_model_inner(&self) -> bool {
        let mut sidecar = self.sidecar.lock().await;
        let had_model = sidecar.is_some();
        if let Some(process) = sidecar.as_mut() {
            process.shutdown().await;
        }
        *sidecar = None;
        *self.current_model.write().await = None;
        had_model
    }

    pub async fn is_model_loaded(&self) -> bool {
        self.sidecar.lock().await.is_some()
    }

    pub async fn get_current_model(&self) -> Option<String> {
        self.current_model.read().await.clone()
    }

    pub async fn transcribe(&self, audio: Vec<f32>, language: &str) -> Result<String> {
        let model_id = self
            .get_current_model()
            .await
            .ok_or_else(|| anyhow!("No FUN-ASR model is loaded"))?;

        let first_attempt = {
            let mut sidecar = self.sidecar.lock().await;
            let process = sidecar
                .as_mut()
                .ok_or_else(|| anyhow!("FUN-ASR helper is not running"))?;
            process.transcribe(language, &audio).await
        };
        let first_error = match first_attempt {
            Ok(text) => return Ok(text),
            Err(error) => error,
        };
        if first_error.response_received {
            // The helper may have processed this request, so don't retry it.
            // If its reply stream is out of step, every later request would
            // misread leftover bytes, so restart the helper for the next one.
            if !first_error.stream_in_sync {
                log::warn!(
                    "FUN-ASR helper reply stream is out of step; restarting it: {first_error}"
                );
                self.unload_model().await;
                if let Err(error) = self.load_model(&model_id).await {
                    log::error!("Failed to restart the FUN-ASR helper: {error}");
                }
            }
            return Err(anyhow!(first_error));
        }
        log::warn!(
            "FUN-ASR helper ended before responding; restarting once before retry: {}",
            first_error
        );
        self.unload_model().await;
        self.load_model(&model_id).await?;
        let mut sidecar = self.sidecar.lock().await;
        sidecar
            .as_mut()
            .ok_or_else(|| anyhow!("FUN-ASR helper restart failed"))?
            .transcribe(language, &audio)
            .await
            .with_context(|| format!("FUN-ASR retry failed after: {first_error}"))
    }

    pub async fn download_model(
        &self,
        model_id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        let spec =
            *model_spec(model_id).ok_or_else(|| anyhow!("Unknown FUN-ASR model: {model_id}"))?;
        let token = CancellationToken::new();
        let latest_progress = Arc::new(AtomicU8::new(0));
        let callback_progress = latest_progress.clone();
        let callback: ProgressCallback = Arc::new(move |update| {
            callback_progress.store(update.percent, Ordering::Relaxed);
            if let Some(callback) = &progress {
                callback(update);
            }
        });
        {
            let mut tokens = self.cancellation_tokens.lock().await;
            if tokens.contains_key(model_id) {
                return Err(anyhow!(
                    "A download for FUN-ASR model '{model_id}' is already in progress"
                ));
            }
            tokens.insert(
                model_id.to_string(),
                ActiveDownload {
                    token: token.clone(),
                    progress: latest_progress,
                },
            );
        }

        let decoder_lock = {
            let mut locks = self.decoder_download_locks.lock().await;
            locks
                .entry(model_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _decoder_guard = decoder_lock.lock().await;

        let encoder_owned = self.owned_models_root.join(encoder_relative_path());
        let decoder_owned = self.owned_models_root.join(decoder_relative_path(&spec));
        let encoder_missing = validate_artifact(&self.encoder_path(), ENCODER, false)
            .await
            .is_err();
        let decoder_missing = validate_artifact(&self.decoder_path(model_id)?, spec.decoder, false)
            .await
            .is_err();
        let total_bytes = if encoder_missing { ENCODER.size } else { 0 }
            + if decoder_missing {
                spec.decoder.size
            } else {
                0
            };
        let started = Instant::now();
        let downloaded = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let result = async {
            if token.is_cancelled() {
                return Err(anyhow!("FUN-ASR model download cancelled"));
            }
            if encoder_missing {
                let _encoder_guard = self.encoder_download_lock.lock().await;
                if token.is_cancelled() {
                    return Err(anyhow!("FUN-ASR model download cancelled"));
                }
                if validate_artifact(&self.encoder_path(), ENCODER, false)
                    .await
                    .is_err()
                {
                    self.download_artifact(
                        model_id,
                        "encoder",
                        ENCODER,
                        &encoder_owned,
                        &token,
                        total_bytes,
                        downloaded.clone(),
                        started,
                        Some(callback.clone()),
                    )
                    .await?;
                } else {
                    downloaded.fetch_add(ENCODER.size, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if decoder_missing {
                if token.is_cancelled() {
                    return Err(anyhow!("FUN-ASR model download cancelled"));
                }
                self.download_artifact(
                    model_id,
                    "decoder",
                    spec.decoder,
                    &decoder_owned,
                    &token,
                    total_bytes,
                    downloaded.clone(),
                    started,
                    Some(callback.clone()),
                )
                .await?;
            }
            Ok(())
        }
        .await;

        self.cancellation_tokens.lock().await.remove(model_id);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_artifact(
        &self,
        model_id: &str,
        phase: &str,
        artifact: ArtifactSpec,
        destination: &Path,
        token: &CancellationToken,
        total_bytes: u64,
        downloaded_before: Arc<std::sync::atomic::AtomicU64>,
        started: Instant,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).await?;
        }
        if destination.is_file() {
            // A download reaches this path only when the owned artifact failed
            // validation. Remove it before the final atomic rename so Windows
            // never has to replace an existing destination.
            fs::remove_file(destination).await?;
        }
        let part = partial_path(destination);
        let mut resume_at = fs::metadata(&part)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if resume_at > artifact.size {
            fs::remove_file(&part).await?;
            resume_at = 0;
        }
        if resume_at == artifact.size {
            match validate_artifact(&part, artifact, true).await {
                Ok(()) => {
                    fs::rename(&part, destination).await?;
                    return Ok(());
                }
                Err(_) => {
                    fs::remove_file(&part).await?;
                    resume_at = 0;
                }
            }
        }

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()?;
        let mut request = client.get(self.download_url(artifact));
        if resume_at > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_at}-"));
        }
        // A server can accept the connection without ever sending headers.
        // Bound that wait too, and let Cancel stop it before any body arrives.
        let response = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(anyhow!("FUN-ASR model download cancelled")),
            response = tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, request.send()) => {
                response.map_err(|_| anyhow!(
                    "FUN-ASR model download stalled: no response headers for {} seconds",
                    DOWNLOAD_STALL_TIMEOUT.as_secs()
                ))??.error_for_status()?
            },
        };
        let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if resume_at > 0 && !resumed {
            resume_at = 0;
        }
        downloaded_before.fetch_add(resume_at, std::sync::atomic::Ordering::Relaxed);

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .await?;
        let mut stream = response.bytes_stream();
        loop {
            // Wait on cancellation and a stall timeout too, so Cancel works and
            // a dead connection fails instead of blocking forever.
            let chunk = tokio::select! {
                _ = token.cancelled() => return Err(anyhow!("FUN-ASR model download cancelled")),
                next = tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, stream.next()) => match next {
                    Ok(Some(chunk)) => chunk?,
                    Ok(None) => break,
                    Err(_) => return Err(anyhow!(
                        "FUN-ASR model download stalled: no data for {} seconds",
                        DOWNLOAD_STALL_TIMEOUT.as_secs()
                    )),
                },
            };
            file.write_all(&chunk).await?;
            let total = downloaded_before
                .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed)
                + chunk.len() as u64;
            if let Some(callback) = &progress {
                let elapsed = started.elapsed().as_secs_f64().max(0.001);
                callback(DownloadProgress {
                    model_name: model_id.to_string(),
                    phase: phase.to_string(),
                    downloaded_bytes: total.min(total_bytes),
                    total_bytes,
                    downloaded_mb: total.min(total_bytes) as f64 / 1_048_576.0,
                    total_mb: total_bytes as f64 / 1_048_576.0,
                    speed_mbps: total as f64 / 1_048_576.0 / elapsed,
                    percent: if total_bytes == 0 {
                        100
                    } else {
                        ((total.min(total_bytes) * 100) / total_bytes) as u8
                    },
                });
            }
        }
        file.flush().await?;
        drop(file);
        validate_artifact(&part, artifact, true).await?;
        fs::rename(&part, destination).await?;
        Ok(())
    }

    pub async fn cancel_download(&self, model_id: &str) -> bool {
        if let Some(download) = self.cancellation_tokens.lock().await.get(model_id) {
            download.token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn delete_model(&self, model_id: &str) -> Result<()> {
        let spec =
            model_spec(model_id).ok_or_else(|| anyhow!("Unknown FUN-ASR model: {model_id}"))?;
        if self.cancellation_tokens.lock().await.contains_key(model_id) {
            return Err(anyhow!(
                "Cancel the download and wait for it to stop before deleting its files."
            ));
        }
        let decoder_lock = {
            let mut locks = self.decoder_download_locks.lock().await;
            locks
                .entry(model_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _decoder_guard = decoder_lock.lock().await;
        if self.cancellation_tokens.lock().await.contains_key(model_id) {
            return Err(anyhow!(
                "Cancel the download and wait for it to stop before deleting its files."
            ));
        }
        // All deletion paths are branch-owned, including partial artifacts.
        let decoder = self.owned_models_root.join(decoder_relative_path(spec));
        let decoder_partial = partial_path(&decoder);
        let encoder = self.owned_models_root.join(encoder_relative_path());
        let encoder_partial = partial_path(&encoder);
        let encoder_only_cleanup =
            !decoder.is_file() && !decoder_partial.is_file() && !encoder_partial.is_file();
        if encoder_only_cleanup {
            // Recheck under both locks before removing an encoder-only orphan.
            // A new download must not register until cleanup has finished.
            if self.has_orphaned_encoder_files() {
                let _encoder_guard = self.encoder_download_lock.try_lock().map_err(|_| {
                    anyhow!("The shared encoder is in use. Try cleanup after the download stops.")
                })?;
                let downloads = self.cancellation_tokens.lock().await;
                let _lifecycle_guard = self.model_lifecycle_lock.try_lock().map_err(|_| {
                    anyhow!("The shared encoder is in use. Try cleanup after the model stops.")
                })?;
                if !downloads.is_empty()
                    || self.current_model.read().await.is_some()
                    || !self.has_orphaned_encoder_files()
                {
                    return Err(anyhow!("The shared encoder is in use. Try cleanup after the model or download stops."));
                }
                fs::remove_file(&encoder).await?;
                return Ok(());
            }

            let official_decoder = self
                .official_models_root
                .as_ref()
                .map(|root| root.join(decoder_relative_path(spec)));
            if official_decoder.as_ref().is_some_and(|path| path.is_file()) {
                return Err(anyhow!(
                    "{} is provided by the official Meetily installation and cannot be deleted from this app.",
                    spec.display_name
                ));
            }
            return Err(anyhow!(
                "{} is not installed in this app's model folder.",
                spec.display_name
            ));
        }
        if self.current_model.read().await.as_deref() == Some(model_id) {
            self.unload_model().await;
        }
        for file in [&decoder, &decoder_partial] {
            if file.is_file() {
                fs::remove_file(file).await?;
            }
        }
        // A different model can be downloading the shared encoder. Leave it
        // alone rather than wait for that transfer to finish.
        let Ok(_encoder_guard) = self.encoder_download_lock.try_lock() else {
            return Ok(());
        };
        let any_owned_decoder = MODELS.iter().any(|candidate| {
            let path = self
                .owned_models_root
                .join(decoder_relative_path(candidate));
            path.is_file() || partial_path(&path).is_file()
        });
        // Hold the registration lock until removal finishes. A new download
        // must see the resulting file state when it decides what to fetch.
        let downloads = self.cancellation_tokens.lock().await;
        if !any_owned_decoder && downloads.is_empty() {
            for file in [&encoder, &encoder_partial] {
                if file.is_file() {
                    fs::remove_file(file).await?;
                }
            }
        }
        Ok(())
    }
}

async fn validate_artifact(path: &Path, spec: ArtifactSpec, hash: bool) -> Result<()> {
    let metadata = fs::metadata(path)
        .await
        .with_context(|| format!("Missing model file {}", path.display()))?;
    if metadata.len() != spec.size {
        return Err(anyhow!(
            "Invalid size for {}: expected {}, got {}",
            path.display(),
            spec.size,
            metadata.len()
        ));
    }
    let mut file = fs::File::open(path).await?;
    let mut magic = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(&mut file, &mut magic).await?;
    if &magic != b"GGUF" {
        return Err(anyhow!("{} is not a GGUF model", path.display()));
    }
    drop(file);
    if hash {
        let path = path.to_path_buf();
        let hash_path = path.clone();
        let actual = tokio::task::spawn_blocking(move || -> Result<String> {
            let mut file = std::fs::File::open(&hash_path)?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(format!("{:x}", hasher.finalize()))
        })
        .await??;
        if actual != spec.sha256 {
            return Err(anyhow!(
                "Checksum mismatch for {}: expected {}, got {}",
                path.display(),
                spec.sha256,
                actual
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn write_file(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    async fn mock_artifact_server(body: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..count]).to_string();
            let offset = request
                .lines()
                .find_map(|line| {
                    line.strip_prefix("range: bytes=")
                        .or_else(|| line.strip_prefix("Range: bytes="))
                })
                .and_then(|value| value.trim_end_matches('-').parse::<usize>().ok())
                .unwrap_or(0);
            let partial = offset > 0;
            let response_body = &body[offset.min(body.len())..];
            let mut headers = format!(
                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                if partial { 206 } else { 200 },
                if partial { "Partial Content" } else { "OK" },
                response_body.len()
            );
            if partial {
                headers.push_str(&format!(
                    "Content-Range: bytes {}-{}/{}\r\n",
                    offset,
                    body.len().saturating_sub(1),
                    body.len()
                ));
            }
            headers.push_str("\r\n");
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(response_body).await.unwrap();
            socket.shutdown().await.unwrap();
            request
        });
        (format!("http://{address}"), handle)
    }

    fn tiny_artifact() -> ArtifactSpec {
        ArtifactSpec {
            filename: "model.gguf",
            size: 8,
            sha256: "30be5a60b7786b878803d49ff1dcaa157e9c098b896ff8bca692104fa9df3ba0",
        }
    }

    async fn download_tiny_artifact(
        engine: &FunAsrEngine,
        destination: &Path,
        token: &CancellationToken,
    ) -> Result<()> {
        engine
            .download_artifact(
                "test-model",
                "decoder",
                tiny_artifact(),
                destination,
                token,
                tiny_artifact().size,
                Arc::new(std::sync::atomic::AtomicU64::new(0)),
                Instant::now(),
                None,
            )
            .await
    }

    #[tokio::test]
    async fn a_missing_selected_model_does_not_load_an_installed_variant() {
        let owned = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        // Sparse GGUF fixtures pass discovery without downloading real models.
        for (path, size) in [
            (owned.path().join(encoder_relative_path()), ENCODER.size),
            (
                owned.path().join(decoder_relative_path(&MODELS[1])),
                MODELS[1].decoder.size,
            ),
        ] {
            write_file(&path, b"GGUF");
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(size)
                .unwrap();
        }
        assert!(matches!(
            engine.discover_models().await[1].status,
            ModelStatus::Available
        ));
        let error = engine.load_selected_model(MODELS[0].id).await.unwrap_err();
        assert!(error.to_string().contains(MODELS[0].id));
        assert_eq!(engine.get_current_model().await, None);
    }

    #[tokio::test]
    async fn batch_session_model_changes_and_cleanup_do_not_change_the_recording_session() {
        let owned = tempfile::tempdir().unwrap();
        let recording = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        *recording.current_model.write().await = Some(MODELS[0].id.to_string());
        let batch = recording.new_session().unwrap();
        assert_eq!(batch.get_current_model().await, None);
        *batch.current_model.write().await = Some(MODELS[1].id.to_string());
        batch.unload_model().await;
        assert_eq!(
            recording.get_current_model().await.as_deref(),
            Some(MODELS[0].id)
        );
        assert_eq!(batch.owned_directory(), recording.owned_directory());
    }

    #[tokio::test]
    async fn discovery_keeps_an_active_download_visible_and_cancelled_files_can_be_deleted() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 469331008\r\n\r\nGGUF")
                .await
                .unwrap();
            std::future::pending::<()>().await;
            drop(socket);
        });
        let owned = tempfile::tempdir().unwrap();
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(format!("http://{address}"));
        let engine = Arc::new(engine);
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let download_engine = engine.clone();
        let download = tokio::spawn(async move {
            download_engine
                .download_model(
                    MODELS[0].id,
                    Some(Arc::new(move |progress| {
                        sent.send(progress).unwrap();
                    })),
                )
                .await
        });
        let progress = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        let models = engine.discover_models().await;
        assert!(
            matches!(models[0].status, ModelStatus::Downloading { progress: p } if p == progress.percent)
        );
        assert!(models[0].has_partial_download);
        // Deletion must not wait forever on an active download.
        let delete = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.delete_model(MODELS[0].id),
        )
        .await;
        assert!(delete.is_ok(), "delete waited for an active transfer");
        assert!(delete.unwrap().unwrap_err().to_string().contains("Cancel"));
        assert!(engine.cancel_download(MODELS[0].id).await);
        assert!(download
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(matches!(
            engine.discover_models().await[0].status,
            ModelStatus::Missing
        ));
        engine.delete_model(MODELS[0].id).await.unwrap();
        assert!(!partial_path(&owned.path().join(encoder_relative_path())).exists());
        server.abort();
    }

    #[tokio::test]
    async fn partial_cleanup_keeps_files_needed_by_other_variants_and_never_changes_official_files()
    {
        let owned = tempfile::tempdir().unwrap();
        let official = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(
            owned.path().to_path_buf(),
            Some(official.path().to_path_buf()),
        )
        .unwrap();
        let encoder = partial_path(&owned.path().join(encoder_relative_path()));
        let first = partial_path(&owned.path().join(decoder_relative_path(&MODELS[0])));
        let second = partial_path(&owned.path().join(decoder_relative_path(&MODELS[1])));
        let official_decoder = official.path().join(decoder_relative_path(&MODELS[0]));
        write_file(&encoder, b"encoder part");
        write_file(&first, b"first part");
        write_file(&second, b"second part");
        write_file(&official_decoder, b"official");
        engine.delete_model(MODELS[0].id).await.unwrap();
        assert!(!first.exists());
        assert!(second.is_file());
        assert!(encoder.is_file());
        engine.delete_model(MODELS[1].id).await.unwrap();
        assert!(!second.exists());
        assert!(!encoder.exists());
        assert_eq!(std::fs::read(&official_decoder).unwrap(), b"official");
    }

    #[tokio::test]
    async fn encoder_left_after_a_failed_concurrent_download_is_discoverable_and_deletable() {
        let owned = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        let encoder = owned.path().join(encoder_relative_path());
        write_file(&encoder, b"GGUF");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&encoder)
            .unwrap()
            .set_len(ENCODER.size)
            .unwrap();
        let decoder = owned.path().join(decoder_relative_path(&MODELS[0]));
        write_file(&decoder, b"decoder");
        let encoder_guard = engine.encoder_download_lock.lock().await;
        engine.cancellation_tokens.lock().await.insert(
            MODELS[1].id.to_string(),
            ActiveDownload {
                token: CancellationToken::new(),
                progress: Arc::new(AtomicU8::new(0)),
            },
        );
        engine.delete_model(MODELS[0].id).await.unwrap();
        assert!(!decoder.exists());
        assert!(encoder.is_file());
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
        // The second transfer fails without creating a decoder or partial file.
        drop(encoder_guard);
        engine.cancellation_tokens.lock().await.remove(MODELS[1].id);
        let models = engine.discover_models().await;
        assert!(models
            .iter()
            .all(|model| matches!(model.status, ModelStatus::Missing)
                && model.has_orphaned_encoder
                && !model.has_partial_download
                && model.encoder_available));
        engine.delete_model(MODELS[0].id).await.unwrap();
        assert!(!encoder.exists());
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
    }

    #[tokio::test]
    async fn encoder_only_cleanup_is_blocked_if_a_download_or_loaded_model_needs_it() {
        let owned = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        let encoder = owned.path().join(encoder_relative_path());
        write_file(&encoder, b"encoder");
        engine.cancellation_tokens.lock().await.insert(
            MODELS[1].id.to_string(),
            ActiveDownload {
                token: CancellationToken::new(),
                progress: Arc::new(AtomicU8::new(0)),
            },
        );
        assert!(engine
            .delete_model(MODELS[0].id)
            .await
            .unwrap_err()
            .to_string()
            .contains("in use"));
        assert!(encoder.is_file());
        engine.cancellation_tokens.lock().await.clear();
        let model_guard = engine.model_lifecycle_lock.lock().await;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.delete_model(MODELS[0].id),
        )
        .await
        .expect("cleanup waited for a model operation");
        assert!(result.unwrap_err().to_string().contains("in use"));
        drop(model_guard);
        *engine.current_model.write().await = Some(MODELS[1].id.to_string());
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
        assert!(engine
            .delete_model(MODELS[0].id)
            .await
            .unwrap_err()
            .to_string()
            .contains("in use"));
        assert!(encoder.is_file());
    }

    #[tokio::test]
    async fn orphan_detection_never_offers_cleanup_for_official_or_needed_encoder_files() {
        let owned = tempfile::tempdir().unwrap();
        let official = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(
            owned.path().to_path_buf(),
            Some(official.path().to_path_buf()),
        )
        .unwrap();
        let official_encoder = official.path().join(encoder_relative_path());
        write_file(&official_encoder, b"official encoder");
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
        assert!(engine.delete_model(MODELS[0].id).await.is_err());
        assert!(official_encoder.is_file());
        let encoder = owned.path().join(encoder_relative_path());
        write_file(&encoder, b"owned encoder");
        let official_decoder = official.path().join(decoder_relative_path(&MODELS[1]));
        write_file(&official_decoder, b"official decoder");
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
        assert!(engine.delete_model(MODELS[0].id).await.is_err());
        assert!(encoder.is_file());
        assert!(official_decoder.is_file());
        std::fs::remove_file(&official_decoder).unwrap();
        let decoder_partial = partial_path(&owned.path().join(decoder_relative_path(&MODELS[1])));
        write_file(&decoder_partial, b"decoder part");
        assert!(engine
            .discover_models()
            .await
            .iter()
            .all(|model| !model.has_orphaned_encoder));
        assert!(engine.delete_model(MODELS[0].id).await.is_err());
        assert!(encoder.is_file());
        assert!(decoder_partial.is_file());
    }

    #[tokio::test]
    async fn artifact_validation_checks_header_size_and_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.gguf");
        let spec = ArtifactSpec {
            filename: "model.gguf",
            size: 8,
            sha256: "30be5a60b7786b878803d49ff1dcaa157e9c098b896ff8bca692104fa9df3ba0",
        };

        write_file(&path, b"GGUFtest");
        validate_artifact(&path, spec, true).await.unwrap();

        write_file(&path, b"NOPEtest");
        assert!(validate_artifact(&path, spec, false).await.is_err());

        write_file(&path, b"GGUFfail");
        assert!(validate_artifact(&path, spec, true).await.is_err());
    }

    #[tokio::test]
    async fn download_completes_atomically_from_a_mock_server() {
        let owned = tempfile::tempdir().unwrap();
        let (base_url, server) = mock_artifact_server(b"GGUFtest".to_vec()).await;
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(base_url);
        let destination = owned.path().join("model.gguf");

        download_tiny_artifact(&engine, &destination, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"GGUFtest");
        assert!(!owned.path().join("model.gguf.part").exists());
        assert!(server.await.unwrap().starts_with("GET /model.gguf "));
    }

    #[tokio::test]
    async fn download_resumes_a_partial_file_with_an_http_range() {
        let owned = tempfile::tempdir().unwrap();
        let (base_url, server) = mock_artifact_server(b"GGUFtest".to_vec()).await;
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(base_url);
        let destination = owned.path().join("model.gguf");
        write_file(&owned.path().join("model.gguf.part"), b"GGUF");

        download_tiny_artifact(&engine, &destination, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"GGUFtest");
        assert!(server.await.unwrap().contains("bytes=4-"));
    }

    #[tokio::test]
    async fn cancelled_download_keeps_a_resumable_part_file() {
        let owned = tempfile::tempdir().unwrap();
        write_file(&owned.path().join("model.gguf.part"), b"GGUF");
        let (base_url, server) = mock_artifact_server(b"GGUFtest".to_vec()).await;
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(base_url);
        let destination = owned.path().join("model.gguf");
        let token = CancellationToken::new();
        token.cancel();

        let error = download_tiny_artifact(&engine, &destination, &token)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(!destination.exists());
        assert!(owned.path().join("model.gguf.part").exists());
        assert_eq!(
            std::fs::read(owned.path().join("model.gguf.part")).unwrap(),
            b"GGUF"
        );
        server.abort();
    }

    #[tokio::test]
    async fn cancelling_before_response_headers_returns_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (received, accepted) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            socket.read(&mut request).await.unwrap();
            received.send(()).unwrap();
            // Keep the connection open without sending response headers.
            std::future::pending::<()>().await;
            drop(socket);
        });
        let owned = tempfile::tempdir().unwrap();
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(format!("http://{address}"));
        let destination = owned.path().join("model.gguf");
        let token = CancellationToken::new();
        let canceller = token.clone();
        let cancel = tokio::spawn(async move {
            accepted.await.unwrap();
            canceller.cancel();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            download_tiny_artifact(&engine, &destination, &token),
        )
        .await;
        server.abort();
        cancel.abort();
        let error = result
            .expect("Cancel waited for the response headers")
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn cancelling_a_stalled_download_returns_promptly() {
        // A server that sends headers and part of the body, then goes silent.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nGGUF")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        let owned = tempfile::tempdir().unwrap();
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(format!("http://{address}"));
        let destination = owned.path().join("model.gguf");
        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            canceller.cancel();
        });

        let started = Instant::now();
        let error = download_tiny_artifact(&engine, &destination, &token)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "cancel waited on the stalled stream"
        );
        assert!(!destination.exists());
        server.abort();
    }

    #[tokio::test]
    async fn checksum_mismatch_never_replaces_the_destination() {
        let owned = tempfile::tempdir().unwrap();
        let (base_url, server) = mock_artifact_server(b"GGUFfail".to_vec()).await;
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(base_url);
        let destination = owned.path().join("model.gguf");

        let error = download_tiny_artifact(&engine, &destination, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Checksum mismatch"));
        assert!(!destination.exists());
        assert!(owned.path().join("model.gguf.part").exists());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_gguf_never_replaces_the_destination() {
        let owned = tempfile::tempdir().unwrap();
        let (base_url, server) = mock_artifact_server(b"NOPEtest".to_vec()).await;
        let mut engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        engine.download_base_url = Some(base_url);
        let destination = owned.path().join("model.gguf");

        let error = download_tiny_artifact(&engine, &destination, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a GGUF"));
        assert!(!destination.exists());
        assert!(owned.path().join("model.gguf.part").exists());
        server.await.unwrap();
    }

    #[test]
    fn owned_models_take_precedence_and_official_models_are_fallback_only() {
        let owned = tempfile::tempdir().unwrap();
        let official = tempfile::tempdir().unwrap();
        let relative = decoder_relative_path(&MODELS[0]);
        let official_file = official.path().join(&relative);
        write_file(&official_file, b"official");

        let engine = FunAsrEngine::new(
            owned.path().to_path_buf(),
            Some(official.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(engine.resolve_for_read(&relative), official_file);

        let owned_file = owned.path().join(&relative);
        write_file(&owned_file, b"owned");
        assert_eq!(engine.resolve_for_read(&relative), owned_file);
    }

    #[tokio::test]
    async fn shared_encoder_is_removed_only_after_the_last_owned_decoder() {
        let owned = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        let encoder = owned.path().join(encoder_relative_path());
        let first_decoder = owned.path().join(decoder_relative_path(&MODELS[0]));
        let second_decoder = owned.path().join(decoder_relative_path(&MODELS[1]));
        write_file(&encoder, b"encoder");
        write_file(&first_decoder, b"first");
        write_file(&second_decoder, b"second");

        engine.delete_model(MODELS[0].id).await.unwrap();
        assert!(encoder.is_file());
        assert!(!first_decoder.exists());
        assert!(second_decoder.is_file());

        engine.delete_model(MODELS[1].id).await.unwrap();
        assert!(!encoder.exists());
        assert!(!second_decoder.exists());
    }

    #[tokio::test]
    async fn deletion_never_touches_official_fallback_files() {
        let owned = tempfile::tempdir().unwrap();
        let official = tempfile::tempdir().unwrap();
        let fallback_decoder = official.path().join(decoder_relative_path(&MODELS[0]));
        let fallback_encoder = official.path().join(encoder_relative_path());
        write_file(&fallback_decoder, b"official decoder");
        write_file(&fallback_encoder, b"official encoder");
        let engine = FunAsrEngine::new(
            owned.path().to_path_buf(),
            Some(official.path().to_path_buf()),
        )
        .unwrap();

        let error = engine.delete_model(MODELS[0].id).await.unwrap_err();
        assert!(
            error.to_string().contains("official Meetily installation"),
            "unexpected error: {error}"
        );
        assert!(fallback_decoder.is_file());
        assert!(fallback_encoder.is_file());
    }

    #[tokio::test]
    async fn deleting_a_model_that_is_not_installed_is_an_error() {
        let owned = tempfile::tempdir().unwrap();
        let engine = FunAsrEngine::new(owned.path().to_path_buf(), None).unwrap();
        let error = engine.delete_model(MODELS[0].id).await.unwrap_err();
        assert!(
            error.to_string().contains("not installed"),
            "unexpected error: {error}"
        );
    }
}
