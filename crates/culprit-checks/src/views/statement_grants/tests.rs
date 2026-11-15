use super::*;
use crate::statement_grant_fixture;
use culprit_core::{
    Engine, ErrorCode, source_id,
    telemetry::{PayloadUnavailable, SourcePartition, StateSourceCoverage},
};

fn read() -> StatementGrantSnapshot {
    statement_grant_fixture::read(&crate::waits::snapshot_with(Engine::Mssql))
}

#[test]
fn exact_entry_and_read_references_preserve_native_rows_without_copying_statement_payloads() {
    let read = read();
    let original = read.clone();
    let report = explain(std::slice::from_ref(&read), &[]).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.snapshot_key, read.key().unwrap());
    assert_eq!(entry.observation_key, read.observations[0].context.identity.key().unwrap());
    assert_eq!(entry.identity, read.observations[0].context.identity);
    assert!(entry.title.render().contains("SELECT value FROM dbo.grant_input"));
    assert!(entry.explanation.render().contains("2.0 GB") && entry.explanation.render().contains("40.0 MB"));
    let SourceObservation::Snapshot(description) = &read.description.observation else { panic!() };
    assert_eq!(report.reads[0].coverage, description.sources);
    assert_eq!(read, original, "derived output must not rewrite raw SQL, native clocks, values or coverage");
    assert!(serde_json::to_value(entry).unwrap().get("statement").is_none());
}

#[test]
fn independent_failed_reads_and_partial_positive_rows_keep_their_own_coverage() {
    let good = read();
    let mut failed = good.clone();
    failed.capture.observation_id = "failed-grant-read".into();
    failed.observations.clear();
    let source = failed.description.source.clone();
    let SourceObservation::Snapshot(description) = &mut failed.description.observation else { panic!() };
    description.records = 0;
    description.sources = vec![StateSourceCoverage::unavailable(source, ErrorCode::PermissionDenied, None)];
    let outcome = SourceOutcome {
        source: source_id::GRANTS.into(),
        state: SourceState::Partial,
        reason: Some("One native selection failed".into()),
        reason_message: None,
    };
    let report = explain(&[good, failed], std::slice::from_ref(&outcome)).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.reads.len(), 2);
    assert_eq!(report.sources, vec![outcome]);
    assert!(report.reads.iter().any(|read| read.coverage[0].error_code == Some(ErrorCode::PermissionDenied)));
    assert!(report.limitations.iter().any(|message| matches!(message, Message::Catalog { key, .. } if key == "view.statement_grants.limit.source_unavailable")));
}

#[test]
fn missing_statement_and_unknown_owner_keep_positive_measurements_without_ownership_claims() {
    let mut read = read();
    read.description.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    read.observations[0].statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let report = explain(std::slice::from_ref(&read), &[]).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].title.render(), t!("view.statement_grants.entry.unknown"));
    assert!(report.entries[0].metrics.iter().any(|metric| metric.is("check.memory_grant.metric.reserved")));
    assert!(
        report.limitations.iter().any(
            |message| matches!(message, Message::Catalog { key, .. } if key == "view.statement_grants.limit.owner")
        )
    );
}

#[test]
fn absent_totals_or_zero_executions_cannot_authorize_per_execution_means() {
    for missing_total in [false, true] {
        let mut read = read();
        if missing_total {
            read.observations[0].total_used_kibibytes = None;
        } else {
            read.observations[0].executions = Some("0".into());
            read.observations[0].total_grant_kibibytes = Some("0".into());
            read.observations[0].total_used_kibibytes = Some("0".into());
        }
        let report = explain(&[read], &[]).unwrap();
        assert!(!report.entries[0].metrics.iter().any(
            |metric| metric.is("check.memory_grant.metric.reserved") || metric.is("check.memory_grant.metric.used")
        ));
        assert_eq!(
            report.entries[0].explanation.render(),
            t!("view.statement_grants.entry.unmeasured") + " " + &t!("view.statement_grants.entry.last")
        );
    }
}

#[test]
fn exact_native_counts_above_floating_point_precision_are_not_rounded() {
    let mut read = read();
    let row = &mut read.observations[0];
    row.executions = Some("9007199254740993".into());
    row.total_grant_kibibytes = Some(u64::MAX.to_string());
    row.total_used_kibibytes = Some("9007199254740993".into());
    let report = explain(&[read], &[]).unwrap();
    let metrics = &report.entries[0].metrics;
    assert_eq!(
        metrics.iter().find(|metric| metric.is("check.memory_grant.metric.runs")).unwrap().text.as_deref(),
        Some("9007199254740993")
    );
    assert_eq!(
        metrics.iter().find(|metric| metric.is("view.statement_grants.metric.total_grant")).unwrap().text.as_deref(),
        Some("18446744073709551615 KiB")
    );
    assert!(report.entries[0].explanation.render().contains("9007199254740993"));
    assert!(metrics.iter().all(|metric| metric.value.is_finite()));
}

#[test]
fn unavailable_zero_handle_counters_remain_an_explicit_unmeasured_entry() {
    let mut read = read();
    let row = &mut read.observations[0];
    row.context.identity.plan_handle = "0x0000".into();
    row.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    row.executions = None;
    row.total_grant_kibibytes = None;
    row.total_used_kibibytes = None;
    row.last_grant_kibibytes = None;
    row.last_used_kibibytes = None;
    row.min_grant_kibibytes = None;
    row.min_used_kibibytes = None;
    row.max_grant_kibibytes = None;
    row.max_used_kibibytes = None;
    let report = explain(&[read], &[]).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert!(report.entries[0].metrics.is_empty());
    assert_eq!(report.entries[0].explanation.render(), t!("view.statement_grants.entry.unmeasured"));
}

#[test]
fn inconsistent_or_conflicting_original_reads_are_rejected_before_derived_claims() {
    let read = read();
    let mut conflicting = read.clone();
    conflicting.observations[0].total_used_kibibytes = Some("0".into());
    assert_eq!(read.key().unwrap(), conflicting.key().unwrap());
    assert!(explain(&[read.clone(), conflicting], &[]).is_err());
    let mut invalid = read.clone();
    let DefinitionPayload::Available { sha256, .. } = &mut invalid.observations[0].statement else { panic!() };
    *sha256 = "0".repeat(64);
    assert!(explain(&[invalid], &[]).is_err());
    let mut invalid = read;
    let SourceObservation::Snapshot(description) = &mut invalid.description.observation else { panic!() };
    description.sources[0].state = SourceState::Unavailable;
    assert!(explain(&[invalid], &[]).is_err());
}

#[test]
fn statement_titles_are_bounded_without_changing_the_original_complete_payload() {
    let mut read = read();
    let text = "SELECT ".to_string() + &"é".repeat(500);
    read.observations[0].context.source_text_bytes = Some((text.encode_utf16().count() * 2).to_string());
    read.observations[0].statement = DefinitionPayload::sql(&text, Engine::Mssql);
    let report = explain(std::slice::from_ref(&read), &[]).unwrap();
    let Message::Catalog { arguments, .. } = &report.entries[0].title else { panic!() };
    let MessageValue::Text(display) = &arguments["statement"] else { panic!() };
    assert_eq!(display.chars().count(), STATEMENT_TITLE_CHARACTERS + 1);
    assert!(display.ends_with('…'));
    assert!(
        matches!(&read.observations[0].statement, DefinitionPayload::Available { text: original, .. } if original.chars().count() > STATEMENT_TITLE_CHARACTERS)
    );
}

#[test]
fn partial_archive_decoding_preserves_the_original_header_and_readable_positive_entries() {
    let mut read = read();
    let SourceObservation::Snapshot(description) = &mut read.description.observation else { panic!() };
    description.records = 2;
    description.sources[0] = StateSourceCoverage::collected(description.sources[0].source.clone(), 2);
    let original = read.description.clone();
    assert!(
        explain(std::slice::from_ref(&read), &[]).is_err(),
        "a complete-read declaration cannot omit original rows"
    );
    let source = SourceOutcome {
        source: read.description.source.clone(),
        state: SourceState::Partial,
        reason: None,
        reason_message: None,
    };
    let report = explain_read(&[read], &[source], false).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert_eq!(report.reads[0].coverage[0].state, SourceState::Collected);
    assert_eq!(report.reads[0].coverage[0].records, 2);
    assert!(!report.reads[0].read_complete);
    let SourceObservation::Snapshot(description) = &original.observation else { panic!() };
    assert_eq!(report.reads[0].coverage, description.sources);
}
