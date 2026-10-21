use super::engine::{FunAsrEngine, ProgressCallback};
use super::model::{DownloadProgress, ModelInfo, ModelStatus};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub static FUNASR_ENGINE: Mutex<Option<Arc<FunAsrEngine>>> = Mutex::new(None);
static MODELS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static OFFICIAL_MODELS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Resolve FUN-ASR's model directories: branch-owned for downloads and
/// deletion, and the official installation's copy as a read-only fallback.
pub fn set_models_directory<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let owned = crate::model_storage::owned_models_root(app)?;
    let official = crate::model_storage::official_models_root(app)?;
    *MODELS_DIR.lock().unwrap() = Some(owned);
    *OFFICIAL_MODELS_DIR.lock().unwrap() = official;
    Ok(())
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

/// Require an explicit saved FUN-ASR selection. A missing model or settings
/// failure must not authorize the use of another model.
pub(crate) fn configured_funasr_model(
    result: Result<Option<crate::api::api::TranscriptConfig>, String>,
) -> Result<String, String> {
    match result {
        Ok(Some(config)) if config.provider == "funasr" && !config.model.trim().is_empty() => {
            Ok(config.model)
        }
        Ok(_) => Err("No FUN-ASR model is selected. Select a model in Settings.".to_string()),
        Err(error) => Err(format!(
            "Could not load transcription settings: {}. Select a transcription model in Settings and try again.",
            error
        )),
    }
}

pub async fn validate_model_ready_with_config<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<String, String> {
    funasr_init().await?;
    let engine = engine()?;
    let selected = configured_funasr_model(
        crate::api::api::api_get_transcript_config(app.clone(), app.state(), None).await,
    )?;
    engine
        .load_selected_model(&selected)
        .await
        .map_err(|error| error.to_string())?;
    Ok(selected)
}

/// Batch jobs own a separate helper. They must never load or unload the engine
/// used by a live recording, even when both jobs select the same model.
pub(crate) fn batch_engine() -> Result<Arc<FunAsrEngine>, String> {
    engine()?
        .new_session()
        .map(Arc::new)
        .map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::configured_funasr_model;
    use crate::api::api::TranscriptConfig;

    fn config(provider: &str, model: &str) -> TranscriptConfig {
        TranscriptConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: None,
        }
    }

    #[test]
    fn settings_errors_are_not_treated_as_no_selection() {
        let error = configured_funasr_model(Err("database is locked".to_string())).unwrap_err();
        assert!(error.contains("database is locked"));
        assert!(error.contains("Could not load transcription settings"));
    }

    #[test]
    fn missing_or_other_provider_configs_require_an_explicit_selection() {
        assert!(configured_funasr_model(Ok(None)).is_err());
        assert!(configured_funasr_model(Ok(Some(config("parakeet", "model-a")))).is_err());
        assert!(configured_funasr_model(Ok(Some(config("funasr", "  ")))).is_err());
    }

    #[test]
    fn the_saved_funasr_model_is_returned() {
        assert_eq!(
            configured_funasr_model(Ok(Some(config("funasr", "model-a")))).unwrap(),
            "model-a".to_string()
        );
    }
}
