//! Keeping the core database's WAL consistent with its main file.
//!
//! In WAL mode a database is two files: the main file and a `-wal` file with
//! recent changes that have not been copied into it yet. SQLite applies the WAL
//! named after the database without checking that it belongs to that file. A
//! WAL left over from another copy of the database therefore does not stop the
//! database from opening; it silently replaces pages, and the damage shows up
//! later. It cannot be detected reliably after the fact, so it has to be
//! prevented: whenever the main file is created or replaced, any WAL already
//! next to it belongs to a different file.
//!
//! When the database cannot be read at all, the main file is damaged. A failed
//! connection already folds a valid WAL into the main file as it closes, so
//! moving the WAL afterwards cannot help. The damage is reported and the files
//! are left as they are.

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The `-wal` file SQLite pairs with `database`.
pub(crate) fn wal_path(database: &Path) -> PathBuf {
    companion(database, "-wal")
}

fn shm_path(database: &Path) -> PathBuf {
    companion(database, "-shm")
}

fn companion(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Before `database` is created or copied into place, move aside a WAL that was
/// left behind by an earlier file of the same name, so SQLite cannot apply it to
/// the new file. The WAL is renamed to pair with
/// `<name>.orphaned-<time>.<extension>`, so it still opens with its original
/// main file if that is ever put back under that name. Returns the new WAL path.
pub(crate) fn set_aside_orphaned_wal(database: &Path) -> std::io::Result<Option<PathBuf>> {
    let wal = wal_path(database);
    let moved = if wal.exists() {
        let target = wal_path(&unused_orphan_path(database));
        std::fs::rename(&wal, &target)?;
        Some(target)
    } else {
        None
    };
    // The -shm file is an index SQLite rebuilds from the WAL; it holds no data.
    match std::fs::remove_file(shm_path(database)) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    Ok(moved)
}

fn unused_orphan_path(database: &Path) -> PathBuf {
    let stem = database
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "database".to_string());
    let extension = database
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut attempt = 1;
    loop {
        let suffix = if attempt == 1 { String::new() } else { format!("-{attempt}") };
        let candidate =
            database.with_file_name(format!("{stem}.orphaned-{stamp}{suffix}{extension}"));
        if !candidate.exists() && !wal_path(&candidate).exists() {
            return candidate;
        }
        attempt += 1;
    }
}

/// Whether the database and its WAL can be read without damage. The check runs
/// on a throwaway copy so the real files are not touched. Every connection to
/// the database must be closed first, so the files are not changing while they
/// are copied.
pub(crate) async fn is_readable(database: &Path) -> std::io::Result<bool> {
    let scratch = tempfile::tempdir()?;
    let copy = scratch.path().join("check.sqlite");
    std::fs::copy(database, &copy)?;
    let wal = wal_path(database);
    if wal.exists() {
        std::fs::copy(&wal, wal_path(&copy))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(&copy)
        .create_if_missing(false);
    let Ok(mut connection) = options.connect().await else {
        return Ok(false);
    };
    let rows: Result<Vec<String>, _> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(&mut connection)
        .await;
    let _ = connection.close().await;
    Ok(matches!(rows.as_deref(), Ok([result]) if result == "ok"))
}

/// Startup message when the database file is damaged.
pub(crate) fn damaged_message(database: &Path, error: &dyn std::fmt::Display) -> String {
    let wal = wal_path(database);
    let files = if wal.exists() {
        format!("{}\n{}", database.display(), wal.display())
    } else {
        database.display().to_string()
    };
    format!(
        "The meeting database is damaged and cannot be read ({error}).\n\n\
         Meetily Branch did not delete or replace it. Keep these files if you want to recover data from them:\n\n\
         {files}\n\n\
         Restore the database from a backup before opening the app again."
    )
}
