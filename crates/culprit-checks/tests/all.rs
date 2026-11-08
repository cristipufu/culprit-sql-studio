//! Every integration test file in `tests/`, as a module of this one test binary: one link (and one scan of a fresh
//! binary) for all of them, where a binary per file relinks every one after each change. A new file in `tests/` is
//! listed here, or `every_test_file_is_listed` fails.

mod actionability;
mod actual_words;
mod brief;
mod bursts;
mod chatty;
mod config_words;
mod counter_age;
mod dashboard;
mod deadlock_words;
mod evidence_contracts;
mod heap_tables;
mod index_advice;
mod index_false_flags;
mod index_words;
mod mariadb;
mod multi_tenant;
mod old_versions;
mod one_off;
mod partitions;
mod pins;
mod plan_cache;
mod plan_warnings;
mod plugin;
mod pressure_config_maintenance;
mod provider_audit;
mod query_checks;
mod registry;
mod signal_false_flags;
mod signal_words;
mod sql_lint_words;
mod sqlite;
mod tuning_words;
mod waits_words;

/// Record `source`'s outcome in `s`, in place of any the scan recorded for it.
pub fn record(s: &mut culprit_core::Snapshot, source: &str, state: culprit_core::SourceState, reason: Option<&str>) {
    s.sources.retain(|o| o.source != source);
    s.sources.push(culprit_core::SourceOutcome { source: source.into(), state, reason: reason.map(str::to_string) });
}

/// Make `w` a window of the last `hours`: its length, and a start that agrees with its end (a window whose bounds and
/// length disagree has no length Culprit stands behind).
pub fn last_hours(w: &mut culprit_core::Workload, hours: f64) {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    (w.windowed, w.window_hours) = (true, hours);
    w.window_start = w.window_end.as_deref().and_then(epoch_seconds).map(|end| iso_from_epoch(end - hours * 3600.0));
}

#[test]
fn every_test_file_is_listed() {
    let listed = include_str!("all.rs");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for entry in std::fs::read_dir(&dir).expect("tests/") {
        let path = entry.expect("an entry").path();
        let Some(name) =
            path.file_stem().and_then(|n| n.to_str()).filter(|_| path.extension().is_some_and(|e| e == "rs"))
        else {
            continue;
        };
        if name != "all" {
            assert!(
                listed.contains(&format!("\nmod {name};")),
                "tests/{name}.rs isn't listed in tests/all.rs: add `mod {name};`"
            );
        }
    }
}
