// SPDX-License-Identifier: GPL-3.0-or-later
//! The downloads the prompt window follows ([`crate::FOLLOWED_FILE`] in
//! the data directory), like IDM's progress and "Download complete"
//! windows: one started in the prompt window shows its progress there, and
//! when it ends the prompt window comes back to say so, even after the
//! user hid it. The prompt window writes the list; the tray reads it to
//! know which ended download should bring the prompt window back.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// More than this and the oldest are forgotten.
pub const MAX: usize = 50;

/// A followed download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Followed {
    pub id: String,
    /// The user hid its progress: it shows again only when it ends.
    #[serde(default)]
    pub hidden: bool,
}

/// An entry as written now (`{"id", "hidden"}`) or by earlier versions (the id alone).
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Full(Followed),
    Id(String),
}

/// The list in `bytes`: unknown entries, empty or overlong ids and repeats
/// are dropped, and only the newest [`MAX`] are kept.
pub fn parse(bytes: &[u8]) -> Vec<Followed> {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(bytes).unwrap_or_default();
    let mut out: Vec<Followed> = Vec::new();
    for v in entries {
        let f = match serde_json::from_value::<Entry>(v) {
            Ok(Entry::Full(f)) => f,
            Ok(Entry::Id(id)) => Followed { id, hidden: false },
            Err(_) => continue,
        };
        if f.id.is_empty() || f.id.len() > 200 || out.iter().any(|o| o.id == f.id) {
            continue;
        }
        out.push(f);
    }
    let extra = out.len().saturating_sub(MAX);
    out.drain(..extra);
    out
}

/// The list in `dir`; none when the file is missing or unreadable.
pub fn read(dir: &Path) -> Vec<Followed> {
    std::fs::read(dir.join(crate::FOLLOWED_FILE))
        .map(|b| parse(&b))
        .unwrap_or_default()
}

/// Saves the list in `dir` (removing the file when it is empty); best effort.
pub fn write(dir: &Path, list: &[Followed]) {
    let path = dir.join(crate::FOLLOWED_FILE);
    if list.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let start = list.len().saturating_sub(MAX);
    let Ok(json) = serde_json::to_vec(&list[start..]) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_forms_and_drops_what_is_not_an_id() {
        let list =
            parse(br#"["dl_1", {"id": "dl_2", "hidden": true}, {"id": "dl_1"}, "", 5, {"x": 1}]"#);
        assert_eq!(
            list,
            vec![
                Followed {
                    id: "dl_1".into(),
                    hidden: false
                },
                Followed {
                    id: "dl_2".into(),
                    hidden: true
                },
            ]
        );
        assert!(parse(b"not json").is_empty());
        assert!(parse(b"{}").is_empty());
    }

    #[test]
    fn keeps_the_newest_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("wdm-followed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let many: Vec<Followed> = (0..MAX + 5)
            .map(|i| Followed {
                id: format!("dl_{i}"),
                hidden: i % 2 == 0,
            })
            .collect();
        write(&dir, &many);
        let back = read(&dir);
        assert_eq!(back.len(), MAX);
        assert_eq!(back[0].id, "dl_5");
        assert_eq!(back, many[5..]);
        write(&dir, &[]);
        assert!(!dir.join(crate::FOLLOWED_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
