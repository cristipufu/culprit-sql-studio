use super::*;
pub(crate) mod fixture;
use fixture::{Evidence, report};

#[test]
fn native_spills_zero_and_missing_keep_distinct_meanings_and_exact_counts() {
    for (value, state) in [
        (Some("9007199254740993"), CompletedSpillState::RecordedSpills),
        (Some("0"), CompletedSpillState::NoRecordedSpills),
        (None, CompletedSpillState::Unavailable),
    ] {
        let report = report(&Evidence::runtime(value), Some(&Evidence::definition()));
        let entry = &report.entries[0];
        assert_eq!(entry.state, state);
        assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::Matched);
        assert!(entry.definition.is_some());
        assert!(!entry.statement_preview.as_ref().unwrap().contains("9264"));
        let total = entry.metrics.iter().find(|metric| metric.is("view.completed_spills.metric.total"));
        assert_eq!(total.and_then(|metric| metric.text.as_deref()), value);
        assert!(report.limitations.contains(&t!("view.completed_spills.limit.role")));
        assert!(entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("1900-01-01T00:00:00.003")));
        assert!(report.limitations.contains(&t!("view.completed_spills.limit.cause")));
    }
}

#[test]
fn native_cache_incarnation_not_sql_text_or_query_hash_binds_the_definition() {
    type Change = fn(&mut MssqlCachedDefinition);
    for change in [
        (|row: &mut MssqlCachedDefinition| row.context.identity.plan_handle = "0x9999".into()) as Change,
        |row| row.context.identity.sql_handle = "0x9999".into(),
        |row| row.context.identity.plan_generation = "3".into(),
        |row| row.context.identity.created_without_offset = "2020-01-01T11:00:00.007".into(),
        |row| row.context.identity.statement_start_bytes = 2,
        |row| row.context.identity.statement_end_bytes = 10,
        |row| row.context.database_id = 8,
    ] {
        let mut definition = Evidence::definition();
        definition.change_definition(change);
        let report = report(&Evidence::runtime(Some("64")), Some(&definition));
        assert_eq!(report.entries[0].state, CompletedSpillState::RecordedSpills);
        assert_eq!(report.entries[0].statement_binding, CompletedSpillStatementBinding::DefinitionNotObserved);
        assert!(report.entries[0].definition.is_none() && report.entries[0].statement_preview.is_none());
    }
}

#[test]
fn canonical_native_keys_and_known_database_incarnation_allow_an_endpoint_alias() {
    let mut definition = Evidence::definition();
    let CollectionTarget::Database(database) = &mut definition.source.target else { panic!() };
    database.location =
        culprit_core::DatabaseLocation::Server { host: "alias.invalid".into(), port: 1433, instance: None };
    definition.change_definition(|row| {
        row.context.identity.plan_handle.make_ascii_uppercase();
        row.context.identity.plan_handle.replace_range(..2, "0x");
        row.context.identity.sql_handle.make_ascii_uppercase();
        row.context.identity.sql_handle.replace_range(..2, "0x");
        row.context.identity.created_without_offset = "2020-01-01T11:00:00.003000".into();
    });
    assert_eq!(
        report(&Evidence::runtime(Some("64")), Some(&definition)).entries[0].statement_binding,
        CompletedSpillStatementBinding::Matched
    );
}

#[test]
fn source_member_process_and_unknown_authority_cannot_supply_another_statement() {
    let runtime = Evidence::runtime(Some("64"));
    for process in [false, true] {
        let mut definition = Evidence::definition();
        let SourcePartition::InstanceLocal { member: Some(member), epoch } = &mut definition.source.partition else {
            panic!()
        };
        if process {
            *epoch = Some("another-process".into());
        } else {
            member.native = NativeIdentity::new("another-member");
        }
        definition.refresh();
        assert_eq!(
            report(&runtime, Some(&definition)).entries[0].statement_binding,
            CompletedSpillStatementBinding::SourceChanged
        );
    }
    let mut unverified = Evidence::definition();
    unverified.source.partition = SourcePartition::Target;
    unverified.refresh();
    assert_eq!(
        report(&runtime, Some(&unverified)).entries[0].statement_binding,
        CompletedSpillStatementBinding::SourceUnverified
    );
}

#[test]
fn missing_sql_does_not_erase_an_exact_binding_or_turn_native_spills_into_an_operator_claim() {
    let mut definition = Evidence::definition();
    definition.change_definition(|row| {
        row.statement.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    });
    let report = report(&Evidence::runtime(Some("64")), Some(&definition));
    let entry = &report.entries[0];
    assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::Matched);
    assert!(entry.definition.is_some());
    assert_eq!(entry.statement_unavailable, Some(PayloadUnavailable::Restricted));
    assert!(entry.statement_preview.is_none());
    assert!(report.limitations.contains(&t!("view.completed_spills.limit.cause")));
}

#[test]
fn a_changed_body_receipt_or_invalid_native_clock_is_integrity_failure() {
    let mut input = Evidence::runtime(Some("64"));
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MssqlCachedRuntime(row), .. }) =
        &mut input.records[0]
    else {
        panic!()
    };
    row.counters.get_mut(&CachedRuntimeMetric::SpilledPages).unwrap().total = "65".into();
    let rows = input.rows();
    assert!(explain(input.view(&rows), None).is_err());
    let mut input = Evidence::runtime(Some("64"));
    let SourceObservation::Snapshot(snapshot) = &mut input.source.observation else { panic!() };
    snapshot.observed_until = "2019-01-01T00:00:00Z".into();
    let rows = input.rows();
    assert!(explain(input.view(&rows), None).is_err());
}

#[test]
fn a_valid_unredacted_definition_cannot_borrow_its_redacted_receipt_and_expose_original_literals() {
    let runtime = Evidence::runtime(Some("64"));
    let mut definition = Evidence::definition();
    let raw = "SELECT amount FROM dbo.orders WHERE id=9264 AND note='credential-canary/private/path'";
    definition.change_definition(|row| {
        row.statement = MssqlCachedPayload {
            source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
            native_sha256: Some(digest(raw.as_bytes())),
            payload: DefinitionPayload::sql(raw, Engine::Mssql),
        };
    });
    let canonical_references = definition.references.clone();
    let control = report(&runtime, Some(&definition));
    let preview = control.entries[0].statement_preview.as_ref().unwrap();
    assert!(!preview.contains("9264") && !preview.contains("credential-canary/private/path"));
    definition.change_definition(|row| {
        let DefinitionPayload::Available { redaction_version, .. } = row.statement.payload else { panic!() };
        row.statement.payload =
            DefinitionPayload::Available { text: raw.into(), sha256: digest(raw.as_bytes()), redaction_version };
    });
    assert_eq!(definition.references, canonical_references, "canonicalization retains the original receipt");
    assert!(definition.records[0].is_valid(), "the supplied body has a valid SHA and native shape");
    let reference = &definition.references[0];
    let canonical = ExportedRecord::new(&definition.source, &reference.capture, definition.records[0].clone()).unwrap();
    assert_ne!(canonical.record, definition.records[0]);
    let runtime_rows = runtime.rows();
    let definition_rows = definition.rows();
    let result = explain(runtime.view(&runtime_rows), Some(definition.view(&definition_rows)));
    assert_eq!(result.unwrap_err().detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[test]
fn missing_archive_rows_keep_the_original_population_and_never_become_measured_zero() {
    let mut input = Evidence::runtime(Some("64"));
    input.records.clear();
    input.references.clear();
    input.sources[0].state = SourceState::Partial;
    input.complete = false;
    let report = report(&input, None);
    assert_eq!((report.native_rows, report.observed_rows), (1, 0));
    assert!(report.entries.is_empty());
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert_eq!(report.summary, t!("view.completed_spills.summary.missing"));
}

#[test]
fn culprit_monitoring_work_is_preserved_separately_from_application_attribution() {
    let mut input = Evidence::runtime(Some("64"));
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MssqlCachedRuntime(row), .. }) =
        &mut input.records[0]
    else {
        panic!()
    };
    row.context.origin = StatementOrigin::CulpritMonitoring;
    row.context.monitoring_template_id = Some(digest(b"template"));
    input.refresh();
    let report = report(&input, None);
    let entry = &report.entries[0];
    assert_eq!(entry.origin, StatementOrigin::CulpritMonitoring);
    assert_eq!(entry.monitoring_template_id, Some(digest(b"template")));
    assert!(entry.explanation.contains(&t!("view.completed_spills.origin.culprit")));
}

#[test]
fn native_and_authenticated_captured_rows_share_one_diagnosis_without_fabricating_receipts() {
    let input = Evidence::runtime(Some("9007199254740993"));
    let read = input.runtime_read();
    let native = explain_read(&read).unwrap();
    let captured = report(&input, None);
    assert_eq!(native.context, captured.context);
    assert_eq!(native.entries[0].identity, captured.entries[0].identity);
    assert_eq!(native.entries[0].state, captured.entries[0].state);
    assert_eq!(
        serde_json::to_value(&native.entries[0].metrics).unwrap(),
        serde_json::to_value(&captured.entries[0].metrics).unwrap()
    );
    assert_eq!(native.entries[0].title, captured.entries[0].title);
    assert_eq!(native.entries[0].explanation, captured.entries[0].explanation);
    let CompletedSpillReference::NativeRead { read_key, .. } = &native.entries[0].observation else { panic!() };
    assert_eq!(read_key, &read.read_key().unwrap());
    assert!(native.entries[0].observation.captured().is_none());
    assert_eq!(captured.entries[0].observation.captured(), Some(&input.references[0]));
    assert!(native.entries[0].statement_preview.is_none());
    assert_eq!(native.entries[0].statement_binding, CompletedSpillStatementBinding::NotRequested);
}

#[test]
fn a_native_position_from_another_read_cannot_authorize_the_same_cache_entry() {
    let input = Evidence::runtime(Some("64"));
    let read = input.runtime_read();
    let mut row = CompletedSpillRecord::native_runtime(digest(b"another-read"), &read.observations[0]).unwrap();
    let sources = input.sources.clone();
    assert!(
        explain(
            CompletedSpillEvidence {
                read_id: &read.read_id().unwrap(),
                source: &read.description,
                rows: std::slice::from_ref(&row),
                sources: &sources,
                read_complete: true
            },
            None
        )
        .is_err()
    );
    row.reference = CompletedSpillReference::NativeRead {
        read_key: read.read_key().unwrap(),
        observation_key: digest(b"another-entry"),
    };
    assert!(
        explain(
            CompletedSpillEvidence {
                read_id: &read.read_id().unwrap(),
                source: &read.description,
                rows: &[row],
                sources: &sources,
                read_complete: true
            },
            None
        )
        .is_err()
    );
}

#[test]
fn native_and_captured_rows_cannot_be_pooled_to_complete_one_declared_read() {
    let mut input = Evidence::runtime(Some("64"));
    let mut next = input.records[0].clone();
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlCachedRuntime(runtime),
        key,
        ..
    }) = &mut next
    else {
        panic!()
    };
    runtime.context.identity.plan_generation = "3".into();
    *key = runtime.context.identity.key().unwrap();
    input.records.push(next);
    let SourceObservation::Snapshot(header) = &mut input.source.observation else { panic!() };
    header.records = 2;
    header.sources[0] = StateSourceCoverage::observed("native-cache".into(), 2, true);
    input.refresh();
    let read = input.runtime_read();
    let captured = input.rows();
    let rows = vec![
        CompletedSpillRecord::native_runtime(read.read_key().unwrap(), &read.observations[0]).unwrap(),
        CompletedSpillRecord::captured(input.references[1].clone(), &input.records[1]).unwrap(),
    ];
    assert!(explain(input.view(&rows), None).is_err());
    assert!(explain(input.view(&captured), None).is_ok(), "the same genuine two-row captured source remains valid");
}

#[test]
fn a_native_source_subset_and_a_missing_body_remain_independent_limits() {
    let mut input = Evidence::runtime(Some("64"));
    let SourceObservation::Snapshot(header) = &mut input.source.observation else { panic!() };
    header.sources[0] = StateSourceCoverage::observed("native-cache".into(), 1, false);
    input.refresh();
    let mut read = input.runtime_read();
    let native = explain_read(&read).unwrap();
    assert!(native.read_complete);
    assert_eq!(native.sources[0].state, SourceState::Partial);
    assert_eq!(native.entries[0].state, CompletedSpillState::RecordedSpills);
    read.observations.clear();
    read.read_complete = false;
    let missing = explain_read(&read).unwrap();
    assert!(!missing.read_complete);
    assert_eq!((missing.native_rows, missing.observed_rows), (1, 0));
    assert!(missing.entries.is_empty());
    assert_eq!(missing.sources[0].state, SourceState::Partial);
}

#[test]
fn identical_unqualified_metadata_from_a_different_original_read_cannot_supply_a_native_position() {
    let input = Evidence::runtime(Some("64"));
    let mut first = input.runtime_read();
    first.description.partition = SourcePartition::Target;
    let mut second = first.clone();
    second.origin = CachedRuntimeReadOrigin::NativeRead {
        read_id: cached_statement_read_id(&CaptureIdentity { observation_id: "3".repeat(32), revision: None }).unwrap(),
    };
    assert_eq!(first.description, second.description);
    assert_ne!(first.read_key().unwrap(), second.read_key().unwrap());
    let rows = vec![CompletedSpillRecord::native_runtime(first.read_key().unwrap(), &second.observations[0]).unwrap()];
    let second_read_id = second.read_id().unwrap();
    let input = CompletedSpillEvidence {
        read_id: &second_read_id,
        source: &second.description,
        rows: &rows,
        sources: &input.sources,
        read_complete: true,
    };
    assert!(explain(input, None).is_err());
    assert_eq!(explain_read(&first).unwrap().entries[0].state, CompletedSpillState::RecordedSpills);
    assert_eq!(explain_read(&second).unwrap().entries[0].state, CompletedSpillState::RecordedSpills);
}

#[test]
fn a_captured_receipt_cannot_be_relabelled_with_another_original_read_owner() {
    let input = Evidence::runtime(Some("64"));
    let rows = input.rows();
    let foreign_id =
        cached_statement_read_id(&CaptureIdentity { observation_id: "4".repeat(32), revision: None }).unwrap();
    let mut evidence = input.view(&rows);
    evidence.read_id = &foreign_id;
    assert!(explain(evidence, None).is_err());
    let original = report(&input, None);
    assert_eq!(original.entries[0].observation.captured(), Some(&input.references[0]));
}

#[test]
fn optional_definitions_keep_their_independent_captured_owner_and_cannot_claim_native_runtime_provenance() {
    let runtime = Evidence::runtime(Some("64"));
    let definitions = Evidence::definition();
    assert_ne!(runtime.read_id, definitions.read_id);
    let runtime_rows = runtime.rows();
    let mut definition_rows = definitions.rows();
    assert!(explain(runtime.view(&runtime_rows), Some(definitions.view(&definition_rows))).is_ok());
    let mut borrowed_owner = definitions.view(&definition_rows);
    borrowed_owner.read_id = &runtime.read_id;
    assert!(explain(runtime.view(&runtime_rows), Some(borrowed_owner)).is_err());

    definition_rows[0].reference = CompletedSpillReference::NativeRead {
        read_key: cached_statement_read_key(&definitions.read_id, &definitions.source).unwrap(),
        observation_key: definition_rows[0].native_key().unwrap(),
    };
    assert!(explain(runtime.view(&runtime_rows), Some(definitions.view(&definition_rows))).is_err());
}

mod captured_read;
