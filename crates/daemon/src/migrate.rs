//! One-time moves after the internal names changed from `whoosh` to `wdm`
//! (2026-11-14). The data directory itself moves in
//! `wdm_proto::runtime::default_data_dir`; what is left is the in-progress
//! files next to unfinished downloads.

use std::path::Path;

/// The in-progress suffix before the rename.
pub const LEGACY_PART_SUFFIX: &str = ".whoosh-part";

/// The state store key that records that [`part_files`] has run.
pub const PART_FILES_KEY: &str = "migration_part_suffix";

/// For each unfinished download (`directory`, `filename`), renames
/// `<filename>.whoosh-part` to `<filename><suffix>` when the new name is not
/// taken, so the download resumes from the bytes it has. Names that are not
/// a plain file name are skipped. Returns how many files were renamed;
/// failures are logged and skipped (the download then starts over, as it
/// would without this).
pub fn part_files<'a>(
    downloads: impl IntoIterator<Item = (&'a Path, &'a str)>,
    suffix: &str,
) -> usize {
    let mut moved = 0;
    for (dir, name) in downloads {
        if name.is_empty() || Path::new(name).file_name() != Some(name.as_ref()) {
            continue;
        }
        let legacy = dir.join(format!("{name}{LEGACY_PART_SUFFIX}"));
        let part = dir.join(format!("{name}{suffix}"));
        if !legacy.is_file() || std::fs::symlink_metadata(&part).is_ok() {
            continue;
        }
        match std::fs::rename(&legacy, &part) {
            Ok(()) => moved += 1,
            Err(e) => tracing::warn!("cannot rename {}: {e}", legacy.display()),
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wdm-daemon-migrate-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn renames_old_part_files_to_the_new_suffix() {
        let d = Dir::new("rename");
        std::fs::write(d.0.join("a.iso.whoosh-part"), b"half").unwrap();
        std::fs::write(d.0.join("b.zip.whoosh-part"), b"b").unwrap();
        let moved = part_files(
            [(d.0.as_path(), "a.iso"), (d.0.as_path(), "c.bin")],
            ".wdm-part",
        );
        assert_eq!(moved, 1);
        assert_eq!(std::fs::read(d.0.join("a.iso.wdm-part")).unwrap(), b"half");
        assert!(!d.0.join("a.iso.whoosh-part").exists());
        // Not one of the downloads given: left alone.
        assert!(d.0.join("b.zip.whoosh-part").exists());
        // Running again changes nothing.
        assert_eq!(part_files([(d.0.as_path(), "a.iso")], ".wdm-part"), 0);
    }

    #[test]
    fn never_overwrites_a_new_part_file() {
        let d = Dir::new("taken");
        std::fs::write(d.0.join("a.iso.whoosh-part"), b"old").unwrap();
        std::fs::write(d.0.join("a.iso.wdm-part"), b"new").unwrap();
        assert_eq!(part_files([(d.0.as_path(), "a.iso")], ".wdm-part"), 0);
        assert_eq!(std::fs::read(d.0.join("a.iso.wdm-part")).unwrap(), b"new");
        assert!(d.0.join("a.iso.whoosh-part").exists());
    }

    #[test]
    fn skips_names_that_are_not_a_plain_file_name() {
        let d = Dir::new("names");
        std::fs::create_dir_all(d.0.join("sub")).unwrap();
        std::fs::write(d.0.join("sub/x.whoosh-part"), b"x").unwrap();
        assert_eq!(
            part_files([(d.0.as_path(), "sub/x"), (d.0.as_path(), "")], ".wdm-part"),
            0
        );
        assert!(d.0.join("sub/x.whoosh-part").exists());
    }
}
