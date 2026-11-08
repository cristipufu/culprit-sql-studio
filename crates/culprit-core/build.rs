//! Compiles every language catalog (`locales/<code>.json` at the repository root) into the library: a language is one
//! file, picked up by the next build, and nothing is read from disk when Culprit runs.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let dir = manifest.join("../../locales").canonicalize().expect("the locales directory");
    // A file added, removed or edited there rebuilds the catalogs.
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut catalogs: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .expect("read locales/")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let code = path.file_name()?.to_str()?.strip_suffix(".json")?.to_string();
            is_language_code(&code).then_some((code, path))
        })
        .collect();
    catalogs.sort();
    let mut out = String::from("/// Every `locales/<code>.json`, by language code, as the build found them.\n");
    out.push_str("pub(crate) static CATALOGS: &[(&str, &str)] = &[\n");
    for (code, path) in &catalogs {
        println!("cargo:rerun-if-changed={}", path.display());
        writeln!(out, "    ({code:?}, include_str!({:?})),", path.display().to_string()).expect("write to a String");
    }
    out.push_str("];\n");
    let target = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("catalogs.rs");
    std::fs::write(target, out).expect("write catalogs.rs");
}

/// `en`, `pt-BR`, `zh-Hans`: what a catalog's file name must be; anything else in locales/ isn't a language.
fn is_language_code(code: &str) -> bool {
    let (language, region) = code.split_once('-').unwrap_or((code, ""));
    (2..=3).contains(&language.len())
        && language.chars().all(|c| c.is_ascii_lowercase())
        && (region.is_empty()
            || ((2..=4).contains(&region.len())
                && region.starts_with(|c: char| c.is_ascii_uppercase())
                && region.chars().all(|c| c.is_ascii_alphabetic())))
}
