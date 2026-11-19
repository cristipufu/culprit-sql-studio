use super::*;
use culprit_collector::analytical::test_support::{PageWriterCase, page_writer_evidence};
use culprit_core::{SourceOutcome, telemetry::*};

fn report(case: PageWriterCase, row_available: bool, source_available: bool) -> PageWaitWriterReport {
    report_with(case, row_available, source_available, |_| {})
}

fn report_with(
    case: PageWriterCase,
    row_available: bool,
    source_available: bool,
    change: impl FnOnce(&mut PageWaitWriterObservation),
) -> PageWaitWriterReport {
    let (source, rows) = page_writer_evidence(case);
    let SourceObservation::PageWaitWriters { focus, snapshot, .. } = &source.observation else { panic!() };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWaitWriters(row), .. }) =
        &rows[0]
    else {
        panic!()
    };
    let mut row = row.as_ref().clone();
    change(&mut row);
    let outcome = SourceOutcome {
        source: snapshot.sources[0].source.clone(),
        state: snapshot.sources[0].state,
        reason: None,
        reason_message: None,
    };
    culprit_checks::page_waits::explain_writers(
        focus,
        &NativeReadContext::from_snapshot(&source).unwrap(),
        row_available.then_some(&row),
        source_available.then_some(&outcome),
    )
    .unwrap()
}

#[test]
fn writer_and_reader_keep_the_original_target_guarded_sql_and_distinct_investigations() {
    let report = report_with(PageWriterCase::TargetWriter, true, true, |row| {
        for (index, candidate) in row.candidates.iter_mut().enumerate() {
            candidate.request.as_mut().unwrap().client = Some(ActivityClient {
                application: Some(format!("source-application-{index}")),
                user: Some(format!("source-user-{index}")),
                host: Some(format!("source-host-{index}")),
            });
        }
    });
    let before = serde_json::to_string(&report).unwrap();
    let text = page_wait_writers(&report);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].relation, PageWriterRelation::TargetObjectWrite);
    assert_eq!(report.entries[1].relation, PageWriterRelation::ReadOnlyPlan);
    assert!(text.contains("INSERT INTO dbo.orders"));
    assert!(text.contains("SELECT customer_id FROM dbo.orders"));
    assert!(!text.contains("9264"));
    assert!(text.contains(&report.original_page.record_id));
    let mut target = String::new();
    original_target(&mut target, &report.original_target);
    assert!(text.contains(&target));
    let object = report.original_target.object.as_ref().unwrap();
    assert!(text.contains(&format!("{}.{}", object.schema, object.name)));
    assert!(text.contains(&label(object.index_name.as_deref())));
    for entry in &report.entries {
        assert!(text.contains(&entry.title));
        assert!(text.contains(&entry.explanation));
        assert!(text.contains(&statement::sql(entry.sql.as_ref())));
        assert!(text.contains(&entry.investigation));
        assert!(text.contains(&entry.verify));
        if let Some(client) = &entry.client {
            for value in [&client.application, &client.user, &client.host].into_iter().flatten() {
                assert!(text.contains(&statement::preview(value)));
            }
        }
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert_eq!(before, serde_json::to_string(&report).unwrap());
}

#[test]
fn changed_or_missing_requests_and_missing_capture_bodies_do_not_borrow_current_sql() {
    for case in [PageWriterCase::ChangedRequest, PageWriterCase::MissingRequest, PageWriterCase::PermissionUnavailable]
    {
        let report = report(case, true, true);
        let text = page_wait_writers(&report);
        assert!(report.entries.iter().all(|entry| entry.sql.is_none() && entry.client.is_none()));
        assert!(text.contains(&t!("evidence.cli.page_holders.sql_not_retained")));
        assert!(!text.contains("INSERT INTO") && !text.contains("SELECT customer_id"));
        for entry in &report.entries {
            assert!(text.contains(&entry.explanation));
            assert!(text.contains(&entry.investigation));
        }
    }
    let missing = report(PageWriterCase::TargetWriter, false, false);
    let text = page_wait_writers(&missing);
    assert!(missing.entries.is_empty());
    assert!(text.contains(&t!("evidence.cli.source_unknown")));
    assert!(text.contains(&t!("view.page_writers.limit.read")));
    assert!(text.contains(&t!("view.page_writers.limit.source")));
    assert!(!text.contains("INSERT INTO"));
}

#[test]
fn native_sql_unavailability_keeps_its_specific_reason_and_qualified_client_context() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let report = report_with(PageWriterCase::PayloadUnavailable, true, true, |row| {
            for candidate in &mut row.candidates {
                candidate.plan_identity = None;
                candidate.binding = None;
                candidate.relation = PageWriterRelation::Unavailable;
                candidate.plan = DefinitionPayload::Unavailable { reason };
                candidate.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason };
            }
        });
        let text = page_wait_writers(&report);
        assert!(report.entries.iter().all(|entry| entry.sql == Some(DefinitionPayload::Unavailable { reason })));
        assert!(text.contains(&statement::sql(Some(&DefinitionPayload::Unavailable { reason }))));
        assert!(!text.contains(&t!("evidence.cli.page_holders.sql_not_retained")));
        assert!(text.contains(&t!("evidence.cli.source_partial")));
    }
}

#[test]
fn negative_mars_generation_withdraws_sql_and_client_labels_before_the_cli_renders() {
    let report = report_with(PageWriterCase::TargetWriter, true, true, |row| {
        row.candidates[0].request.as_mut().unwrap().client = Some(ActivityClient {
            application: Some("private-withdrawn-application".into()),
            user: Some("private-withdrawn-user".into()),
            host: Some("private-withdrawn-host".into()),
        });
        row.candidates[1].request.as_mut().unwrap().session_context = Some(Box::new(ActivitySessionContext {
            lifecycle: SessionLifecycle::PoolResetPrelogin,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
    });
    assert!(report.entries.iter().all(|entry| entry.sql.is_none() && entry.client.is_none()));
    let text = page_wait_writers(&report);
    assert!(!text.contains("private-withdrawn") && !text.contains("INSERT INTO"));
    assert!(text.contains(&t!("view.page_writers.request.unverified")));
}

#[test]
fn a_later_mapping_change_does_not_replace_the_original_mapped_target_or_make_a_writer_verdict() {
    let report = report(PageWriterCase::ChangedTarget, true, true);
    let text = page_wait_writers(&report);
    assert_eq!(report.target_state, Some(PageWriterTargetState::ChangedTarget));
    assert!(text.contains(&t!("view.page_writers.target.changed")));
    let mut target = String::new();
    original_target(&mut target, &report.original_target);
    assert!(text.contains(&target));
    assert!(report.entries.iter().all(|entry| entry.relation == PageWriterRelation::Unavailable));
    assert!(!text.contains("INSERT INTO"));
}

#[test]
fn sql_and_database_labels_are_escaped_bounded_excerpts_without_changing_the_report() {
    let mut report = report(PageWriterCase::TargetWriter, true, true);
    let sql = format!("SELECT [\u{1b}[31m]\nFROM [{}]", "λ".repeat(4096));
    report.entries[0].sql = Some(DefinitionPayload::Available {
        sha256: digest(sql.as_bytes()),
        text: sql.clone(),
        redaction_version: REDACTION_VERSION,
    });
    report.entries[0].client = Some(ActivityClient {
        application: Some(format!("app\u{1b}[0m\n{}", "λ".repeat(512))),
        user: Some("user\r\tname".into()),
        host: Some("host\u{7}name".into()),
    });
    report.original_target.object.as_mut().unwrap().schema = "schema\u{1b}[31m\nname".into();
    report.source_reason = Some(format!("reason\u{1b}[31m\n{}", "λ".repeat(4096)));
    let before = serde_json::to_string(&report).unwrap();
    let text = page_wait_writers(&report);
    assert!(!text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\r') && !text.contains('\t'));
    assert!(text.contains("\\u{1b}"));
    assert!(text.contains(&statement::preview(&sql)));
    assert!(!text.contains(&sql));
    assert!(text.len() < 16 * 1024);
    assert_eq!(before, serde_json::to_string(&report).unwrap());
}

#[test]
fn candidate_counts_and_finite_qualifications_bound_text_without_shortening_the_raw_report() {
    let mut report = report(PageWriterCase::TargetWriter, true, true);
    report.entries = vec![report.entries[0].clone(); 100];
    report.limitations = (0..100).map(|_| "λ".repeat(4096)).collect();
    let before = serde_json::to_string(&report).unwrap();
    let text = page_wait_writers(&report);
    assert_eq!(
        text.matches(&statement::sql(report.entries[0].sql.as_ref())).count(),
        super::super::super::BRIEF_FINDING_LIMIT
    );
    assert!(text.contains(&t!(
        "evidence.cli.page_writers.counts",
        shown = format::digits(super::super::super::BRIEF_FINDING_LIMIT.to_string()),
        total = "100"
    )));
    assert_eq!(
        text.chars().filter(|character| *character == 'λ').count(),
        MAX_WRITER_LIMITATIONS * OBSERVATION_DETAIL_CHARS
    );
    assert!(text.len() < 32 * 1024);
    assert_eq!(before, serde_json::to_string(&report).unwrap());
}
