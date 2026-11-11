//! Product names on disk.
//!
//! The project was renamed from Hard Vibe Code to Ikore on 2026-11-10. Everything
//! new uses the current names. The legacy names are still recognized, so an
//! existing data folder, database, snapshot or workspace export folder carries
//! over: [`adopt_legacy_dir`] and [`adopt_legacy_database`] move them once, the
//! first time the renamed application starts. Stored credentials move on first
//! use in [`crate::llm::KeyringCredentialStore`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Folder under `%APPDATA%` (data) and `%LOCALAPPDATA%` (machine-local cache).
pub const APP_IDENTIFIER: &str = "com.ikore.desktop";
/// The folder name used before the rename.
pub const LEGACY_APP_IDENTIFIER: &str = "com.hardvibecode.desktop";

/// SQLite database inside the data folder.
pub const DATABASE_FILE_NAME: &str = "ikore.db";
/// The database name used before the rename.
pub const LEGACY_DATABASE_FILE_NAME: &str = "hard-vibe-code.db";

/// Lock file that keeps a second instance away from the data folder.
pub const LOCK_FILE_NAME: &str = "ikore.lock";
/// The lock file of an instance built before the rename.
pub const LEGACY_LOCK_FILE_NAME: &str = "hard-vibe-code.lock";

/// Prefixes of the app-owned database snapshots (`<prefix>.pre-v24.<uuid>.db`,
/// `<prefix>.after-deletion.<uuid>.db`). New snapshots use the first one; the
/// cleanup that follows a project deletion recognizes all of them.
pub const SNAPSHOT_PREFIXES: [&str; 2] = ["ikore", "hard-vibe-code"];

/// Folder for the exports the application writes inside a workspace.
pub const WORKSPACE_DIR_NAME: &str = ".ikore";
/// The workspace folder used before the rename. It stays excluded from
/// repository reads, like the current one.
pub const LEGACY_WORKSPACE_DIR_NAME: &str = ".hard-vibe-code";

/// SQLite files that travel with the database, renamed before the database itself.
const DATABASE_COMPANION_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// The current snapshot prefix.
pub fn snapshot_prefix() -> &'static str {
    SNAPSHOT_PREFIXES[0]
}

/// Returns the folder to use: `current` when it exists or once `legacy` has been
/// renamed to it, and `current` when neither exists. When the rename fails (for
/// example, because an instance built before the rename still holds files open in
/// `legacy`), it returns `legacy`, so nothing is lost and the next start tries again.
pub fn adopt_legacy_dir(legacy: &Path, current: &Path) -> PathBuf {
    if current.exists() || !legacy.is_dir() {
        return current.to_path_buf();
    }
    match fs::rename(legacy, current) {
        Ok(()) => current.to_path_buf(),
        Err(_) => legacy.to_path_buf(),
    }
}

/// Renames a legacy database in `data_dir` to [`DATABASE_FILE_NAME`], once, before
/// it is opened. The WAL, shared-memory and journal files move first, so an
/// interruption never leaves the renamed database without its WAL. Returns whether
/// the database was renamed. Does nothing when the current database already exists.
pub fn adopt_legacy_database(data_dir: &Path) -> io::Result<bool> {
    let current = data_dir.join(DATABASE_FILE_NAME);
    let legacy = data_dir.join(LEGACY_DATABASE_FILE_NAME);
    if current.exists() || !legacy.is_file() {
        return Ok(false);
    }
    for suffix in DATABASE_COMPANION_SUFFIXES {
        let from = data_dir.join(format!("{LEGACY_DATABASE_FILE_NAME}{suffix}"));
        let to = data_dir.join(format!("{DATABASE_FILE_NAME}{suffix}"));
        if from.is_file() && !to.exists() {
            fs::rename(&from, &to)?;
        }
    }
    fs::rename(&legacy, &current)?;
    Ok(true)
}

/// Whether `name` is an app-owned snapshot of the given kind (`pre-v13`, `pre-v24`,
/// `after-deletion`) under any known prefix, returning the part after the kind.
pub fn snapshot_suffix<'a>(name: &'a str, kind: &str) -> Option<&'a str> {
    SNAPSHOT_PREFIXES.iter().find_map(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.strip_prefix(kind))
            .and_then(|rest| rest.strip_prefix('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_folder_is_renamed_once() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join(LEGACY_APP_IDENTIFIER);
        let current = root.path().join(APP_IDENTIFIER);
        fs::create_dir_all(legacy.join("voice")).unwrap();
        fs::write(legacy.join("voice/model.bin"), b"model").unwrap();

        assert_eq!(adopt_legacy_dir(&legacy, &current), current);
        assert!(!legacy.exists());
        assert_eq!(fs::read(current.join("voice/model.bin")).unwrap(), b"model");
        // A second start finds the current folder and leaves it alone.
        assert_eq!(adopt_legacy_dir(&legacy, &current), current);
    }

    #[test]
    fn an_existing_current_folder_wins_over_a_legacy_one() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join(LEGACY_APP_IDENTIFIER);
        let current = root.path().join(APP_IDENTIFIER);
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&current).unwrap();

        assert_eq!(adopt_legacy_dir(&legacy, &current), current);
        assert!(
            legacy.exists(),
            "the legacy folder is never merged or removed"
        );
    }

    #[test]
    fn without_any_folder_the_current_name_is_used() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join(APP_IDENTIFIER);
        assert_eq!(
            adopt_legacy_dir(&root.path().join(LEGACY_APP_IDENTIFIER), &current),
            current
        );
        assert!(!current.exists(), "the caller creates the folder");
    }

    #[test]
    fn a_legacy_database_moves_with_its_companions() {
        let data = tempfile::tempdir().unwrap();
        for (suffix, content) in [("", "db"), ("-wal", "wal"), ("-shm", "shm")] {
            fs::write(
                data.path()
                    .join(format!("{LEGACY_DATABASE_FILE_NAME}{suffix}")),
                content,
            )
            .unwrap();
        }

        assert!(adopt_legacy_database(data.path()).unwrap());
        for (suffix, content) in [("", "db"), ("-wal", "wal"), ("-shm", "shm")] {
            let current = data.path().join(format!("{DATABASE_FILE_NAME}{suffix}"));
            assert_eq!(fs::read_to_string(current).unwrap(), content);
            assert!(
                !data
                    .path()
                    .join(format!("{LEGACY_DATABASE_FILE_NAME}{suffix}"))
                    .exists()
            );
        }
        assert!(!adopt_legacy_database(data.path()).unwrap());
    }

    #[test]
    fn a_current_database_is_never_replaced_by_a_legacy_one() {
        let data = tempfile::tempdir().unwrap();
        fs::write(data.path().join(DATABASE_FILE_NAME), "current").unwrap();
        fs::write(data.path().join(LEGACY_DATABASE_FILE_NAME), "legacy").unwrap();

        assert!(!adopt_legacy_database(data.path()).unwrap());
        assert_eq!(
            fs::read_to_string(data.path().join(DATABASE_FILE_NAME)).unwrap(),
            "current"
        );
    }

    #[test]
    fn an_interrupted_rename_finishes_on_the_next_start() {
        let data = tempfile::tempdir().unwrap();
        // The WAL already moved; the database did not.
        fs::write(data.path().join(format!("{DATABASE_FILE_NAME}-wal")), "wal").unwrap();
        fs::write(data.path().join(LEGACY_DATABASE_FILE_NAME), "db").unwrap();

        assert!(adopt_legacy_database(data.path()).unwrap());
        assert_eq!(
            fs::read_to_string(data.path().join(DATABASE_FILE_NAME)).unwrap(),
            "db"
        );
        assert_eq!(
            fs::read_to_string(data.path().join(format!("{DATABASE_FILE_NAME}-wal"))).unwrap(),
            "wal"
        );
    }

    #[test]
    fn snapshots_are_recognized_under_both_prefixes() {
        assert_eq!(
            snapshot_suffix("ikore.pre-v24.1234.db", "pre-v24"),
            Some("1234.db")
        );
        assert_eq!(
            snapshot_suffix("hard-vibe-code.after-deletion.1234.db", "after-deletion"),
            Some("1234.db")
        );
        assert_eq!(snapshot_suffix("ikore.db", "pre-v24"), None);
        assert_eq!(snapshot_suffix("other.pre-v24.1234.db", "pre-v24"), None);
        assert_eq!(snapshot_prefix(), "ikore");
    }
}
