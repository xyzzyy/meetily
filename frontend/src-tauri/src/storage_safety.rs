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

/// Resolve branch-owned storage only after the identity has been validated.
pub fn branch_app_data_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    validate_runtime_identity(app)?;
    let path = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve Meetily Branch storage: {error}"))?;

    validate_branch_directory(&path)?;
    Ok(path)
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Meetily Branch storage must not be a symbolic link: {}. No files were opened.",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to inspect Meetily Branch storage {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn validate_branch_directory(path: &Path) -> Result<(), String> {
    if path.file_name().and_then(|name| name.to_str()) != Some(BRANCH_IDENTIFIER) {
        return Err(format!(
            "Meetily Branch resolved an unexpected storage directory: {}. No files were opened.",
            path.display()
        ));
    }
    reject_symlink(path)
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
    adopt_legacy_installation(&resolved_branch_dir)
}

fn adopt_legacy_installation(resolved_branch_dir: &Path) -> Result<PathBuf, String> {
    validate_branch_directory(resolved_branch_dir)?;
    let (legacy_dir, branch_dir) = legacy_and_branch_dirs(resolved_branch_dir)?;
    reject_symlink(&legacy_dir)?;
    let legacy_exists = legacy_dir.exists();
    let branch_exists = branch_dir.exists();

    match (legacy_exists, branch_exists) {
        (true, false) => {
            std::fs::rename(&legacy_dir, &branch_dir).map_err(|error| {
                format!(
                    "Could not move the previous speaker-test installation from {} to {}: {error}",
                    legacy_dir.display(),
                    branch_dir.display()
                )
            })?;
            Ok(branch_dir)
        }
        (true, true) => Err(format!(
            "Meetily Branch found two separate installations and will not merge them automatically. Neither was modified.\n\nPrevious test data: {}\nBranch data: {}",
            legacy_dir.display(),
            branch_dir.display()
        )),
        _ => Ok(branch_dir),
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
    fn legacy_adoption_moves_only_isolated_data_and_refuses_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let branch = temp.path().join(BRANCH_IDENTIFIER);
        let legacy = temp.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        let official = temp.path().join(OFFICIAL_IDENTIFIER);
        std::fs::create_dir(&official).unwrap();
        std::fs::write(official.join("data"), b"official").unwrap();
        assert!(validate_branch_directory(&branch).is_ok());
        assert_eq!(adopt_legacy_installation(&branch).unwrap(), branch);
        assert!(!branch.exists());

        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("data"), b"legacy").unwrap();
        assert_eq!(adopt_legacy_installation(&branch).unwrap(), branch);
        assert_eq!(std::fs::read(branch.join("data")).unwrap(), b"legacy");
        assert!(!legacy.exists());

        std::fs::create_dir(&legacy).unwrap();
        assert!(adopt_legacy_installation(&branch).is_err());
        assert!(legacy.exists());
        assert_eq!(std::fs::read(branch.join("data")).unwrap(), b"legacy");
        assert_eq!(std::fs::read(official.join("data")).unwrap(), b"official");
        assert!(validate_branch_directory(&official).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn branch_and_legacy_symlinks_are_rejected_including_dangling_links() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let branch = temp.path().join(BRANCH_IDENTIFIER);
        let legacy = temp.path().join(LEGACY_SPEAKER_TEST_IDENTIFIER);
        let official = temp.path().join(OFFICIAL_IDENTIFIER);
        std::fs::create_dir(&official).unwrap();
        std::fs::write(official.join("data"), b"official").unwrap();

        for link in [&branch, &legacy] {
            for target in [&official, &temp.path().join("missing")] {
                symlink(target, link).unwrap();
                assert!(adopt_legacy_installation(&branch)
                    .unwrap_err()
                    .contains("symbolic link"));
                if link == &branch {
                    assert!(validate_branch_directory(&branch).is_err());
                } else {
                    assert!(!branch.exists());
                }
                assert!(std::fs::symlink_metadata(link)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert_eq!(std::fs::read(official.join("data")).unwrap(), b"official");
                std::fs::remove_file(link).unwrap();
            }
        }
    }
}
