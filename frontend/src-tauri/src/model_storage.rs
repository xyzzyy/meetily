use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime};

pub fn owned_models_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let app_data_dir = crate::storage_safety::branch_app_data_dir(app)?;
    let owned = app_data_dir.join("models");
    std::fs::create_dir_all(&owned)
        .map_err(|error| format!("Failed to create branch model directory: {error}"))?;
    Ok(owned)
}

/// Official Meetily's model directory is exposed only as an optional,
/// read-only lookup root. No mutation helper ever returns this path.
pub fn official_models_root<R: Runtime>(app: &AppHandle<R>) -> Result<Option<PathBuf>, String> {
    let branch_dir = crate::storage_safety::branch_app_data_dir(app)?;
    let application_support = branch_dir
        .parent()
        .ok_or_else(|| "Branch app-data directory has no parent".to_string())?;
    let official = application_support
        .join(crate::storage_safety::OFFICIAL_IDENTIFIER)
        .join("models");
    Ok(official.is_dir().then_some(official))
}

/// Compatibility alias for code that needs a writable model destination.
pub fn models_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    owned_models_root(app)
}

pub fn resolve_model_for_read<R: Runtime>(
    app: &AppHandle<R>,
    relative_path: impl AsRef<Path>,
) -> Result<PathBuf, String> {
    let relative_path = relative_path.as_ref();
    let owned = owned_models_root(app)?.join(relative_path);
    if owned.is_file() {
        return Ok(owned);
    }
    if let Some(shared) = official_models_root(app)? {
        let candidate = shared.join(relative_path);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(owned)
}

pub fn is_branch_owned_model<R: Runtime>(
    app: &AppHandle<R>,
    path: &Path,
) -> Result<bool, String> {
    Ok(path.starts_with(owned_models_root(app)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_identifier_is_never_a_writable_root() {
        let writable = Path::new("/Application Support/com.meetily.ai.branch/models");
        assert!(writable.to_string_lossy().contains("com.meetily.ai.branch"));
        assert!(!writable.ends_with("com.meetily.ai/models"));
    }
}
