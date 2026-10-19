use super::engine::{FunAsrEngine, ProgressCallback};
use super::model::{DownloadProgress, ModelInfo, ModelStatus, DEFAULT_FUNASR_MODEL};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub static FUNASR_ENGINE: Mutex<Option<Arc<FunAsrEngine>>> = Mutex::new(None);
static MODELS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static OFFICIAL_MODELS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_models_directory<R: Runtime>(app: &AppHandle<R>) {
    match crate::model_storage::owned_models_root(app) {
        Ok(path) => *MODELS_DIR.lock().unwrap() = Some(path),
        Err(error) => log::error!("Failed to configure FUN-ASR model directory: {error}"),
    }
    *OFFICIAL_MODELS_DIR.lock().unwrap() = crate::model_storage::official_models_root(app)
        .ok()
        .flatten();
}

#[tauri::command]
pub async fn funasr_init() -> Result<(), String> {
    let mut guard = FUNASR_ENGINE.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let models = MODELS_DIR
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "FUN-ASR model directory is not configured".to_string())?;
    let official = OFFICIAL_MODELS_DIR.lock().unwrap().clone();
    let engine = FunAsrEngine::new(models, official).map_err(|error| error.to_string())?;
    *guard = Some(Arc::new(engine));
    Ok(())
}

fn engine() -> Result<Arc<FunAsrEngine>, String> {
    FUNASR_ENGINE
        .lock()
        .unwrap()
        .as_ref()
        .cloned()
        .ok_or_else(|| "FUN-ASR engine is not initialized".to_string())
}

#[tauri::command]
pub async fn funasr_get_available_models() -> Result<Vec<ModelInfo>, String> {
    Ok(engine()?.discover_models().await)
}

#[tauri::command]
pub async fn funasr_get_current_model() -> Result<Option<String>, String> {
    Ok(engine()?.get_current_model().await)
}

#[tauri::command]
pub async fn funasr_is_model_loaded() -> Result<bool, String> {
    Ok(engine()?.is_model_loaded().await)
}

#[tauri::command]
pub async fn funasr_has_available_models() -> Result<bool, String> {
    Ok(engine()?
        .discover_models()
        .await
        .iter()
        .any(|model| matches!(model.status, ModelStatus::Available)))
}

#[tauri::command]
pub async fn funasr_load_model<R: Runtime>(
    app: AppHandle<R>,
    model_name: String,
) -> Result<(), String> {
    app.emit(
        "funasr-model-loading-started",
        serde_json::json!({ "modelName": model_name }),
    )
    .ok();
    let result = engine()?
        .load_model(&model_name)
        .await
        .map_err(|error| error.to_string());
    match &result {
        Ok(()) => {
            app.emit(
                "funasr-model-loading-completed",
                serde_json::json!({ "modelName": model_name }),
            )
            .ok();
        }
        Err(error) => {
            app.emit(
                "funasr-model-loading-failed",
                serde_json::json!({ "modelName": model_name, "error": error }),
            )
            .ok();
        }
    }
    result
}

#[tauri::command]
pub async fn funasr_unload_model() -> Result<bool, String> {
    Ok(engine()?.unload_model().await)
}

#[tauri::command]
pub async fn funasr_validate_model_ready<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    validate_model_ready_with_config(&app).await
}

pub async fn validate_model_ready_with_config<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<String, String> {
    funasr_init().await?;
    let engine = engine()?;
    let configured = crate::api::api::api_get_transcript_config(app.clone(), app.state(), None)
        .await
        .ok()
        .flatten()
        .filter(|config| config.provider == "funasr")
        .map(|config| config.model);

    let available = engine.discover_models().await;
    let selected = configured
        .filter(|model| {
            available.iter().any(|candidate| {
                candidate.name == *model && matches!(candidate.status, ModelStatus::Available)
            })
        })
        .or_else(|| {
            available
                .iter()
                .find(|candidate| {
                    candidate.name == DEFAULT_FUNASR_MODEL
                        && matches!(candidate.status, ModelStatus::Available)
                })
                .map(|candidate| candidate.name.clone())
        })
        .or_else(|| {
            available
                .iter()
                .find(|candidate| matches!(candidate.status, ModelStatus::Available))
                .map(|candidate| candidate.name.clone())
        })
        .ok_or_else(|| {
            "No FUN-ASR models are available. Download a FUN-ASR Nano model first.".to_string()
        })?;
    engine
        .load_model(&selected)
        .await
        .map_err(|error| error.to_string())?;
    Ok(selected)
}

#[tauri::command]
pub async fn funasr_get_models_directory() -> Result<String, String> {
    Ok(engine()?.owned_directory().to_string_lossy().to_string())
}

#[tauri::command]
pub async fn funasr_download_model<R: Runtime>(
    app: AppHandle<R>,
    model_name: String,
) -> Result<(), String> {
    let callback_app = app.clone();
    let callback: ProgressCallback = Arc::new(move |progress: DownloadProgress| {
        callback_app
            .emit(
                "funasr-model-download-progress",
                serde_json::json!({
                    "modelName": progress.model_name,
                    "phase": progress.phase,
                    "progress": progress.percent,
                    "downloaded_bytes": progress.downloaded_bytes,
                    "total_bytes": progress.total_bytes,
                    "downloaded_mb": progress.downloaded_mb,
                    "total_mb": progress.total_mb,
                    "speed_mbps": progress.speed_mbps,
                    "status": "downloading"
                }),
            )
            .ok();
    });
    let result = engine()?
        .download_model(&model_name, Some(callback))
        .await
        .map_err(|error| error.to_string());
    match &result {
        Ok(()) => {
            app.emit(
                "funasr-model-download-complete",
                serde_json::json!({
                    "modelName": model_name,
                    "phase": "complete",
                    "progress": 100,
                    "status": "complete"
                }),
            )
            .ok();
            crate::tray::update_tray_menu(&app);
        }
        Err(error) => {
            let cancelled = error.contains("cancelled");
            app.emit(
                "funasr-model-download-error",
                serde_json::json!({
                    "modelName": model_name,
                    "phase": if cancelled { "cancelled" } else { "error" },
                    "status": if cancelled { "cancelled" } else { "error" },
                    "error": error
                }),
            )
            .ok();
        }
    }
    result
}

#[tauri::command]
pub async fn funasr_cancel_download<R: Runtime>(
    app: AppHandle<R>,
    model_name: String,
) -> Result<(), String> {
    engine()?.cancel_download(&model_name).await;
    app.emit(
        "funasr-model-download-progress",
        serde_json::json!({
            "modelName": model_name,
            "phase": "cancelled",
            "progress": 0,
            "downloaded_bytes": 0,
            "total_bytes": 0,
            "status": "cancelled",
            "cancelled": true
        }),
    )
    .ok();
    Ok(())
}

#[tauri::command]
pub async fn funasr_delete_model(model_name: String) -> Result<String, String> {
    engine()?
        .delete_model(&model_name)
        .await
        .map_err(|error| error.to_string())?;
    Ok(format!("Deleted {model_name}"))
}

#[tauri::command]
pub async fn funasr_transcribe_audio(
    audio_data: Vec<f32>,
    language: Option<String>,
) -> Result<String, String> {
    let language = super::normalize_language(language.as_deref())?;
    engine()?
        .transcribe(audio_data, language)
        .await
        .map_err(|error| error.to_string())
}

pub async fn shutdown() {
    if let Ok(engine) = engine() {
        engine.unload_model().await;
    }
}
