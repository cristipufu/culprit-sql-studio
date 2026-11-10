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
        plural: PluralRule::for_locale(meta["plural"].as_str().unwrap()).expect("a CLDR rule set"),
        decimal: meta["decimal"].as_str().unwrap().into(),
        group: meta["group"].as_str().unwrap().into(),
        direction: meta["direction"].as_str().unwrap_or("ltr").into(),
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

#[test]
fn every_cldr_sample_selects_its_recorded_category() {
    let samples: Vec<(String, f64, String)> =
        serde_json::from_str(include_str!("../../../../locales/.tests/plurals.json")).unwrap();
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../../../../locales/.cldr/cardinals.json")).unwrap();
    let expected: BTreeSet<&str> = data["languages"].as_object().unwrap().keys().map(String::as_str).collect();
    let covered: BTreeSet<&str> = samples.iter().map(|(language, _, _)| language.as_str()).collect();
    assert_eq!(covered, expected, "every CLDR language has independent samples");
    for (language, number, expected) in samples {
        let rule = PluralRule::for_locale(&language).unwrap();
        assert_eq!(rule.category(number), expected, "{language}: {number}");
    }
}

#[test]
fn catalog_tags_allow_scripts_regions_and_variants_with_canonical_spelling() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../../../../locales/.tests/language-tags.json")).unwrap();
    for (kind, valid) in [("valid", true), ("invalid", false)] {
        for code in cases[kind].as_array().unwrap() {
            let code = code.as_str().unwrap();
            assert_eq!(is_language_code(code), valid, "{code}");
        }
    }
}

#[test]
fn english_fallback_keeps_its_reading_direction_inside_an_rtl_language() {
    let mut lang = test_lang(&serde_json::json!({ "plural": "ar", "decimal": ".", "group": ",", "direction": "rtl" }));
    lang.messages.insert("ui.common.copy".into(), "نسخ".into());
    scope("en", || {
        CURRENT.with(|current| *current.borrow_mut() = Some(Arc::new(lang)));
        assert_eq!(t("ui.common.copy", &[]), "نسخ");
        assert_eq!(t("ui.sidebar.language", &[]), "\u{2066}Language\u{2069}");
    });
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
    // A language Culprit doesn't have speaks English, and can't become the process's (tests never set one: every
    // other test reads English).
    assert_eq!(scope("xx", || current().code.clone()), "en");
    assert!(!set_language("xx"));
    assert_eq!(current().code, "en");
    // A key no catalog has reads as itself rather than vanishing.
    assert_eq!(t("no.such.key", &[]), "no.such.key");
}

/// Every message key the code uses: `t!("..")` and `key!("..")` in Rust, and `t("..")`, `tx("..")` and `key("..")` in
/// the app's TypeScript.
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
            for marker in ["t!(", "message!(", "t(", "tx(", "key!(", "key("] {
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
    scan(&root().join("app/src-tauri/src"), &mut out);
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
