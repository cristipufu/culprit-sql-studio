use super::*;

fn snapshot(state: Option<MssqlTableFullText>) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.tables.truncate(1);
    snapshot.tables[0].mssql_full_text = state;
    snapshot.sources.retain(|source| source.source != source_id::TABLES);
    snapshot.sources.push(SourceOutcome {
        source: source_id::TABLES.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot
}
fn present() -> MssqlTableFullText {
    MssqlTableFullText {
        availability: MssqlCatalogAvailability::Available,
        index: Some(MssqlFullTextIndex {
            unique_index_id: Some(1),
            catalog_id: Some(3),
            enabled: Some(false),
            change_tracking: Some(MssqlFullTextChangeTracking::Manual),
            last_crawl_completed: Some(true),
        }),
    }
}
fn value<'a>(entry: &'a TableFullTextEntry, key: &str) -> &'a str {
    entry.metrics.iter().find(|metric| metric.is(key)).unwrap().text.as_deref().unwrap()
}

#[test]
fn disabled_and_last_crawl_complete_are_independent_catalog_facts_without_a_health_verdict() {
    let report = explain_snapshot(&snapshot(Some(present()))).unwrap().unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.summary, t!("view.table_full_text.present"));
    assert_eq!(value(entry, "view.table_full_text.field.enabled"), t!("view.storage.no"));
    assert_eq!(value(entry, "view.table_full_text.field.last_crawl"), t!("view.storage.yes"));
    assert_eq!(entry.qualification, t!("view.table_full_text.limit.catalog"));
    assert!(entry.observation_key.is_none());
    assert!(report.observed_from.is_none() && report.native_rows.is_none() && report.read_complete.is_none());
}

#[test]
fn native_nulls_keep_the_index_present_and_each_value_unknown() {
    let mut state = present();
    let index = state.index.as_mut().unwrap();
    index.enabled = None;
    index.change_tracking = None;
    index.last_crawl_completed = None;
    let report = explain_snapshot(&snapshot(Some(state))).unwrap().unwrap();
    assert_eq!(report.entries[0].summary, t!("view.table_full_text.present"));
    assert_eq!(report.entries[0].metrics.len(), 3);
    assert!(
        report.entries[0]
            .metrics
            .iter()
            .all(|metric| metric.text.as_deref() == Some(t!("view.database_properties.unknown").as_str()))
    );
}

#[test]
fn absence_unavailable_and_uncollected_do_not_collapse_into_a_healthy_zero() {
    for (availability, key) in [
        (MssqlCatalogAvailability::Available, "view.table_full_text.absent"),
        (MssqlCatalogAvailability::Unsupported, "view.table_full_text.unsupported"),
        (MssqlCatalogAvailability::PermissionDenied, "view.table_full_text.denied"),
        (MssqlCatalogAvailability::Unknown, "view.table_full_text.unknown"),
    ] {
        let report =
            explain_snapshot(&snapshot(Some(MssqlTableFullText { availability, index: None }))).unwrap().unwrap();
        assert_eq!(report.entries[0].summary, culprit_core::i18n::t(key, &[]));
        assert!(report.entries[0].metrics.is_empty());
    }
    assert!(explain_snapshot(&snapshot(None)).unwrap().is_none());
}

#[test]
fn all_tracking_modes_and_partial_source_are_context_without_changing_findings() {
    for (tracking, key) in [
        (MssqlFullTextChangeTracking::Manual, "view.table_full_text.tracking.manual"),
        (MssqlFullTextChangeTracking::Automatic, "view.table_full_text.tracking.automatic"),
        (MssqlFullTextChangeTracking::Off, "view.table_full_text.tracking.off"),
    ] {
        let mut state = present();
        state.index.as_mut().unwrap().change_tracking = Some(tracking);
        let mut input = snapshot(Some(state));
        input.sources.last_mut().unwrap().state = SourceState::Partial;
        let report = explain_snapshot(&input).unwrap().unwrap();
        assert_eq!(value(&report.entries[0], "view.table_full_text.field.tracking"), culprit_core::i18n::t(key, &[]));
        assert_eq!(report.source_state, Some(SourceState::Partial));
        assert!(report.limitations.contains(&t!("view.table_full_text.limit.partial")));
        let mut without = input.clone();
        without.tables[0].mssql_full_text = None;
        assert_eq!(
            serde_json::to_value(crate::analyze(input).findings).unwrap(),
            serde_json::to_value(crate::analyze(without).findings).unwrap()
        );
    }
}

#[test]
fn invalid_payload_or_foreign_vendor_and_unavailable_source_cannot_supply_positive_catalog_context() {
    let mut invalid = present();
    invalid.availability = MssqlCatalogAvailability::PermissionDenied;
    assert!(explain_snapshot(&snapshot(Some(invalid))).is_err());
    let mut foreign = snapshot(Some(present()));
    foreign.server.engine = Engine::Postgres;
    assert!(explain_snapshot(&foreign).is_err());
    let mut unavailable = snapshot(Some(present()));
    unavailable.sources.last_mut().unwrap().state = SourceState::Unavailable;
    assert!(explain_snapshot(&unavailable).is_err());
}
