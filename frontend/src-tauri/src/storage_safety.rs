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

    if path.file_name().and_then(|name| name.to_str()) != Some(BRANCH_IDENTIFIER) {
        return Err(format!(
            "Meetily Branch resolved an unexpected storage directory: {}. No files were opened.",
            path.display()
        ));
    }
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
}
