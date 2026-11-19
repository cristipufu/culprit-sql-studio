use super::*;
use culprit_core::{SourceState, telemetry::*};

fn row(id: &str) -> ExecutionGrantObservation {
    let mut row = super::super::tests::observation(id, ExecutionGrantState::Granted);
    super::super::tests::associated(&mut row);
    row.resource = Some(
        NativeIdentity::new("2")
            .qualified("kind", "sql-server-resource-governor")
            .qualified("resource_semaphore_id", "0"),
    );
    row.workload_group = Some(NativeIdentity::new("7").qualified("kind", "sql-server-resource-governor-group"));
    row.workload_group_tempdb = Some(MssqlWorkloadGroupTempdbObservation {
        group_id: "7".into(),
        pool_id: "2".into(),
        statistics_started_without_offset: Some("2020-01-01T10:00:00.003".into()),
        current_kibibytes: Some("0".into()),
        peak_kibibytes: Some("9007199254740993".into()),
        limit_violations: Some("3".into()),
        stored_limits: Some(MssqlStoredTempdbDataLimits {
            fixed_mebibytes: Some("0.5".into()),
            percent: Some("10.25".into()),
        }),
    });
    assert!(row.is_valid());
    row
}
fn context() -> NativeReadContext {
    let mut context = super::super::tests::context();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { unreachable!() };
    member.kind = SharedResourceKind::Instance;
    context
}
fn report(rows: &[&ExecutionGrantObservation]) -> MemoryParticipantsReport {
    super::super::explain(
        RequestMemoryEvidence { context: &context(), observations: rows },
        Some(&SourceOutcome {
            source: culprit_core::source_id::CURRENT_REQUEST_MEMORY.into(),
            state: SourceState::Partial,
            reason: None,
            reason_message: None,
        }),
    )
    .unwrap()
}

#[test]
fn same_original_group_observation_explains_quota_history_without_charging_this_query_or_global_capacity() {
    let row = row("one");
    let original = row.clone();
    let report = report(&[&row]);
    let entry = &report.entries[0];
    assert_eq!(entry.qualified_request.as_ref(), row.request.as_ref().map(|request| &request.key));
    assert!(entry.explanation.contains(&t!("view.request_memory.tempdb.current", value = "0")));
    assert!(entry.explanation.contains(&t!("view.request_memory.tempdb.peak", value = "9007199254740993")));
    assert!(entry.explanation.contains(&t!("view.request_memory.tempdb.violations", count = "3")));
    assert!(entry.explanation.contains(&t!("view.request_memory.tempdb.fixed", value = "0.5")));
    assert!(entry.explanation.contains(&t!("view.request_memory.tempdb.percent", value = "10.25")));
    assert!(entry.investigation.ends_with(&t!("view.request_memory.tempdb.investigation")));
    assert!(entry.verify.ends_with(&t!("view.request_memory.tempdb.verify")));
    assert!(report.limitations.contains(&t!("view.request_memory.tempdb.limit")));
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(row, original);
    let json = serde_json::to_string(&report).unwrap();
    for absent in ["SELECT payload", "9264", "causal_share", "fix_sql", "severity"] {
        assert!(!json.contains(absent));
    }
}

#[test]
fn zero_unknown_lifetime_and_stored_null_or_unavailable_are_explicit_without_effective_percent_inference() {
    for (violations, stored) in [
        (None, None),
        (Some("0"), Some(MssqlStoredTempdbDataLimits { fixed_mebibytes: None, percent: None })),
        (Some("3"), Some(MssqlStoredTempdbDataLimits { fixed_mebibytes: Some("0".into()), percent: None })),
    ] {
        let mut row = row("one");
        let group = row.workload_group_tempdb.as_mut().unwrap();
        group.current_kibibytes = None;
        group.statistics_started_without_offset = None;
        group.limit_violations = violations.map(str::to_owned);
        group.stored_limits = stored.clone();
        let report = report(&[&row]);
        let text = &report.entries[0].explanation;
        assert!(text.contains(&t!("view.request_memory.tempdb.current_unknown")));
        assert!(text.contains(&t!("view.request_memory.tempdb.statistics_unknown")));
        assert_eq!(text.contains(&t!("view.request_memory.tempdb.violations", count = "0")), violations == Some("0"));
        match stored {
            None => assert!(text.contains(&t!("view.request_memory.tempdb.limits_unavailable"))),
            Some(limits) if limits.fixed_mebibytes.is_none() => {
                assert!(text.contains(&t!("view.request_memory.tempdb.no_stored_limit")))
            }
            _ => assert!(text.contains(&t!("view.request_memory.tempdb.fixed", value = "0"))),
        }
    }
}

#[test]
fn repeated_group_rows_are_not_summed_and_different_reset_contexts_do_not_become_one_lifetime() {
    let one = row("one");
    let mut two = row("two");
    two.workload_group_tempdb.as_mut().unwrap().limit_violations = Some("7".into());
    two.workload_group_tempdb.as_mut().unwrap().statistics_started_without_offset =
        Some("2020-01-02T12:00:00.007".into());
    let report = report(&[&one, &two]);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|entry| entry.explanation.contains(&t!("view.request_memory.tempdb.violations", count = "3")))
            .count(),
        1
    );
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|entry| entry.explanation.contains(&t!("view.request_memory.tempdb.violations", count = "7")))
            .count(),
        1
    );
    assert!(
        !report
            .entries
            .iter()
            .any(|entry| entry.explanation.contains(&t!("view.request_memory.tempdb.violations", count = "10")))
    );
    assert_eq!(report.limitations.iter().filter(|limit| **limit == t!("view.request_memory.tempdb.limit")).count(), 1);
}

#[test]
fn missing_or_refused_request_generation_keeps_raw_group_counters_without_attaching_their_sql_context() {
    for missing in ["request", "negative-generation"] {
        let mut row = row("one");
        if missing == "request" {
            row.request = None;
            row.scope = None;
            row.request_association = RequestGrantAssociation::MissingRequest;
            row.statement_relationship = GrantStatementRelationship::Unresolved;
        } else {
            row.request.as_mut().unwrap().session_context = Some(Box::new(ActivitySessionContext {
                lifecycle: SessionLifecycle::PoolResetPrelogin,
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: None,
            }));
        }
        assert!(row.is_valid());
        let report = report(&[&row]);
        assert_eq!(report.entries[0].qualified_request, None);
        assert!(!report.entries[0].explanation.contains(&t!("view.request_memory.tempdb.context", group = "7")));
        assert!(row.workload_group_tempdb.is_some());
    }
}

#[test]
fn an_unavailable_new_feature_keeps_previous_words_and_saved_group_facts_replay_in_each_language() {
    let mut unavailable = row("one");
    unavailable.workload_group_tempdb = None;
    let unavailable_report = report(&[&unavailable]);
    assert!(!unavailable_report.limitations.contains(&t!("view.request_memory.tempdb.limit")));
    assert!(!unavailable_report.entries[0].investigation.contains(&t!("view.request_memory.tempdb.investigation")));
    let observed = row("two");
    let restored: ExecutionGrantObservation = serde_json::from_slice(&serde_json::to_vec(&observed).unwrap()).unwrap();
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || assert_eq!(report(&[&observed]), report(&[&restored])));
    }
}
