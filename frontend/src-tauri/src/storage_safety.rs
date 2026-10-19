use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

pub const OFFICIAL_IDENTIFIER: &str = "com.meetily.ai";
pub const BRANCH_IDENTIFIER: &str = "com.meetily.ai.branch";
pub const LEGACY_SPEAKER_TEST_IDENTIFIER: &str = "com.meetily.ai.speaker-test";

/// This check intentionally uses only the compiled configuration. It must run
/// before resolving an app-data path or initializing any subsystem that may do
/// so. A branch binary carrying the official identity is never allowed to
/// discover or open official Meetily storage.
pub fn validate_compiled_identity(identifier: &str) -> Result<(), String> {
    if identifier == OFFICIAL_IDENTIFIER {
        return Err(
            "Unsafe Meetily Branch build: this binary uses the official com.meetily.ai identity. It has stopped before opening any database or application-data file. Rebuild it with com.meetily.ai.branch."
                .to_string(),
        );
    }
    if identifier != BRANCH_IDENTIFIER {
        return Err(format!(
            "Unsafe Meetily Branch build identity: {identifier}. Expected {BRANCH_IDENTIFIER}. The app has stopped before opening any database or application-data file."
        ));
    }
    Ok(())
}

pub fn validate_runtime_identity<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    validate_compiled_identity(&app.config().identifier)
}

/// Reject a storage directory that is a symlink. A link could point the
/// branch build straight at the official installation, so neither the branch
/// nor the legacy directory may be used through one.
fn ensure_not_symlink(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "Meetily Branch refused to use a symlinked {label} directory: {}. No files were opened.",
                    path.display()
                ));
            }
            Ok(())
        }
        // A directory that does not exist yet cannot be a link to official
        // storage, and it is created later by the normal storage setup.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Meetily Branch could not verify the {label} directory {}: {error}. No files were opened.",
            path.display()
        )),
    }
}

/// Resolve branch-owned storage only after the identity has been validated.
pub fn branch_app_data_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    validate_runtime_identity(app)?;
    let path = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve Meetily Branch storage: {error}"))?;

    if path.file_name().and_then(|name| name.to_str()) != Some(BRANCH_IDENTIFIER) {
        return Err(format!(
            "Meetily Branch resolved an unexpected storage directory: {}. No files were opened.",
            path.display()
        ));
    }
    // The name check above only looks at the last path component. Verify the
    // directory itself is not a link that redirects the branch build into
    // official Meetily storage.
    ensure_not_symlink(&path, "branch storage")?;
    Ok(path)
}

fn legacy_and_branch_dirs(branch_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let parent = branch_dir.parent().ok_or_else(|| {
        format!(
            "Meetily Branch storage has no parent directory: {}",
            branch_dir.display()
        )
    })?;
    Ok((
        parent.join(LEGACY_SPEAKER_TEST_IDENTIFIER),
        parent.join(BRANCH_IDENTIFIER),
    ))
}

/// Adopt the previous isolated test installation without copying its model
/// cache. The official directory is neither resolved nor inspected here.
pub fn adopt_legacy_speaker_test_installation<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<PathBuf, String> {
    let resolved_branch_dir = branch_app_data_dir(app)?;
    let (legacy_dir, branch_dir) = legacy_and_branch_dirs(&resolved_branch_dir)?;
    adopt_legacy_directory(&legacy_dir, &branch_dir)
}

/// Move the previous speaker-test installation into the branch directory.
/// Both paths are proven to be real directories, so neither the move nor the
/// resulting branch directory can resolve to official storage.
fn adopt_legacy_directory(legacy_dir: &Path, branch_dir: &Path) -> Result<PathBuf, String> {
    ensure_not_symlink(legacy_dir, "previous speaker-test storage")?;
    ensure_not_symlink(branch_dir, "branch storage")?;

    let legacy_exists = legacy_dir.exists();
    let branch_exists = branch_dir.exists();

    match (legacy_exists, branch_exists) {
        (true, false) => {
            std::fs::rename(legacy_dir, branch_dir).map_err(|error| {
                format!(
                    "Could not move the previous speaker-test installation from {} to {}: {error}",
                    legacy_dir.display(),
                    branch_dir.display()
                )
            })?;
            // The adopted directory is used from here on, so it is verified
            // again before it is handed back.
            ensure_not_symlink(branch_dir, "branch storage")?;
            Ok(branch_dir.to_path_buf())
        }
        (true, true) => Err(format!(
            "Meetily Branch found two separate installations and will not merge them automatically. Neither was modified.\n\nPrevious test data: {}\nBranch data: {}",
            legacy_dir.display(),
            branch_dir.display()
        )),
        _ => Ok(branch_dir.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_identity_is_rejected_before_path_resolution() {
        assert!(validate_compiled_identity(OFFICIAL_IDENTIFIER).is_err());
        assert!(validate_compiled_identity(BRANCH_IDENTIFIER).is_ok());
        assert!(validate_compiled_identity(LEGACY_SPEAKER_TEST_IDENTIFIER).is_err());
    }

    #[test]
    fn migration_paths_never_point_at_official_storage() {
        let branch = Path::new("/tmp/Application Support/com.meetily.ai.branch");
        let (legacy, resolved) = legacy_and_branch_dirs(branch).unwrap();
        assert_eq!(legacy.file_name().unwrap(), LEGACY_SPEAKER_TEST_IDENTIFIER);
        assert_eq!(resolved.file_name().unwrap(), BRANCH_IDENTIFIER);
        assert_ne!(legacy.file_name().unwrap(), OFFICIAL_IDENTIFIER);
        assert_ne!(resolved.file_name().unwrap(), OFFICIAL_IDENTIFIER);
    }

    #[test]
    fn real_and_missing_directories_pass_the_symlink_check() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(BRANCH_IDENTIFIER);
        std::fs::create_dir_all(&storage).unwrap();
        assert!(ensure_not_symlink(&storage, "branch storage").is_ok());
        assert!(ensure_not_symlink(&dir.path().join("missing"), "branch storage").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let official = dir.path().join(OFFICIAL_IDENTIFIER);
        std::fs::create_dir_all(&official).unwrap();

        let link = dir.path().join(BRANCH_IDENTIFIER);
        std::os::unix::fs::symlink(&official, &link).unwrap();
        assert!(ensure_not_symlink(&link, "branch storage").is_err());

        // A link with a missing target is rejected as well, so it cannot
        // start pointing at official storage later on.
        let broken = dir.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        std::os::unix::fs::symlink(dir.path().join("missing"), &broken).unwrap();
        assert!(ensure_not_symlink(&broken, "previous speaker-test storage").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_legacy_directory_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let official = dir.path().join(OFFICIAL_IDENTIFIER);
        std::fs::create_dir_all(&official).unwrap();

        let legacy = dir.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        std::os::unix::fs::symlink(&official, &legacy).unwrap();
        let branch = dir.path().join(BRANCH_IDENTIFIER);

        assert!(adopt_legacy_directory(&legacy, &branch).is_err());
        assert!(official.is_dir());
        assert!(!branch.exists());
    }

    #[test]
    fn a_real_legacy_directory_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        let branch = dir.path().join(BRANCH_IDENTIFIER);
        std::fs::create_dir_all(&legacy).unwrap();

        let adopted = adopt_legacy_directory(&legacy, &branch).unwrap();
        assert_eq!(adopted, branch);
        assert!(branch.is_dir());
        assert!(!legacy.exists());
    }

    #[test]
    fn two_existing_installations_are_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        let branch = dir.path().join(BRANCH_IDENTIFIER);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&branch).unwrap();

        assert!(adopt_legacy_directory(&legacy, &branch).is_err());
        assert!(legacy.is_dir());
        assert!(branch.is_dir());
    }
}
