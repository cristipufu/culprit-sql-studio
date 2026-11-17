use super::*;
mod fixture;
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
