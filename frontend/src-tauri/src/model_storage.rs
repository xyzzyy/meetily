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
    official_models_root_for_branch_dir(&branch_dir)
}

pub(crate) fn official_models_root_for_branch_dir(
    branch_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    crate::storage_safety::validate_branch_directory(branch_dir)?;
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

/// Resolve a file or model directory for reading, preferring branch-owned data.
/// The returned path may be shared and must never be used for mutations.
pub fn resolve_model_for_read(
    owned_root: &Path,
    shared_root: Option<&Path>,
    relative_path: impl AsRef<Path>,
) -> PathBuf {
    let relative_path = relative_path.as_ref();
    let owned = owned_root.join(relative_path);
    if owned.exists() {
        return owned;
    }
    if let Some(shared) = shared_root {
        let candidate = shared.join(relative_path);
        if candidate.exists() {
            return candidate;
        }
    }
    owned
}

pub fn is_branch_owned_model<R: Runtime>(app: &AppHandle<R>, path: &Path) -> Result<bool, String> {
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
    #[test]
    fn shared_reads_fall_back_and_prefer_owned_files_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("branch/models");
        let shared = temp.path().join("official/models");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::create_dir_all(shared.join("parakeet/model")).unwrap();
        std::fs::write(shared.join("model.bin"), b"shared").unwrap();

        assert_eq!(
            resolve_model_for_read(&owned, Some(&shared), "model.bin"),
            shared.join("model.bin")
        );
        assert_eq!(
            resolve_model_for_read(&owned, Some(&shared), "parakeet/model"),
            shared.join("parakeet/model")
        );
        assert_eq!(
            resolve_model_for_read(&owned, Some(&shared), "missing.bin"),
            owned.join("missing.bin")
        );
        assert_eq!(
            resolve_model_for_read(&owned, None, "model.bin"),
            owned.join("model.bin")
        );

        std::fs::write(owned.join("model.bin"), b"owned").unwrap();
        std::fs::create_dir_all(owned.join("parakeet/model")).unwrap();
        assert_eq!(
            resolve_model_for_read(&owned, Some(&shared), "model.bin"),
            owned.join("model.bin")
        );
        assert_eq!(
            resolve_model_for_read(&owned, Some(&shared), "parakeet/model"),
            owned.join("parakeet/model")
        );
        assert_eq!(std::fs::read(shared.join("model.bin")).unwrap(), b"shared");
    }
}
