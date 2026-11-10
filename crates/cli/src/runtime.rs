//! Where to find the daemon: [`vayu_proto::runtime`], the data-dir rule and
//! the runtime file's checks shared with the daemon and `vayu-nmhost`.

use std::path::Path;

pub use vayu_proto::runtime::{RUNTIME_FILE, RuntimeError, RuntimeInfo, default_data_dir};

/// What reading `<data dir>/runtime.json` found.
#[derive(Debug)]
pub enum Lookup {
    Found(RuntimeInfo),
    /// No usable file; the reason is for messages. Starting a daemon fixes
    /// all of these: it rewrites the file (owner-only) and tightens the
    /// directory, or refuses with its own message.
    Unusable(String),
}

pub fn read(data_dir: &Path) -> Result<Lookup, RuntimeError> {
    let path = data_dir.join(RUNTIME_FILE);
    match vayu_proto::runtime::read_runtime(&path) {
        Ok(info) => Ok(Lookup::Found(info)),
        Err(RuntimeError::Missing(_)) => Ok(Lookup::Unusable("no runtime file".into())),
        Err(e @ (RuntimeError::Invalid { .. } | RuntimeError::Insecure { .. })) => {
            Ok(Lookup::Unusable(e.to_string()))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unusable_runtime_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vayu-cli-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = dir.join(RUNTIME_FILE);
        assert!(matches!(read(&dir).unwrap(), Lookup::Unusable(why) if why == "no runtime file"));
        std::fs::write(&file, b"{\"port\":").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(read(&dir).unwrap(), Lookup::Unusable(_)));
        std::fs::write(&file, br#"{"port":1,"token":"t","pid":2,"version":"v"}"#).unwrap();
        assert!(matches!(read(&dir).unwrap(), Lookup::Found(i) if i.port == 1));
        // Readable by others: refused.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(read(&dir).unwrap(), Lookup::Unusable(why) if why.contains("mode")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
