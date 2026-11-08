use super::diff::setting_text;
use super::provenance::latest_creation;
use super::*;
use culprit_core::{Capability, Engine, EventAction, EventKind, EventSource, Report, Snapshot};

#[test]
fn setting_values_lose_trailing_zeros() {
    assert_eq!(setting_text("10.000000"), "10");
    assert_eq!(setting_text("0.500000"), "0.5");
    assert_eq!(setting_text(" ON "), "ON");
    assert_eq!(setting_text("1.2.3"), "1.2.3");
}

fn demo() -> Snapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/pg-demo.report.json");
    let report: Report = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    report.snapshot
}

#[test]
fn saved_events_render_for_the_reader_without_changing_their_evidence() {
    let dir = std::env::temp_dir().join(format!("culprit-history-language-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let current = demo();
    let mut previous = current.clone();
    previous.taken_at = "2026-10-01T00:00:00Z".into();
    previous.indexes.pop();
    let events = culprit_core::i18n::scope("fr", || diff(Some(&previous), &current, Vec::new()));
    assert!(!events.is_empty());
    keep_scan(&dir, &current, Some(&previous), &events);
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(path(&dir, &current.id)).unwrap()).unwrap();
    assert!(stored["events"].as_array().unwrap().iter().all(|e| e["title"] == ""));
    let en = culprit_core::i18n::scope("en", || read(&dir, &current.id).unwrap().events);
    let fr = culprit_core::i18n::scope("fr", || read(&dir, &current.id).unwrap().events);
    assert_eq!(en.len(), fr.len());
    assert!(en.iter().zip(&fr).any(|(en, fr)| en.title != fr.title));
    for (mut en, mut fr) in en.into_iter().zip(fr) {
        en.clear_rendered_text();
        fr.clear_rendered_text();
        assert_eq!(en, fr);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_scan_dates_the_index_it_saw_appear() {
    let dir = std::env::temp_dir().join(format!("culprit-history-new-index-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut after = demo();
    after.id = "20261011T080000.000Z".into();
    after.taken_at = "2026-10-11T08:00:00Z".into();
    let mut before = after.clone();
    before.id = "20261010T080000.000Z".into();
    before.taken_at = "2026-10-10T08:00:00Z".into();
    let added = before.indexes.remove(before.indexes.iter().position(|i| !i.is_primary_key).unwrap()).name;
    reports::save(&dir, &culprit_checks::analyze(before.clone())).unwrap();

    let changes = diff(Some(&before), &after, Vec::new());
    date_new_indexes(&dir, &mut after, &changes);
    for i in &after.indexes {
        let expected = (i.name == added).then_some("2026-10-11T08:00:00Z");
        assert_eq!(i.created_at_latest.as_deref(), expected, "{}", i.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// The false flag: someone follows Culprit's own advice (SET GLOBAL userstat = ON) and scans again; the server has
// been up for months, so every index userstat hasn't seen read was "unused" at once. Its counters count from the
// first scan that saw them, here this one: no index is old enough to call unused yet.
#[test]
fn userstat_counters_count_from_the_scans_that_saw_them() {
    let dir = std::env::temp_dir().join(format!("culprit-history-userstat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = demo();
    s.server.engine = Engine::Mysql;
    s.identity.as_mut().unwrap().engine = Engine::Mysql;
    s.server.product = "MariaDB 10.11".into();
    s.server.capabilities.retain(|c| *c != Capability::PerformanceSchema);
    s.server.capabilities.push(Capability::IndexUsage);
    s.taken_at = "2026-10-11T08:00:00Z".into();
    date_new_indexes(&dir, &mut s, &[]);
    assert!(s.indexes.iter().all(|i| i.created_at_latest.as_deref() == Some("2026-10-11T08:00:00Z")));
    // A week of scans that all counted with userstat: since the first of them.
    let mut first = s.clone();
    first.id = "20261003T080000.000Z".into();
    first.taken_at = "2026-10-03T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(first)).unwrap();
    // The next scan's own collect, dated afresh.
    s.indexes.iter_mut().for_each(|i| (i.counters_since, i.created_at_latest) = (None, None));
    date_new_indexes(&dir, &mut s, &[]);
    assert!(s.indexes.iter().all(|i| i.created_at_latest.as_deref() == Some("2026-10-03T08:00:00Z")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_age_uses_its_database_and_past_scans() {
    let dir = std::env::temp_dir().join(format!("culprit-history-scoped-age-{}", std::process::id()));
    let mut current = demo();
    let bind = |s: &mut Snapshot, host: &str| {
        s.bind_identity(culprit_core::DatabaseIdentity::server(
            s.server.engine,
            &s.server.database.clone(),
            host,
            5432,
            None,
        ));
    };
    bind(&mut current, "current.invalid");
    current.id = "20261012T080000.000Z".into();
    current.taken_at = "2026-10-12T08:00:00Z".into();
    let index = &current.indexes[0];
    let created = |at: &str| {
        ChangeEvent::new(EventKind::Index, EventSource::Scans, "created", at, "created")
            .action(EventAction::Created)
            .table(index.qualified_table())
            .object(index.name.clone())
    };
    let mut own = current.clone();
    own.id = "20261010T080000.000Z".into();
    own.taken_at = "2026-10-10T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(own.clone())).unwrap();
    // The scan kept a creation the database dated.
    let mut dated_creation = created("2026-10-09T08:00:00Z");
    dated_creation.source = EventSource::Database;
    keep_scan(&dir, &own, None, &[dated_creation]);
    let mut foreign = own.clone();
    bind(&mut foreign, "foreign.invalid");
    foreign.id = "20261011T080000.000Z".into();
    foreign.taken_at = "2026-10-11T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(foreign.clone())).unwrap();
    keep_scan(&dir, &foreign, None, &[created(&foreign.taken_at)]);
    let mut future = current.clone();
    future.id = "20261013T080000.000Z".into();
    future.taken_at = "2026-10-13T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(future.clone())).unwrap();
    keep_scan(&dir, &future, Some(&own), &[created(&future.taken_at).between(&own.taken_at)]);
    let references = date_new_indexes_traced(&dir, &mut current, &[]);
    assert_eq!(current.indexes[0].created_at_latest.as_deref(), Some("2026-10-09T08:00:00Z"));
    assert!(!references.is_empty());
    assert!(references.iter().all(|r| r.scan.id.as_deref() == Some(own.id.as_str())));
    assert!(read(&dir, &own.id).unwrap().inputs[0].database.is_some());
    // A database-dated table alteration before this database's first scan may have created its indexes.
    // A much older scan of another endpoint must not make that evidence disappear.
    foreign.id = "20261001T080000.000Z".into();
    foreign.taken_at = "2026-10-01T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(foreign)).unwrap();
    let alteration =
        ChangeEvent::new(EventKind::Table, EventSource::Database, "table", "2026-10-09T12:00:00Z", "altered")
            .table(current.indexes[0].qualified_table())
            .action(EventAction::Changed);
    date_new_indexes(&dir, &mut current, &[alteration]);
    assert_eq!(current.indexes[0].created_at_latest.as_deref(), Some("2026-10-09T12:00:00Z"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_event_sidecar_recovers_database_recorded_changes_from_the_report() {
    let dir = std::env::temp_dir().join(format!("culprit-history-recorded-recovery-{}", std::process::id()));
    let mut current = demo();
    current.id = "20261012T080000.000Z".into();
    current.taken_at = "2026-10-12T08:00:00Z".into();
    let mut report = culprit_checks::analyze(current.clone());
    let event = ChangeEvent::new(
        EventKind::Table,
        EventSource::Database,
        "procedure",
        "2026-10-11T08:00:00Z",
        "a dated alteration",
    )
    .table("public.procedure")
    .action(EventAction::Changed);
    report.evidence.as_mut().unwrap().recorded_changes.push(event.clone());
    reports::save(&dir, &report).unwrap();
    let start = epoch("2026-10-10T00:00:00Z").unwrap();
    let end = epoch(&current.taken_at).unwrap();
    assert!(events(&dir, start, end).iter().any(|e| e.id == event.id));
    assert!(read(&dir, &current.id).unwrap().events.iter().any(|e| e.id == event.id));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn userstat_history_stops_at_a_different_database() {
    let dir = std::env::temp_dir().join(format!("culprit-history-userstat-scope-{}", std::process::id()));
    let mut current = demo();
    current.server.engine = Engine::Mysql;
    current.server.product = "MariaDB 10.11".into();
    current.server.capabilities.retain(|c| *c != Capability::PerformanceSchema);
    current.server.capabilities.push(Capability::IndexUsage);
    current.bind_identity(culprit_core::DatabaseIdentity::server(
        Engine::Mysql,
        &current.server.database.clone(),
        "current.invalid",
        3306,
        None,
    ));
    current.id = "20261012T080000.000Z".into();
    current.taken_at = "2026-10-12T08:00:00Z".into();
    let mut old = current.clone();
    old.id = "20261010T080000.000Z".into();
    old.taken_at = "2026-10-10T08:00:00Z".into();
    reports::save(&dir, &culprit_checks::analyze(old.clone())).unwrap();
    old.id = "20261011T080000.000Z".into();
    old.taken_at = "2026-10-11T08:00:00Z".into();
    old.bind_identity(culprit_core::DatabaseIdentity::server(
        Engine::Mysql,
        &old.server.database.clone(),
        "foreign.invalid",
        3306,
        None,
    ));
    reports::save(&dir, &culprit_checks::analyze(old)).unwrap();
    date_new_indexes(&dir, &mut current, &[]);
    assert!(current.indexes.iter().all(|i| i.counters_since.as_deref() == Some(current.taken_at.as_str())));
    std::fs::remove_dir_all(dir).unwrap();
}

fn mssql_demo() -> Snapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Two scans of the SQL Server demo six hours apart, the server up all along.
fn two_scans() -> (Snapshot, Snapshot) {
    let mut before = mssql_demo();
    before.id = "20261011T060000.000Z".into();
    before.taken_at = "2026-10-11T06:00:00Z".into();
    before.server.started_at = Some("2026-09-01T00:00:00Z".into());
    let mut after = before.clone();
    after.id = "20261011T120000.000Z".into();
    after.taken_at = "2026-10-11T12:00:00Z".into();
    for i in after.indexes.iter_mut() {
        i.reads = i.reads.map(|r| r + 10.0);
    }
    (before, after)
}

#[test]
fn usage_counters_that_started_over_date_their_indexes() {
    // The database went offline (AUTO_CLOSE, a failover) between two scans: SQL Server dropped every index's usage
    // row, though the server never restarted.
    let (before, mut after) = two_scans();
    for i in after.indexes.iter_mut() {
        (i.reads, i.writes, i.seeks, i.scans, i.lookups) = (Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(0.0));
    }
    let events = diff(Some(&before), &after, Vec::new());
    let reset: Vec<&ChangeEvent> = events.iter().filter(|e| is_usage_reset(e)).collect();
    assert_eq!(reset.len(), 1, "one event for the whole database: {events:?}");
    assert_eq!((reset[0].table.as_deref(), reset[0].after.as_deref()), (None, Some("2026-10-11T06:00:00Z")));

    let dir = std::env::temp_dir().join(format!("culprit-history-usage-reset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    date_new_indexes(&dir, &mut after, &events);
    for i in &after.indexes {
        // Counted only since the latest they can have started over: not unread for a month.
        assert_eq!(i.counters_since.as_deref(), Some("2026-10-11T12:00:00Z"), "{}", i.name);
        assert_eq!(i.created_at_latest.as_deref(), Some("2026-10-11T12:00:00Z"), "{}", i.name);
    }
    assert!(after.tables.iter().all(|t| t.counters_since.as_deref() == Some("2026-10-11T12:00:00Z")));
    let _ = std::fs::remove_dir_all(&dir);

    // One table altered (MySQL resets its index counters): only that table's indexes.
    let (before, mut after) = two_scans();
    let altered = after.indexes.iter().find(|i| i.reads.unwrap_or(0.0) > 100.0).unwrap().qualified_table();
    for i in after.indexes.iter_mut().filter(|i| i.qualified_table() == altered) {
        i.reads = Some(1.0);
    }
    let events = diff(Some(&before), &after, Vec::new());
    let reset: Vec<&ChangeEvent> = events.iter().filter(|e| is_usage_reset(e)).collect();
    assert_eq!(reset.len(), 1);
    assert_eq!(reset[0].table.as_deref(), Some(altered.as_str()));
    let mut on_table = after.indexes.iter().find(|i| i.qualified_table() == altered).unwrap().clone();
    assert_eq!(latest_creation(&on_table, &events, None).as_deref(), Some("2026-10-11T12:00:00Z"));
    on_table.schema = "elsewhere".into();
    assert_eq!(latest_creation(&on_table, &events, None), None);

    // A restart starts every counter over, and the server's start time already says so.
    let (before, mut after) = two_scans();
    after.server.started_at = Some("2026-10-11T09:00:00Z".into());
    for i in after.indexes.iter_mut() {
        i.reads = Some(0.0);
    }
    assert!(!diff(Some(&before), &after, Vec::new()).iter().any(is_usage_reset));
    // Counters that only grew say nothing.
    let (before, after) = two_scans();
    assert!(!diff(Some(&before), &after, Vec::new()).iter().any(is_usage_reset));
}

#[test]
fn a_replicas_usage_counters_are_set_aside() {
    let mut s = mssql_demo();
    set_aside_replica_usage(&mut s);
    assert!(s.server.has(Capability::IndexUsage), "not a replica: untouched");
    assert!(s.indexes.iter().any(|i| i.reads.is_some()));

    s.settings.push(culprit_core::Setting {
        name: culprit_core::REPLICA_SETTING.into(),
        value: "readable-secondary".into(),
        default_value: None,
        scope: "database".into(),
    });
    set_aside_replica_usage(&mut s);
    assert!(!s.server.has(Capability::IndexUsage));
    let missing = s.server.missing.iter().find(|m| m.capability == Capability::IndexUsage).unwrap();
    assert!(missing.reason.contains("readable secondary"), "{}", missing.reason);
    assert!(s.indexes.iter().all(|i| i.reads.is_none() && i.writes.is_none()));
    let report = culprit_checks::analyze(s);
    assert!(!report.findings.iter().any(|f| f.check == "unused-index"));
}

#[test]
fn an_index_is_dated_only_when_something_says_it_may_be_new() {
    let s = demo();
    let i = s.indexes.iter().find(|i| !i.is_primary_key).unwrap();
    let table = i.qualified_table();
    let on_table = |kind, source, action, at: &str| {
        ChangeEvent::new(kind, source, &format!("{table}.{}", i.name), at, "t")
            .action(action)
            .table(table.clone())
            .object(i.name.clone())
    };
    let first_scan = epoch("2026-10-05T00:00:00Z");

    // Nothing says so: as old as its counters.
    assert_eq!(latest_creation(i, &[], first_scan), None);
    // A scan saw its table without it.
    let created = on_table(EventKind::Index, EventSource::Scans, EventAction::Created, "2026-10-09T12:00:00Z")
        .between("2026-10-09T06:00:00Z");
    assert_eq!(latest_creation(i, &[created], first_scan).as_deref(), Some("2026-10-09T12:00:00Z"));
    // Another index on the table appearing says nothing about this one.
    let mut other = on_table(EventKind::Index, EventSource::Scans, EventAction::Created, "2026-10-09T12:00:00Z");
    other.object = Some("ix_something_else".into());
    assert_eq!(latest_creation(i, &[other], first_scan), None);
    // The database dated a change to its table before the first scan: the index may be what changed.
    let altered = |at: &str| on_table(EventKind::Table, EventSource::Database, EventAction::Changed, at);
    assert_eq!(
        latest_creation(i, &[altered("2026-10-04T20:00:00Z")], first_scan).as_deref(),
        Some("2026-10-04T20:00:00Z")
    );
    assert_eq!(latest_creation(i, &[altered("2026-10-04T20:00:00Z")], None).as_deref(), Some("2026-10-04T20:00:00Z"));
    // After it, the scans were watching: a new index would have been seen appearing.
    assert_eq!(latest_creation(i, &[altered("2026-10-08T20:00:00Z")], first_scan), None);
}

#[test]
fn scans_notice_schema_settings_and_restarts() {
    let mut before = demo();
    before.taken_at = "2026-10-11T10:00:00Z".into();
    before.server.started_at = Some("2026-10-01T00:00:00Z".into());
    let mut after = before.clone();
    after.taken_at = "2026-10-11T12:00:00Z".into();
    after.server.started_at = Some("2026-10-11T11:30:00Z".into());
    let dropped = after.indexes.iter().position(|i| !i.is_primary_key).unwrap();
    let gone = after.indexes.remove(dropped);
    let first_table = after.tables[0].clone();
    after.columns.push(culprit_core::ColumnInfo {
        schema: first_table.schema.clone(),
        table: first_table.name.clone(),
        name: "archived_at".into(),
        data_type: "timestamptz".into(),
        nullable: true,
        default: None,
        generated: false,
        ordinal: 99,
    });
    let setting = after.settings[0].name.clone();
    after.settings[0].value = "something else".into();
    let recorded = vec![
        ChangeEvent::new(
            EventKind::Setting,
            EventSource::Database,
            "reload",
            "2026-10-11T11:00:00Z",
            "Configuration reloaded",
        )
        .action(EventAction::Changed),
    ];
    let events = diff(Some(&before), &after, recorded);
    let find = |kind: EventKind| events.iter().filter(move |e| e.kind == kind);
    let index = find(EventKind::Index).next().expect("the dropped index");
    assert_eq!(index.title, format!("Index {} was dropped from {}", gone.name, gone.qualified_table()));
    assert_eq!(index.after.as_deref(), Some("2026-10-11T10:00:00Z"), "noticed between the two scans");
    assert_eq!(index.action, Some(EventAction::Dropped));
    let column = find(EventKind::Table).next().expect("the added column");
    assert!(column.title.contains("added archived_at timestamptz"), "{}", column.title);
    let set = find(EventKind::Setting).next().expect("the changed setting");
    assert!(set.title.starts_with(&format!("{setting} changed from")), "{}", set.title);
    assert_eq!(set.at, "2026-10-11T11:00:00Z", "the reload dates it");
    assert_eq!(set.source, EventSource::Database);
    assert_eq!(find(EventKind::Setting).count(), 1, "the reload itself is folded into the change it explains");
    let restart = find(EventKind::Restart).next().expect("the restart");
    assert_eq!(restart.at, "2026-10-11T11:30:00Z");
    // Nothing changed: nothing to say.
    let mut again = after.clone();
    again.taken_at = "2026-10-11T13:00:00Z".into();
    assert!(diff(Some(&after), &again, vec![]).iter().all(|e| e.kind == EventKind::Statistics));
}

#[test]
fn missing_table_data_is_not_a_mass_drop() {
    let before = demo();
    let mut after = before.clone();
    after.taken_at = "2099-01-01T00:00:00Z".into();
    after.tables.clear();
    after.indexes.clear();
    assert!(diff(Some(&before), &after, vec![]).iter().all(|e| !matches!(e.kind, EventKind::Table | EventKind::Index)));
}

#[test]
fn scans_notice_foreign_key_constraint_and_trigger_changes() {
    let mut before = demo();
    before.taken_at = "2026-10-11T10:00:00Z".into();
    before.foreign_keys[0].delete_rule = Some("NO ACTION".into());

    let mut after = before.clone();
    after.taken_at = "2026-10-11T12:00:00Z".into();
    after.foreign_keys[0].delete_rule = Some("CASCADE".into());
    after.constraints.push(culprit_core::ConstraintIssue {
        schema: "public".into(),
        table: "orders".into(),
        name: "ck_orders_total".into(),
        kind: culprit_core::ConstraintKind::Check,
        disabled: false,
        inheritance_child: false,
    });
    let orders = after.tables.iter_mut().find(|t| t.name == "orders").unwrap();
    orders.storage.get_or_insert_with(Default::default).options.push("trigger=trg_orders_audit:IU".into());

    let events = diff(Some(&before), &after, vec![]);
    assert!(
        events.iter().any(|e| {
            e.kind == EventKind::Table
                && e.table.as_deref() == Some("public.orders")
                && e.title.contains("changed ON DELETE rule")
                && e.detail.as_deref() == Some("ON DELETE NO ACTION → ON DELETE CASCADE")
        }),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| {
            e.kind == EventKind::Table
                && e.table.as_deref() == Some("public.orders")
                && e.title.contains("Constraint ck_orders_total on public.orders is NOT VALID / untrusted")
        }),
        "{events:?}"
    );
    let trig_event = events
        .iter()
        .find(|e| e.kind == EventKind::Code && e.object.as_deref() == Some("trg_orders_audit"))
        .expect("trigger event");
    assert_eq!(trig_event.title, "Trigger trg_orders_audit was created on public.orders");
    assert_eq!(trig_event.table.as_deref(), Some("public.orders"));
    assert_eq!(trig_event.detail.as_deref(), Some("INSERT OR UPDATE"));

    let recorded_trig = ChangeEvent::new(
        EventKind::Code,
        EventSource::Database,
        "public.trg_orders_audit",
        "2026-10-11T10:30:00Z",
        "Trigger trg_orders_audit on public.orders was created",
    )
    .action(EventAction::Created)
    .table("public.orders")
    .object("public.trg_orders_audit");
    let merged = diff(Some(&before), &after, vec![recorded_trig]);
    let code_events: Vec<&ChangeEvent> = merged.iter().filter(|e| e.kind == EventKind::Code).collect();
    assert_eq!(code_events.len(), 1, "recorded trigger event should date the scan-diffed trigger event");
    assert_eq!(code_events[0].at, "2026-10-11T10:30:00Z");
    assert_eq!(code_events[0].source, EventSource::Database);
    assert_eq!(code_events[0].detail.as_deref(), Some("INSERT OR UPDATE"));
}
