use super::*;
use culprit_collector::analytical::test_support::{
    request_memory_evidence, request_memory_evidence_cases, reservation_demand_evidence_cases,
    session_memory_evidence_cases,
};
use culprit_core::telemetry::*;

fn source_outcome(source: &CollectionDescription) -> SourceOutcome {
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    SourceOutcome { source: coverage.source.clone(), state: coverage.state, reason: None, reason_message: None }
}

fn request_report(source: &CollectionDescription, rows: &[TelemetryRecord]) -> MemoryParticipantsReport {
    let observations = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = row
            else {
                panic!("request-memory fixture")
            };
            grant.as_ref()
        })
        .collect::<Vec<_>>();
    culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence {
            context: &NativeReadContext::from_snapshot(source).unwrap(),
            observations: &observations,
        },
        Some(&source_outcome(source)),
    )
    .unwrap()
}

fn group_report() -> MemoryParticipantsReport {
    let (mut source, mut rows) = request_memory_evidence(
        DefinitionPayload::sql(
            "SELECT payload FROM dbo.private_request_input WHERE id=9264",
            culprit_core::Engine::Mssql,
        ),
        true,
    );
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
    member.kind = SharedResourceKind::Instance;
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. }) =
        &mut rows[0]
    else {
        panic!()
    };
    grant.request.as_mut().unwrap().statement_identity =
        Some(NativeIdentity::new("private-current-statement-reference"));
    grant.request.as_mut().unwrap().client.as_mut().unwrap().application = Some("private-memory-client".into());
    grant.native_properties.insert("private-native-key".into(), "private-native-value".into());
    grant.resource = Some(
        NativeIdentity::new("2")
            .qualified("kind", "sql-server-resource-governor")
            .qualified("resource_semaphore_id", "0"),
    );
    grant.workload_group = Some(NativeIdentity::new("7").qualified("kind", "sql-server-resource-governor-group"));
    grant.workload_group_tempdb = Some(MssqlWorkloadGroupTempdbObservation {
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
    assert!(grant.is_valid());
    request_report(&source, &rows)
}

fn assert_canonical_fields(report: &MemoryParticipantsReport) {
    let text = participants(report);
    for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
        for field in [&entry.title, &entry.explanation, &entry.investigation, &entry.verify] {
            assert!(
                text.contains(&super::super::brief_detail(field, OBSERVATION_DETAIL_CHARS)),
                "missing canonical field: {field}"
            );
        }
    }
    for limitation in &report.limitations {
        assert!(
            text.contains(&super::super::brief_detail(limitation, OBSERVATION_DETAIL_CHARS)),
            "missing qualification: {limitation}"
        );
    }
}

#[test]
fn group_context_replays_canonical_next_steps_without_copying_private_native_payloads() {
    let report = group_report();
    assert!(report.entries[0].qualified_request.is_some());
    let text = participants(&report);
    assert_canonical_fields(&report);
    for field in [
        t!("view.request_memory.tempdb.context", group = "7"),
        t!("view.request_memory.tempdb.current", value = "0"),
        t!("view.request_memory.tempdb.peak", value = "9007199254740993"),
        t!("view.request_memory.tempdb.violations", count = "3"),
        t!("view.request_memory.tempdb.investigation"),
        t!("view.request_memory.tempdb.verify"),
        t!("view.request_memory.tempdb.limit"),
    ] {
        assert!(text.contains(&field), "{field}");
    }
    for private in [
        "SELECT payload",
        "private_request_input",
        "9264",
        "private-memory-client",
        "private-native-key",
        "private-native-value",
        "private-current-statement-reference",
        "observation_key",
        "qualified_request",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    let restored: MemoryParticipantsReport = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
    assert_eq!(participants(&restored), text);
}

#[test]
fn reservation_and_session_memory_reports_keep_each_original_source_qualification() {
    for (source, rows) in request_memory_evidence_cases() {
        let report = request_report(&source, &rows);
        assert_canonical_fields(&report);
        assert_source_state(&report);
    }
    for (source, rows) in session_memory_evidence_cases() {
        let observations = rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::SessionMemory(memory),
                    ..
                }) = row
                else {
                    panic!("session-memory fixture")
                };
                memory.as_ref()
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::session_memory::explain(&observations, Some(&source_outcome(&source))).unwrap();
        assert_canonical_fields(&report);
        assert_source_state(&report);
        let text = participants(&report);
        assert!(!text.contains("context-only-other-db") && !text.contains("context-only-native-digest"));
        if !rows.is_empty() {
            assert!(text.contains("18446744073709551615"));
        }
    }
}

fn assert_source_state(report: &MemoryParticipantsReport) {
    let text = participants(report);
    match report.source_state {
        Some(SourceState::Partial) => assert!(text.contains(&t!("evidence.cli.source_partial"))),
        Some(SourceState::Unavailable) => assert!(text.contains(&t!("evidence.cli.source_unavailable"))),
        Some(SourceState::Empty) => assert!(report.entries.is_empty()),
        _ => {}
    }
}

#[test]
fn each_unicode_field_and_entry_is_bounded_without_hiding_later_verification() {
    let mut report = group_report();
    let original = report.entries[0].clone();
    report.entries = (0..super::super::BRIEF_FINDING_LIMIT + 1)
        .map(|index| {
            let mut entry = original.clone();
            entry.title = format!("native-participant-{index}-{}", "界".repeat(OBSERVATION_DETAIL_CHARS + 1));
            entry.explanation = "é".repeat(OBSERVATION_DETAIL_CHARS + 1);
            entry.investigation = format!("bounded investigation {index}");
            entry.verify = format!("independent verification {index}");
            entry
        })
        .collect();
    let unchanged = serde_json::to_value(&report).unwrap();
    let text = participants(&report);
    for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
        assert!(text.contains(&super::super::brief_detail(&entry.title, OBSERVATION_DETAIL_CHARS)));
        assert!(text.contains(&format!("{}…", "é".repeat(OBSERVATION_DETAIL_CHARS))));
        assert!(text.contains(&entry.investigation) && text.contains(&entry.verify));
        assert!(!text.contains(&entry.title) && !text.contains(&entry.explanation));
    }
    assert!(!text.contains(&format!("native-participant-{}-", super::super::BRIEF_FINDING_LIMIT)));
    assert_eq!(text.matches("\n• native-participant-").count(), super::super::BRIEF_FINDING_LIMIT);
    assert_eq!(serde_json::to_value(&report).unwrap(), unchanged, "rendering cannot rewrite the raw report");
}

#[test]
fn original_waiter_size_and_concurrency_advice_reuses_the_canonical_bounded_report() {
    for (index, (source, rows)) in reservation_demand_evidence_cases().into_iter().enumerate() {
        let report = request_report(&source, &rows);
        assert_canonical_fields(&report);
        let text = participants(&report);
        let expected = if index == 0 {
            t!("view.request_memory.demand.larger_investigation")
        } else {
            t!("view.request_memory.demand.several_investigation")
        };
        assert!(text.contains(&expected));
        assert!(text.contains(&t!("view.request_memory.demand.limit")));
        assert!(text.contains(&t!("view.request_memory.limit.population")));
        for absent in ["SELECT payload", "9264", "9007199254740993", "observation_key", "qualified_request"] {
            assert!(!text.contains(absent), "{absent}");
        }
        let restored: MemoryParticipantsReport =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(participants(&restored), text);
    }
}
