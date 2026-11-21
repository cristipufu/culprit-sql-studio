use culprit_checks::managed_instance_resources::{ManagedInstanceResourceEvidence, explain};
use culprit_collector::analytical::test_support::{ManagedInstanceResourceCase, managed_instance_resource_case};
use culprit_core::{SourceState, t, telemetry::*};
fn rows(records: Vec<TelemetryRecord>) -> Vec<ResourceMeasurement> {
    records
        .into_iter()
        .map(|record| match record {
            TelemetryRecord::ResourceSample(row) => row,
            _ => panic!(),
        })
        .collect()
}
#[test]
fn original_native_intervals_preserve_wide_storage_and_io_totals_without_pressure_or_query_ownership() {
    let (source, records) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    let rows = rows(records);
    let report =
        explain(ManagedInstanceResourceEvidence { source: &source, observations: &rows, read_complete: true }).unwrap();
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.source_state, SourceState::Collected);
    assert_eq!(report.from.as_deref(), Some("2020-01-01T06:00:00Z"));
    assert_eq!(report.observed_at.as_deref(), Some("2020-01-01T06:10:00Z"));
    let fields = &report.intervals[0].fields;
    assert_eq!(
        fields.iter().find(|field| field.key == "io_request").unwrap().native_value.as_deref(),
        Some("9007199254740993")
    );
    assert_eq!(
        fields.iter().find(|field| field.key == "storage_space_used_mb").unwrap().native_value.as_deref(),
        Some("9007199254740992.25")
    );
    let value = serde_json::to_string(&report).unwrap();
    for private in [
        "current_reader",
        "private-current-machine",
        "private-current-instance",
        "private-current-process",
        "payload_sha256",
        "manifest_sha256",
        "healthy",
        "severity",
    ] {
        assert!(!value.contains(private), "{private}");
    }
    assert!(report.limitations.iter().any(|value| value == &t!("view.mi_resources.limit.meaning")));
}
#[test]
fn denied_unsupported_empty_and_partial_native_sources_keep_their_independent_qualification() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, records) = managed_instance_resource_case(case);
        let rows = rows(records);
        let report =
            explain(ManagedInstanceResourceEvidence { source: &source, observations: &rows, read_complete: true })
                .unwrap();
        let SourceObservation::MssqlManagedInstanceResources(history) = &source.observation else { panic!() };
        assert_eq!(report.source_state, history.coverage.state, "{case:?}");
        assert_eq!(report.recorded_rows, Some(history.coverage.records));
        if matches!(
            case,
            ManagedInstanceResourceCase::Denied
                | ManagedInstanceResourceCase::Unsupported
                | ManagedInstanceResourceCase::NoRows
        ) {
            assert!(report.intervals.is_empty());
            assert_ne!(report.source_state, SourceState::Empty);
        }
        if case == ManagedInstanceResourceCase::CpuUnsupported {
            assert_eq!(report.intervals[0].fields[0].availability, MssqlCatalogAvailability::Unsupported);
            assert_eq!(report.intervals[0].fields[0].value, None);
            assert_eq!(
                report.intervals[0].fields[0].missing_reason.as_deref(),
                Some(t!("view.mi_resources.field_unsupported").as_str())
            );
            assert!(
                report.intervals[0]
                    .fields
                    .iter()
                    .find(|field| field.key == "io_request")
                    .unwrap()
                    .native_value
                    .is_some()
            );
        }
        if case == ManagedInstanceResourceCase::NativeNull {
            assert_eq!(report.intervals[0].fields[0].availability, MssqlCatalogAvailability::Available);
            assert_eq!(report.intervals[0].fields[0].value, None);
        }
        if case == ManagedInstanceResourceCase::Partial {
            assert_eq!(report.native_rows, Some(2));
            assert_eq!(report.observed_rows, 1);
            assert_eq!(report.native_source.as_ref().unwrap().stopped, Some(CollectionStop::RowLimit));
        }
    }
}
#[test]
fn missing_original_bodies_never_rewrite_native_counts_or_become_a_quiet_interval() {
    let (source, _) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    assert!(
        explain(ManagedInstanceResourceEvidence { source: &source, observations: &[], read_complete: true }).is_err()
    );
    let report =
        explain(ManagedInstanceResourceEvidence { source: &source, observations: &[], read_complete: false }).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.recorded_rows, Some(1));
    assert_eq!(report.observed_rows, 0);
    assert!(report.intervals.is_empty());
    assert!(!report.read_complete);
}
#[test]
fn foreign_source_unknown_reader_or_changed_native_values_cannot_supply_the_report() {
    for label in ["source", "engine", "reader", "field", "duplicate"] {
        let (mut source, records) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
        let mut rows = rows(records);
        match label {
            "source" => source.source = "resources".into(),
            "engine" => {
                let CollectionTarget::Shared(owner) = &mut source.target else { panic!() };
                owner.engine = culprit_core::Engine::Postgres;
            }
            "reader" => {
                let SourceObservation::MssqlManagedInstanceResources(history) = &mut source.observation else {
                    panic!()
                };
                history.current_reader = None;
            }
            "field" => rows[0].metrics[0].value = Some(0.0),
            "duplicate" => rows.push(rows[0].clone()),
            _ => unreachable!(),
        };
        assert!(
            explain(ManagedInstanceResourceEvidence { source: &source, observations: &rows, read_complete: true })
                .is_err(),
            "{label}"
        );
    }
}
