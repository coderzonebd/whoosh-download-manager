// SPDX-License-Identifier: GPL-3.0-or-later
//! Tauri's build step, plus the UI's locale files (`packages/ui/src/lib/i18n/
//! locales/*.json`, edited on Weblate) embedded for the strings the shell
//! shows itself (the tray menu). See `src/shell_i18n.rs`.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    embed_locales();
    tauri_build::build();
}

fn embed_locales() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dir = manifest_dir.join("../../../packages/ui/src/lib/i18n/locales");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let tag = path.file_stem()?.to_str()?.to_owned();
            Some((tag, path))
        })
        .collect();
    files.sort();
    let mut code = String::from(
        "/// (tag, locale JSON), from build.rs.\npub static LOCALES: &[(&str, &str)] = &[\n",
    );
    for (tag, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        // An absolute path (CARGO_MANIFEST_DIR is absolute), not canonicalized:
        // on Windows that gives a `\\?\` verbatim path. Debug formatting
        // escapes the backslashes.
        writeln!(
            code,
            "    ({tag:?}, include_str!({:?})),",
            path.display().to_string()
        )
        .expect("write to String");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("locales.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}
