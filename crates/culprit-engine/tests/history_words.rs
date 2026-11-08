//! What the timeline says about the changes Culprit's scans noticed, word for word: every kind of change between two
//! scans (tables, columns, indexes, foreign keys, constraints, triggers, settings, version, restarts, counters that
//! started over, statistics), and why a replica's usage counters are set aside. Their words come from the language
//! catalog (locales/en.json); the pins in `tests/words/` hold the English to what they said when it was in code.

use culprit_core::{Capability, ChangeEvent, ConstraintIssue, ConstraintKind, ForeignKey, Report, Setting, Snapshot};
use culprit_engine::history;

use crate::export_words::pin;

fn demo() -> Snapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/pg-demo.report.json");
    let report: Report = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    report.snapshot
}

fn said(events: &[ChangeEvent]) -> String {
    events
        .iter()
        .map(|e| {
            format!(
                "{} · {:?} {:?} · {}\n  title: {}\n  detail: {}",
                e.id,
                e.kind,
                e.action,
                e.after.as_deref().unwrap_or("-"),
                e.title,
                e.detail.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn setting(name: &str, value: &str) -> Setting {
    Setting { name: name.into(), value: value.into(), default_value: None, scope: "server".into() }
}

/// Two scans a day apart, the second seeing every kind of change in the schema, settings and server.
fn pair() -> (Snapshot, Snapshot) {
    let mut before = demo();
    before.id = "20261010T080000.000Z".into();
    before.taken_at = "2026-10-10T08:00:00Z".into();
    before.server.started_at = Some("2026-10-01T00:00:00Z".into());
    let mut after = before.clone();
    after.id = "20261011T080000.000Z".into();
    after.taken_at = "2026-10-11T08:00:00Z".into();
    after.server.started_at = Some("2026-10-11T07:30:00Z".into());
    after.server.version = "19.0".into();
    // A table gone and one new.
    let mut added = after.tables[0].clone();
    added.name = "brand_new".into();
    after.tables.push(added);
    let dropped = before.tables[0].clone();
    after.tables.retain(|t| t.name != dropped.name);
    // Columns: one change on a table, several on another.
    let tables: Vec<(String, String)> = {
        let mut seen: Vec<(String, String)> = Vec::new();
        for c in &after.columns {
            let key = (c.schema.clone(), c.table.clone());
            if c.table != dropped.name && !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen
    };
    let (one, many) = (&tables[0], &tables[1]);
    let col = after.columns.iter_mut().find(|c| (&c.schema, &c.table) == (&one.0, &one.1)).unwrap();
    col.data_type = "bigint".into();
    col.nullable = !col.nullable;
    let mut extra = after.columns.iter().find(|c| (&c.schema, &c.table) == (&many.0, &many.1)).unwrap().clone();
    extra.name = "added_col".into();
    extra.nullable = false;
    after.columns.push(extra);
    let gone = after.columns.iter().position(|c| (&c.schema, &c.table) == (&many.0, &many.1)).unwrap();
    after.columns.remove(gone);
    // Indexes: created, dropped, renamed, disabled, enabled again, redefined.
    let on_kept: Vec<usize> =
        (0..after.indexes.len()).filter(|i| after.indexes[*i].table != dropped.name).take(6).collect();
    let [created, dropped_ix, renamed, disabled, enabled, redefined] = on_kept[..] else { panic!("six indexes") };
    before.indexes[created].name = "ix_was_never".into();
    after.indexes[dropped_ix].name = "ix_gone_now".into();
    let mut renamed_before = before.indexes[renamed].clone();
    renamed_before.name = "ix_old_name".into();
    before.indexes[renamed] = renamed_before;
    after.indexes[renamed].definition = None;
    after.indexes[disabled].is_disabled = true;
    before.indexes[enabled].is_disabled = true;
    after.indexes[redefined].columns.push("extra_col".into());
    after.indexes[redefined].definition = None;
    before.indexes[redefined].definition = None;
    before.indexes[created].definition = None;
    after.indexes[created].include = vec!["inc".into()];
    after.indexes[created].filter = Some("status = 'open'".into());
    after.indexes[created].is_unique = true;
    // Foreign keys: added, dropped, a changed delete rule.
    let fk = |name: &str, table: &str, column: &str, rule: Option<&str>| ForeignKey {
        name: name.into(),
        schema: one.0.clone(),
        table: table.into(),
        columns: vec![column.into()],
        ref_schema: many.0.clone(),
        ref_table: many.1.clone(),
        ref_columns: vec!["id".into()],
        delete_rule: rule.map(str::to_string),
    };
    before.foreign_keys.push(fk("fk_ruled", &one.1, "a_id", Some("no_action")));
    after.foreign_keys.push(fk("fk_ruled", &one.1, "a_id", Some("cascade")));
    after.foreign_keys.push(fk("fk_added", &one.1, "b_id", None));
    before.foreign_keys.push(fk("fk_dropped", &one.1, "c_id", Some("set_null")));
    // Constraints: one new (untrusted), one switched off, one validated.
    let con = |name: &str, disabled: bool, kind: ConstraintKind| ConstraintIssue {
        schema: one.0.clone(),
        table: one.1.clone(),
        name: name.into(),
        kind,
        disabled,
        inheritance_child: false,
    };
    after.constraints.push(con("ck_new", false, ConstraintKind::Check));
    after.constraints.push(con("fk_off", true, ConstraintKind::ForeignKey));
    before.constraints.push(con("fk_off", false, ConstraintKind::ForeignKey));
    before.constraints.push(con("ck_validated", false, ConstraintKind::Check));
    // Triggers: created, redefined, dropped.
    let storage = |s: &mut Snapshot, options: &[&str]| {
        let t = s.tables.iter_mut().find(|t| t.schema == one.0 && t.name == one.1).unwrap();
        t.storage.get_or_insert_with(Default::default).options = options.iter().map(|o| o.to_string()).collect();
    };
    storage(&mut before, &["trigger=tr_changed:I", "trigger=tr_dropped:UD"]);
    storage(&mut after, &["trigger=tr_changed:IU", "trigger=tr_new:D", "trigger=tr_odd:X"]);
    // Settings: a few.
    before.settings.push(setting("long_query_time", "10.000000"));
    after.settings.push(setting("long_query_time", "2.500000"));
    (before, after)
}

#[test]
fn every_change_between_two_scans_says_what_it_said() {
    let (before, after) = pair();
    let mut out = vec![said(&history::diff(Some(&before), &after, Vec::new()))];
    // The first scan: the server started, and statistics dated since.
    let mut first = after.clone();
    for t in first.tables.iter_mut().take(2) {
        t.last_analyze = Some("2026-10-11T06:00:00Z".into());
    }
    out.push(said(&history::diff(None, &first, Vec::new())));
    // Many settings at once fold into one change.
    let mut many = before.clone();
    for i in 0..14 {
        many.settings.push(setting(&format!("setting_{i:02}"), "1"));
    }
    let mut changed = many.clone();
    changed.taken_at = "2026-10-11T08:00:00Z".into();
    for s in changed.settings.iter_mut().filter(|s| s.name.starts_with("setting_")) {
        s.value = "2".into();
    }
    out.push(said(&history::diff(Some(&many), &changed, Vec::new())));
    pin("history-changes", &out.join("\n=== next\n"));
}

#[test]
fn counters_that_started_over_say_what_they_said() {
    let mut before = demo();
    before.taken_at = "2026-10-10T08:00:00Z".into();
    before.server.capabilities.push(Capability::IndexUsage);
    if let Some(p) = before.pressure.as_mut() {
        p.uptime_hours = None;
    }
    before.workload.as_mut().unwrap().window_start = Some("2026-10-01T00:00:00Z".into());
    for i in &mut before.indexes {
        i.reads = Some(1000.0);
    }
    let mut after = before.clone();
    after.taken_at = "2026-10-11T08:00:00Z".into();
    // Most indexes counted less: the database's counters started over.
    let mut all = after.clone();
    for i in &mut all.indexes {
        i.reads = Some(10.0);
    }
    // One table's: its own did.
    let mut one = after.clone();
    let table = one.indexes[0].table.clone();
    for i in one.indexes.iter_mut().filter(|i| i.table == table) {
        i.reads = Some(10.0);
    }
    // pg_stat_statements says when it was reset.
    let mut reset = after.clone();
    let w = reset.workload.as_mut().unwrap();
    w.window_start = Some("2026-10-11T07:00:00Z".into());
    let out = [
        said(&history::diff(Some(&before), &all, Vec::new())),
        said(&history::diff(Some(&before), &one, Vec::new())),
        said(&history::diff(Some(&before), &reset, Vec::new())),
    ];
    pin("history-counters", &out.join("\n=== next\n"));
}

#[test]
fn a_replicas_usage_counters_say_why_they_are_set_aside() {
    let mut out = Vec::new();
    for role in ["standby", "readable-secondary"] {
        let mut s = demo();
        s.settings.push(setting(culprit_core::REPLICA_SETTING, role));
        history::set_aside_replica_usage(&mut s);
        let missing = s.server.missing.iter().find(|m| m.capability == Capability::IndexUsage).unwrap();
        out.push(format!("reason: {}\nfix: {}", missing.reason, missing.fix.as_deref().unwrap_or("-")));
    }
    pin("history-replica", &out.join("\n"));
}
