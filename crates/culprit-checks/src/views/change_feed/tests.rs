use super::*;
fn original() -> (CollectionDescription, Vec<OperationalObservation>) {
    let context = super::super::change_capture::test_context();
    let data:Vec<MssqlChangeFeedData>=serde_json::from_value(serde_json::json!([
        {"kind":"availability","value":{"native_major":16,"native_edition":3,"has_database_state":true,"has_database_performance_state":false,"scan_layout_available":true,"error_layout_available":true}},
        {"kind":"scan-session","value":{"session_id":0,"started_without_offset":"2019-12-31T23:00:00","ended_without_offset":null,"native_phase":"Aggregate","duration_seconds":"0","error_count":"1","transactions":"2","log_records":"8","commands":null,"schema_changes":"0","empty_scans":"2","failed_sessions":"1","latency_seconds":"3","processed_commit_without_offset":null,"batch_start_lsn":"0","currently_processed_lsn":"0","batch_end_lsn":null,"processed_commit_lsn":null}},
        {"kind":"error","value":{"session_id":0,"source_task":255,"table_id":null,"table_group_id":null,"capture_phase_number":null,"entered_without_offset":"2019-12-31T23:00:00.999","error_number":2628,"severity":16,"state":1,"command_id":null,"batch_start_lsn":"0","batch_end_lsn":"0","transaction_begin_lsn":null,"transaction_commit_lsn":null,"sequence_value":"0"}}
    ])).unwrap();
    let rows = data
        .into_iter()
        .map(|data| {
            let p = MssqlChangeFeedObservation {
                context: context.clone(),
                database_id: 5,
                observed_at: "2020-01-01T00:00:00.5Z".into(),
                position: 1,
                reset: culprit_core::ResetMarker::Unknown,
                data,
            };
            OperationalObservation {
                source: p.source().into(),
                key: p.key().unwrap(),
                fact: OperationalFact::MssqlChangeFeed(Box::new(p)),
            }
        })
        .collect::<Vec<_>>();
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: "mssql-change-feed".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::ChangeFeed,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: 3,
            sources: MssqlChangeCaptureFamily::ChangeFeed
                .sources()
                .into_iter()
                .map(|name| StateSourceCoverage::collected(name.into(), 1))
                .collect(),
        }),
    };
    (source, rows)
}
fn report(source: &CollectionDescription, rows: &[OperationalObservation], complete: bool) -> Result<ChangeFeedReport> {
    explain(ChangeFeedEvidence { source, observations: &rows.iter().collect::<Vec<_>>(), read_complete: complete })
}
#[test]
fn native_aggregate_unknown_task_and_snapshot_nulls_keep_meaning_without_event_or_current_lag_claims() {
    let (source, rows) = original();
    let r = report(&source, &rows, true).unwrap();
    assert_eq!(r.source_state, SourceState::Collected);
    assert_eq!(r.entries.len(), 2);
    assert_eq!(r.entries[0].kind, ChangeCaptureEntryKind::Error);
    assert_eq!(r.entries[1].kind, ChangeCaptureEntryKind::Aggregate);
    assert_eq!(r.entries[0].fields.iter().find(|f| f.key == "source_task").unwrap().value.as_deref(), Some("255"));
    assert!(r.entries[0].fields.iter().find(|f| f.key == "table_id").unwrap().value.is_none());
    assert!(r.limitations.contains(&t!("view.change_feed.limit.delivery")));
    assert!(r.limitations.contains(&t!("view.change_feed.limit.reset")));
    assert!(serde_json::to_string(&r).unwrap().contains("2019-12-31T23:00:00.999"));
}
#[test]
fn retained_errors_are_not_deduplicated_into_unique_events_and_incomplete_body_preserves_native_counts() {
    let (mut source, mut rows) = original();
    let mut duplicate = rows[2].clone();
    let OperationalFact::MssqlChangeFeed(p) = &mut duplicate.fact else { unreachable!() };
    p.position = 2;
    duplicate.key = p.key().unwrap();
    rows.push(duplicate);
    let SourceObservation::Snapshot(h) = &mut source.observation else { unreachable!() };
    h.records = 4;
    h.sources[2].records = 2;
    let full = report(&source, &rows, true).unwrap();
    assert_eq!(full.entries.iter().filter(|e| e.kind == ChangeCaptureEntryKind::Error).count(), 2);
    rows.pop();
    let partial = report(&source, &rows, false).unwrap();
    assert_eq!(partial.source_state, SourceState::Partial);
    assert_eq!(partial.sources[2].records, 2);
    assert!(report(&source, &rows, true).is_err());
}
#[test]
fn empty_or_denied_native_history_never_becomes_a_healthy_delivery_or_enabled_feature_verdict() {
    for denied in [false, true] {
        let (mut source, mut rows) = original();
        rows.truncate(1);
        let SourceObservation::Snapshot(h) = &mut source.observation else { unreachable!() };
        h.records = 1;
        for coverage in h.sources.iter_mut().skip(1) {
            *coverage = if denied {
                StateSourceCoverage::unavailable(coverage.source.clone(), ErrorCode::PermissionDenied, None)
            } else {
                StateSourceCoverage::collected(coverage.source.clone(), 0)
            };
        }
        let r = report(&source, &rows, true).unwrap();
        assert!(r.entries.is_empty());
        assert_eq!(r.source_state, if denied { SourceState::Partial } else { SourceState::Collected });
        assert!(r.limitations.contains(&t!("view.change_feed.limit.delivery")));
        assert!(serde_json::to_value(&r).unwrap().get("enabled").is_none());
    }
}
#[test]
fn changed_family_vendor_owner_point_or_duplicate_position_refuses_the_whole_original() {
    for change in ["family", "vendor", "owner", "point", "duplicate"] {
        let (mut source, mut rows) = original();
        match change {
            "family" => {
                let SourceObservation::Snapshot(h) = &mut source.observation else { unreachable!() };
                h.dataset = SnapshotDataset::Cdc;
            }
            "vendor" => {
                let CollectionTarget::Database(db) = &mut source.target else { unreachable!() };
                db.engine = culprit_core::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = culprit_core::Engine::Postgres;
            }
            "owner" => {
                let OperationalFact::MssqlChangeFeed(p) = &mut rows[2].fact else { unreachable!() };
                p.context.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
            }
            "point" => {
                let OperationalFact::MssqlChangeFeed(p) = &mut rows[2].fact else { unreachable!() };
                p.observed_at = "2020-01-01T00:00:02Z".into();
            }
            _ => rows.push(rows[2].clone()),
        }
        assert!(report(&source, &rows, false).is_err(), "{change}");
    }
}
