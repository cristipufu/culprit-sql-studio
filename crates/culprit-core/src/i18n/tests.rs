use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::*;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../../locales/.tests/messages.json")).expect("messages.json")
}

fn test_lang(meta: &serde_json::Value) -> Lang {
    Lang {
        code: "test".into(),
        name: "test".into(),
        english_name: "test".into(),
        plural: match meta["plural"].as_str() {
            Some("one_below_2") => PluralRule::OneBelow2,
            _ => PluralRule::OneIs1,
        },
        decimal: meta["decimal"].as_str().unwrap().into(),
        group: meta["group"].as_str().unwrap().into(),
        messages: Default::default(),
    }
}

// The same cases the app's implementation runs (app/src/lib/message-format.test.ts): both read messages alike.
#[test]
fn every_shared_case_formats_as_the_app_formats_it() {
    let v = vectors();
    for case in v["cases"].as_array().unwrap() {
        let lang = test_lang(&v["locales"][case["locale"].as_str().unwrap()]);
        let args: Vec<(String, Arg)> = case["args"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (k.clone(), v.as_f64().map(Arg::Number).unwrap_or_else(|| Arg::Text(v.as_str().unwrap().into())))
            })
            .collect();
        let args: Vec<(&str, Arg)> = args.iter().map(|(k, a)| (k.as_str(), a.clone())).collect();
        let message = case["message"].as_str().unwrap();
        assert_eq!(format_message(message, &args, &lang), case["expected"].as_str().unwrap(), "{message}");
    }
    for malformed in v["malformed"].as_array().unwrap() {
        assert!(parse(malformed.as_str().unwrap()).is_err(), "{malformed}");
    }
}

// Every file in locales/ the build compiled in: each one reads, describes itself, and says what English says with the
// same arguments used the same way; none has a key English lacks.
#[test]
fn every_catalog_is_compiled_in_and_agrees_with_english() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(root().join("locales"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .collect();
    let compiled: BTreeSet<String> = CATALOGS.iter().map(|(code, _)| code.to_string()).collect();
    assert_eq!(compiled, on_disk, "every locales/*.json is a language the build compiled in");
    let english = english();
    for (code, text) in CATALOGS {
        let lang = Lang::parse(code, text).unwrap_or_else(|e| panic!("{e}"));
        assert!(!lang.name.is_empty() && !lang.english_name.is_empty(), "{code}.json describes itself");
        for key in lang.keys() {
            let message = lang.message(key).unwrap();
            let theirs = english.message(key).unwrap_or_else(|| panic!("{code}.json: {key} is not in en.json"));
            let ours = argument_kinds(message).unwrap_or_else(|e| panic!("{code}.json {key}: {e}"));
            assert_eq!(ours, argument_kinds(theirs).unwrap(), "{code}.json {key}: arguments differ from English");
        }
    }
}

#[test]
fn a_scope_speaks_its_language_and_restores_the_one_before() {
    assert_eq!(current().code, "en");
    let inner = scope("fr", || {
        let nested = scope("de", || current().code.clone());
        (nested, current().code.clone())
    });
    assert_eq!(inner, ("de".to_string(), "fr".to_string()));
    assert_eq!(current().code, "en");
    // A language Culprit doesn't have speaks English.
    assert_eq!(scope("xx", || current().code.clone()), "en");
    // A key no catalog has reads as itself rather than vanishing.
    assert_eq!(t("no.such.key", &[]), "no.such.key");
}

/// Every message key the code uses: `t!("..")` and `key!("..")` in Rust, and `t("..")` in the app's TypeScript.
fn keys_used() -> BTreeSet<String> {
    fn scan(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if path.is_dir() {
                if !matches!(name, "target" | "node_modules" | "dist" | "gen") {
                    scan(&path, out);
                }
                continue;
            }
            if !(name.ends_with(".rs") || name.ends_with(".ts") || name.ends_with(".tsx")) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            // Examples in comments and docs aren't uses; a call may put its key on the next line.
            let code: String = text
                .lines()
                .filter(|line| {
                    let line = line.trim_start();
                    !(line.starts_with("//") || line.starts_with('*') || line.starts_with("/*"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            for marker in ["t!(", "t(", "key!("] {
                let mut from = 0;
                while let Some(at) = code[from..].find(marker).map(|at| at + from) {
                    from = at + marker.len();
                    // `t` or `key` as a name of its own, not the end of `format!(` or `split(`.
                    if code[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                        continue;
                    }
                    let rest = code[from..].trim_start();
                    let Some(rest) = rest.strip_prefix('"') else { continue };
                    let Some(key) = rest.split('"').next() else { continue };
                    if key.contains('.') && key.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
                        out.insert(key.to_string());
                    }
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    scan(&root().join("crates"), &mut out);
    scan(&root().join("app/src"), &mut out);
    out
}

// A key the code uses that English doesn't have would show as the key itself; one English has that nothing uses is a
// translation no one reads.
#[test]
fn every_key_the_code_uses_is_in_english_and_every_english_message_is_used() {
    let english = english();
    let used = keys_used();
    let missing: Vec<&String> =
        used.iter().filter(|k| english.message(k).is_none() && !k.starts_with("no.such")).collect();
    assert!(missing.is_empty(), "keys used but not in locales/en.json: {missing:?}");
    let unused: Vec<&str> = english.keys().filter(|k| !used.contains(*k)).collect();
    assert!(unused.is_empty(), "locales/en.json keys nothing uses: {unused:?}");
}
