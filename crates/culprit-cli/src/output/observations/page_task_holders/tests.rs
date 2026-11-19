use super::super::statement::preview as sql_preview;
use super::*;
use culprit_collector::analytical::test_support::{PageTaskHolderCase, page_task_holder_evidence};
use culprit_core::{SourceOutcome, telemetry::*};

fn report(case: PageTaskHolderCase, row_available: bool, source_available: bool) -> PageTaskHolderReport {
    let (source, rows) = page_task_holder_evidence(case);
    let SourceObservation::PageTaskHolders { focus, snapshot, .. } = &source.observation else { panic!() };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageTaskHolders(row), .. }) =
        &rows[0]
    else {
        panic!()
    };
    let outcome = SourceOutcome {
        source: snapshot.sources[0].source.clone(),
        state: snapshot.sources[0].state,
        reason: None,
        reason_message: None,
    };
    culprit_checks::page_waits::explain_task_holders(
        focus,
        &NativeReadContext::from_snapshot(&source).unwrap(),
        row_available.then_some(row.as_ref()),
        source_available.then_some(&outcome),
    )
    .unwrap()
}

#[test]
fn separate_native_endpoints_keep_their_own_task_request_generations_clocks_and_page_receipt() {
    let report = report(PageTaskHolderCase::DifferentHolders, true, true);
    let text = page_task_holders(&report);
    assert_eq!(report.entries.len(), 2);
    assert!(text.find(&report.entries[0].title).unwrap() < text.find(&report.entries[1].title).unwrap());
    assert!(text.contains(&report.original_page.record_id));
    assert!(text.contains(&report.summary));
    for entry in &report.entries {
        let task = entry.task.as_ref().unwrap();
        assert_eq!(task.request_id, Some(0));
        assert!(text.contains(&task.task_address));
        assert!(text.contains(&entry.observed_at));
        assert!(text.contains(&super::super::blocking::request(entry.request.as_ref().unwrap())));
        assert!(text.contains(&entry.explanation));
        assert!(text.contains(&entry.investigation));
        assert!(text.contains(&entry.verify));
    }
    assert_ne!(report.entries[0].request, report.entries[1].request);
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
}

#[test]
fn task_only_ambiguous_foreign_and_sentinel_points_never_receive_an_invented_request_or_sql() {
    for case in [
        PageTaskHolderCase::TaskOnly,
        PageTaskHolderCase::OtherDatabase,
        PageTaskHolderCase::Ambiguous,
        PageTaskHolderCase::UnknownPointer,
        PageTaskHolderCase::Sentinel,
    ] {
        let report = report(case, true, true);
        let text = page_task_holders(&report);
        assert!(report.entries.iter().all(|entry| entry.request.is_none() && entry.statement.is_none()));
        assert!(text.contains(&t!("evidence.cli.page_holders.request_unavailable")), "{case:?}");
        assert!(text.contains(&t!("evidence.cli.page_holders.sql_not_retained")), "{case:?}");
        for entry in &report.entries {
            assert!(text.contains(&entry.explanation));
            if let Some(task) = &entry.task {
                assert!(text.contains(&task.task_address));
            } else {
                assert!(text.contains(&t!("evidence.cli.page_holders.task_unavailable")));
            }
        }
    }
}

#[test]
fn native_sql_unavailability_preserves_its_reason_without_discarding_the_qualified_point() {
    for (case, key) in [
        (PageTaskHolderCase::SqlUnavailable, "evidence.cli.page_holders.sql_source_unavailable"),
        (PageTaskHolderCase::OversizedSql, "evidence.cli.page_holders.sql_oversized"),
    ] {
        let report = report(case, true, true);
        let text = page_task_holders(&report);
        assert!(report.entries.iter().all(|entry| entry.request.is_some()));
        assert!(text.contains(&culprit_core::i18n::t(key, &[])));
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(!text.contains(&t!("evidence.cli.page_holders.sql_not_retained")));
    }
}

#[test]
fn missing_warehouse_body_and_unknown_source_remain_gaps_without_reconstructing_native_points() {
    let report = report(PageTaskHolderCase::PartialWarehouseBody, false, false);
    let text = page_task_holders(&report);
    assert!(report.entries.is_empty());
    assert!(text.contains(&t!("evidence.cli.source_unknown")));
    assert!(text.contains(&t!("view.page_holders.limit.read")));
    assert!(text.contains(&t!("view.page_holders.limit.source")));
    assert!(!text.contains("00000000BBAA0073"));
    assert!(text.contains(&t!("evidence.cli.page_holders.mapping_unavailable")));
}

#[test]
fn final_mapping_uses_its_independent_native_timestamp_and_ids_even_when_earlier_mapping_changed() {
    let mapped = report(PageTaskHolderCase::ChangedPage, true, true);
    assert_eq!(mapped.target_state, Some(PageWriterTargetState::ChangedTarget));
    let final_target = mapped.final_target.as_ref().unwrap();
    let text = page_task_holders(&mapped);
    assert!(text.contains(&final_target.observed_at));
    let header = &final_target.target.header;
    assert!(text.contains(&t!(
        "evidence.cli.page_holders.final_mapping",
        at = &final_target.observed_at,
        database = format::digits(header.database_id.to_string()),
        file = format::digits(header.file_id.to_string()),
        page = format::digits(header.page_id.to_string()),
        object = native_identifier(header.object_id.as_deref()),
        index = native_identifier(header.index_id.as_deref()),
        partition = native_identifier(header.partition_id.as_deref()),
        allocation = native_identifier(header.allocation_unit_id.as_deref())
    )));
    let missing_first = report(PageTaskHolderCase::FirstMissFinalPositive, true, true);
    assert_eq!(missing_first.target_state, Some(PageWriterTargetState::Unavailable));
    assert!(missing_first.entries[0].request.is_none());
    assert!(missing_first.entries[1].request.is_some());
    assert!(
        page_task_holders(&missing_first)
            .contains(&super::super::blocking::request(missing_first.entries[1].request.as_ref().unwrap()))
    );
}

#[test]
fn long_sql_is_only_an_escaped_bounded_text_excerpt_while_json_retains_the_native_report() {
    let mut report = report(PageTaskHolderCase::LongSql, true, true);
    let original_json = serde_json::to_string(&report).unwrap();
    let DefinitionPayload::Available { text: sql, .. } = report.entries[0].statement.as_ref().unwrap() else {
        panic!()
    };
    assert!(sql.len() > 1024);
    let text = page_task_holders(&report);
    assert!(!text.contains(sql));
    assert!(text.contains(&sql_preview(sql)));
    assert_eq!(original_json, serde_json::to_string(&report).unwrap());

    // Only the presentation payload changes here; no native relationship is reinterpreted by the renderer.
    let DefinitionPayload::Available { text, sha256, .. } = report.entries[0].statement.as_mut().unwrap() else {
        panic!()
    };
    *text = "SELECT [\u{1b}[31m]\nFROM dbo.orders".into();
    *sha256 = digest(text.as_bytes());
    let text = page_task_holders(&report);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\u{1b}"));
}

#[test]
fn entry_and_qualification_presentation_is_bounded_without_modifying_the_report() {
    let mut report = report(PageTaskHolderCase::QualifiedBoth, true, true);
    // Expand presentation only after the genuine pure report has established its native relationships.
    report.entries = vec![report.entries[0].clone(); 100];
    report.limitations = (0..100).map(|_| "λ".repeat(4096)).collect();
    let before = serde_json::to_string(&report).unwrap();
    let text = page_task_holders(&report);
    assert_eq!(text.matches(&report.entries[0].observed_at).count(), super::super::super::BRIEF_FINDING_LIMIT);
    assert_eq!(
        text.chars().filter(|character| *character == 'λ').count(),
        MAX_HOLDER_LIMITATIONS * OBSERVATION_DETAIL_CHARS
    );
    assert!(text.len() < 32 * 1024);
    assert_eq!(before, serde_json::to_string(&report).unwrap());
}
