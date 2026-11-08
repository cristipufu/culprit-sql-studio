//! Compiles every language catalog (`locales/<code>.json` at the repository root) into the library: a language is one
//! file, picked up by the next build, and nothing is read from disk when Culprit runs.

use std::fmt::Write as _;
use std::path::PathBuf;

#[path = "src/i18n/language_code.rs"]
mod language_code;

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
            language_code::is_language_code(&code).then_some((code, path))
        })
        .collect();
    catalogs.sort();
    let mut out = String::from("/// Every `locales/<code>.json`, by language code, as the build found them.\n");
    out.push_str("pub(crate) static CATALOGS: &[(&str, &str)] = &[\n");
    for (code, path) in &catalogs {
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read_to_string(path).expect("read a language catalog");
        let mut catalog: serde_json::Value = serde_json::from_str(&text).expect("a JSON language catalog");
        catalog["_meta"].as_object_mut().expect("catalog metadata").remove("source_hashes");
        let runtime = serde_json::to_string(&catalog).expect("serialize catalog messages");
        writeln!(out, "    ({code:?}, {runtime:?}),").expect("write to a String");
    }
    out.push_str("];\n");
    let target = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("catalogs.rs");
    std::fs::write(target, out).expect("write catalogs.rs");
}
