// diarization/models.rs
//
// Model location and download for speaker identification.
// Mirrors the parakeet_engine download pattern: stream from a stable URL
// into <app_data>/models/diarization/, .tmp + rename for atomicity,
// progress emitted as Tauri events.

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Runtime};

/// WeSpeaker CAM++ speaker-embedding model (Apache-2.0, exported to ONNX by
/// the sherpa-onnx project). ~28 MB. Input: fbank [1, T, 80]; output: [1, 512].
/// NOTE: "recongition" is the canonical (misspelled) sherpa-onnx release tag.
pub const EMBEDDING_MODEL_FILENAME: &str = "wespeaker_en_voxceleb_CAM++.onnx";
pub const EMBEDDING_MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_CAM%2B%2B.onnx";
pub const EMBEDDING_MODEL_SHA256: &str =
    "c46fad10b5f81e1aa4a60c162714208577093655076c5450f8c469e522ec54ef";
pub const EMBEDDING_MODEL_SIZE: u64 = 29_292_684;
// Include the feature recipe: voiceprints made before PCM16 scaling cannot be
// compared safely with corrected embeddings, even though the model is unchanged.
pub const EMBEDDING_MODEL_VERSION: &str = "wespeaker-campp-voxceleb-2023-pcm16-v2";

/// Branch-owned diarization model directory. Downloads and deletions use this.
pub fn models_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(crate::model_storage::owned_models_root(app)?.join("diarization"))
}

/// Resolve the embedding model for reading, falling back to the official
/// installation's copy. The returned path must never be used for mutations.
pub fn embedding_model_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let owned_root = crate::model_storage::owned_models_root(app)?;
    let shared_root = crate::model_storage::official_models_root(app)?;
    crate::model_storage::resolve_model_for_read(
        &owned_root,
        shared_root.as_deref(),
        std::path::Path::new("diarization").join(EMBEDDING_MODEL_FILENAME),
    )
}

pub fn is_embedding_model_present<R: Runtime>(app: &AppHandle<R>) -> bool {
    embedding_model_path(app)
        .map(|p| validate_model_file(&p).is_ok())
        .unwrap_or(false)
}

pub fn validate_model_file(path: &std::path::Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Failed to read diarization model: {error}"))?;
    if bytes.len() as u64 != EMBEDDING_MODEL_SIZE {
        return Err(format!(
            "Diarization model size mismatch: expected {EMBEDDING_MODEL_SIZE}, got {}",
            bytes.len()
        ));
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != EMBEDDING_MODEL_SHA256 {
        return Err("Diarization model checksum mismatch".to_string());
    }
    Ok(())
}

/// Download the embedding model, emitting `diarization-model-download-progress`
/// events with { downloaded_bytes, total_bytes, percent }.
pub async fn download_embedding_model<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let dir = models_dir(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create models dir: {}", e))?;

    let final_path = dir.join(EMBEDDING_MODEL_FILENAME);
    if is_embedding_model_present(app) {
        log::info!(
            "Diarization embedding model already present at {}",
            final_path.display()
        );
        return Ok(());
    }
    let tmp_path = dir.join(format!("{}.tmp", EMBEDDING_MODEL_FILENAME));

    log::info!(
        "Downloading diarization embedding model from {}",
        EMBEDDING_MODEL_URL
    );
    // A stalled download would otherwise hold the speaker processing slot
    // until the app restarts. Same limits as the Parakeet downloader.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| format!("Failed to prepare the download: {}", e))?;
    let response = client
        .get(EMBEDDING_MODEL_URL)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}", response.status()));
    }

    let total_bytes = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut last_emitted_percent: i64 = -1;

    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut stream = response.bytes_stream();
    loop {
        let chunk = match tokio::time::timeout(std::time::Duration::from_secs(30), stream.next()).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err("Download stalled: no data received for 30 seconds".to_string());
            }
        };
        let chunk = chunk.map_err(|e| format!("Download stream error: {}", e))?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("Failed to write model file: {}", e))?;
        downloaded += chunk.len() as u64;

        let percent = if total_bytes > 0 {
            (downloaded * 100 / total_bytes) as i64
        } else {
            0
        };
        if percent != last_emitted_percent {
            last_emitted_percent = percent;
            let _ = app.emit(
                "diarization-model-download-progress",
                serde_json::json!({
                    "downloaded_bytes": downloaded,
                    "total_bytes": total_bytes,
                    "percent": percent,
                }),
            );
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| format!("Failed to flush model file: {}", e))?;
    drop(file);

    if let Err(error) = validate_model_file(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }

    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("Failed to finalize model file: {}", e))?;

    log::info!(
        "Diarization embedding model downloaded to {} ({} bytes)",
        final_path.display(),
        downloaded
    );
    Ok(())
}

pub fn delete_embedding_model<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // Only the branch-owned copy is deleted; a shared official copy is read-only.
    let path = models_dir(app)?.join(EMBEDDING_MODEL_FILENAME);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|error| format!("Failed to delete diarization model: {error}"))?;
    }
    let temporary_path = path.with_extension("onnx.tmp");
    if temporary_path.exists() {
        let _ = std::fs::remove_file(temporary_path);
    }
    Ok(())
}
