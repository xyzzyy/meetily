use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime};

pub fn owned_models_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let app_data_dir = crate::storage_safety::branch_app_data_dir(app)?;
    let owned = app_data_dir.join("models");
    std::fs::create_dir_all(&owned)
        .map_err(|error| format!("Failed to create branch model directory: {error}"))?;
    Ok(owned)
}

/// Derive the official shared models root from a branch-owned app-data
/// directory.
///
/// The official directory is only ever used as a read-only lookup root, so it
/// is only returned when the app-data directory really is the branch one and
/// the official models directory actually exists.
pub fn official_models_root_for_app_data(branch_app_data_dir: &Path) -> Option<PathBuf> {
    if branch_app_data_dir.file_name().and_then(|name| name.to_str())
        != Some(crate::storage_safety::BRANCH_IDENTIFIER)
    {
        return None;
    }

    let application_support = branch_app_data_dir.parent()?;
    let official = application_support
        .join(crate::storage_safety::OFFICIAL_IDENTIFIER)
        .join("models");
    official.is_dir().then_some(official)
}

/// Official Meetily's model directory is exposed only as an optional,
/// read-only lookup root. No mutation helper ever returns this path.
pub fn official_models_root<R: Runtime>(app: &AppHandle<R>) -> Result<Option<PathBuf>, String> {
    let branch_dir = crate::storage_safety::branch_app_data_dir(app)?;
    Ok(official_models_root_for_app_data(&branch_dir))
}

/// Compatibility alias for code that needs a writable model destination.
pub fn models_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    owned_models_root(app)
}

/// Resolve a model for reading.
///
/// Branch-owned storage always wins. The official shared directory is only
/// consulted when the model is missing from branch-owned storage. When neither
/// root holds the model, the branch-owned path is returned so that downloads
/// and other mutations keep writing to branch-owned storage.
pub fn resolve_model_path_for_read(
    owned_root: &Path,
    shared_root: Option<&Path>,
    relative_path: &Path,
) -> PathBuf {
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

/// Resolve a model relative to the branch models root, falling back to the
/// official shared directory when the model is absent from branch storage.
pub fn resolve_model_for_read<R: Runtime>(
    app: &AppHandle<R>,
    relative_path: impl AsRef<Path>,
) -> Result<PathBuf, String> {
    let owned_root = owned_models_root(app)?;
    let shared_root = official_models_root(app)?;
    Ok(resolve_model_path_for_read(
        &owned_root,
        shared_root.as_deref(),
        relative_path.as_ref(),
    ))
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

    #[test]
    fn shared_root_is_only_derived_from_branch_app_data() {
        let temp = tempfile::tempdir().unwrap();
        let branch_app_data = temp.path().join("com.meetily.ai.branch");
        std::fs::create_dir_all(&branch_app_data).unwrap();

        // The official directory does not exist yet, so no shared root is
        // exposed.
        assert!(official_models_root_for_app_data(&branch_app_data).is_none());

        // A non-branch app-data directory never resolves to official storage.
        let official_app_data = temp.path().join("com.meetily.ai");
        std::fs::create_dir_all(&official_app_data).unwrap();
        assert!(official_models_root_for_app_data(&official_app_data).is_none());

        // The official models directory is only exposed as a read-only lookup
        // root for the branch app-data directory.
        let official_models = temp.path().join("com.meetily.ai").join("models");
        std::fs::create_dir_all(&official_models).unwrap();
        assert_eq!(
            official_models_root_for_app_data(&branch_app_data),
            Some(official_models)
        );
    }

    #[test]
    fn read_resolution_prefers_branch_storage_and_keeps_writes_local() {
        let temp = tempfile::tempdir().unwrap();
        let owned_root = temp.path().join("owned");
        let shared_root = temp.path().join("shared");
        std::fs::create_dir_all(&owned_root).unwrap();
        std::fs::create_dir_all(&shared_root).unwrap();

        let owned_file = owned_root.join("model.bin");
        let shared_file = shared_root.join("model.bin");
        std::fs::write(&shared_file, b"shared").unwrap();

        // Missing from branch storage: fall back to the shared directory.
        assert_eq!(
            resolve_model_path_for_read(
                &owned_root,
                Some(shared_root.as_path()),
                Path::new("model.bin")
            ),
            shared_file
        );

        // Present in branch storage: branch storage wins.
        std::fs::write(&owned_file, b"owned").unwrap();
        assert_eq!(
            resolve_model_path_for_read(
                &owned_root,
                Some(shared_root.as_path()),
                Path::new("model.bin")
            ),
            owned_file
        );

        // Missing everywhere: the branch-owned path is returned so that
        // downloads keep writing to branch storage.
        assert_eq!(
            resolve_model_path_for_read(
                &owned_root,
                Some(shared_root.as_path()),
                Path::new("other.bin")
            ),
            owned_root.join("other.bin")
        );

        // No shared root: branch storage is used unconditionally.
        assert_eq!(
            resolve_model_path_for_read(&owned_root, None, Path::new("model.bin")),
            owned_file
        );
    }
}
