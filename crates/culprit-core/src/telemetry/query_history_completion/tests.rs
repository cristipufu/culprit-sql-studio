use super::*;
use crate::{DatabaseIdentity, Engine, RuntimePopulationMembership};

fn source() -> CollectionDescription {
    let target: CollectionTarget =
        DatabaseIdentity::server(Engine::Mssql, "history", "owned.invalid", 1433, None).into();
    let owner = NativeReadContext {
        target: target.clone(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-machine"),
                native: NativeIdentity::new("native-instance"),
            }),
            epoch: Some("native-recovery-process-epoch".into()),
        },
        observed_from: "2026-11-19T12:00:00.000000000Z".into(),
        observed_until: "2026-11-19T12:00:00.000000000Z".into(),
        database_writable: Some(false),
    };
    CollectionDescription {
        target,
        partition: SourcePartition::Target,
        source: "native-query-history".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval: TelemetryInterval {
                native_id: "17".into(),
                from: "2026-11-19T13:00:00.000000000Z".into(),
                until: "2026-11-19T14:00:00.000000000Z".into(),
            },
            acquisition_owner: Some(Box::new(owner)),
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: [("source-capability".into(), "native-known".into())].into(),
        }),
    }
}
fn history(source: &CollectionDescription) -> &QueryHistoryDescription {
    source.query_history().unwrap()
}
fn completed(before: &CollectionDescription) -> QueryHistoryDescription {
    let mut after = history(before).clone();
    after.acquisition_owner.as_mut().unwrap().observed_until = "2026-11-19T12:00:03.123456789Z".into();
    after
}
fn observed(before: &CollectionDescription) -> QueryHistoryDescription {
    let mut after = completed(before);
    let population = crate::NativeRuntimePopulation {
        identity: NativeIdentity::new("7"),
        native_properties: [("replica_group_id".into(), "7".into())].into(),
    };
    after.population_catalog = Some(crate::NativeRuntimePopulationCatalog {
        source: crate::source_id::QUERY_STORE.into(),
        membership: RuntimePopulationMembership::ObservedWindow {
            from: after.interval.from.clone(),
            until: after.interval.until.clone(),
        },
        partitioned: true,
        requires_selection: false,
        populations: vec![population.clone()],
    });
    after.population_binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: vec![QueryHistoryPopulationBindingEntry {
            population: population.identity,
            qualifiers: [("replica_group_id".into(), "7".into())].into(),
        }],
    };
    after.dimensions.insert("replica_group_id".into(), vec!["7".into()]);
    after
}
fn finish(before: &CollectionDescription, after: QueryHistoryDescription) -> Result<Option<CollectionDescription>> {
    before.completed(SourceCompletion::QueryHistory(Box::new(after)))
}
#[test]
fn history_owner_completion_keeps_native_data_namespace_and_independent_clock_domains() {
    let before = source();
    let result = finish(&before, observed(&before)).unwrap().unwrap();
    assert_eq!(result.target, before.target);
    assert_eq!(result.partition, SourcePartition::Target);
    assert_eq!(result.continuity, SourceContinuity::Unverified);
    assert_eq!(
        result.target.database().unwrap().incarnation,
        None,
        "local recovery facts cannot become global identity"
    );
    let read = history(&result).acquisition_owner.as_ref().unwrap();
    assert_eq!(read.database_writable, Some(false), "updateability is not a primary/secondary label");
    assert!(read.completes_read(history(&before).acquisition_owner.as_ref().unwrap()));
    assert!(
        NativeSourceReadScope::from_context(read).is_err(),
        "history acquisition grants no global-incarnation cost authority"
    );
    assert_eq!(
        history(&result).interval,
        history(&before).interval,
        "native history clocks are not rewritten to acquisition time"
    );
    assert!(!history(&result).population_catalog.as_ref().unwrap().has_complete_membership());
}
#[test]
fn no_unknown_or_changed_original_owner_can_become_completion_authority() {
    let before = source();
    for change in [
        "missing-before",
        "missing-after",
        "member",
        "epoch",
        "target",
        "updateability",
        "unknown-updateability",
        "start",
        "reversed",
        "shrunk",
    ] {
        let mut initial = before.clone();
        let mut after = completed(&initial);
        match change {
            "missing-before" => {
                let SourceObservation::QueryHistory(history) = &mut initial.observation else { panic!() };
                history.acquisition_owner = None;
            }
            "missing-after" => after.acquisition_owner = None,
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } =
                    &mut after.acquisition_owner.as_mut().unwrap().partition
                else {
                    panic!()
                };
                member.as_mut().unwrap().native.id = "other-native-instance".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } =
                    &mut after.acquisition_owner.as_mut().unwrap().partition
                else {
                    panic!()
                };
                *epoch = Some("other-process-or-recovery".into());
            }
            "target" => {
                after.acquisition_owner.as_mut().unwrap().target =
                    DatabaseIdentity::server(Engine::Mssql, "other", "owned.invalid", 1433, None).into()
            }
            "updateability" => after.acquisition_owner.as_mut().unwrap().database_writable = Some(true),
            "unknown-updateability" => {
                let SourceObservation::QueryHistory(history) = &mut initial.observation else { panic!() };
                history.acquisition_owner.as_mut().unwrap().database_writable = None;
            }
            "start" => after.acquisition_owner.as_mut().unwrap().observed_from = "2026-11-19T12:00:01Z".into(),
            "reversed" => after.acquisition_owner.as_mut().unwrap().observed_until = "2026-11-19T11:59:59Z".into(),
            "shrunk" => {
                let SourceObservation::QueryHistory(history) = &mut initial.observation else { panic!() };
                history.acquisition_owner.as_mut().unwrap().observed_until = "2026-11-19T12:00:04Z".into();
            }
            _ => unreachable!(),
        }
        assert!(finish(&initial, after).is_err(), "{change}");
    }
    let mut representation = completed(&before);
    representation.acquisition_owner.as_mut().unwrap().observed_from = "2026-11-19T14:00:00+02:00".into();
    representation.acquisition_owner.as_mut().unwrap().observed_until = "2026-11-19T14:00:03.123456789+02:00".into();
    assert!(finish(&before, representation).is_ok(), "compare validated UTC instants, not lexical clock strings");
}
#[test]
fn observed_membership_cannot_change_original_interval_waits_properties_or_native_catalog() {
    let before = source();
    for change in [
        "interval-id",
        "interval-clock",
        "properties",
        "waits",
        "unpartitioned",
        "native-catalog",
        "extra-dimension",
        "membership-window",
    ] {
        let mut after = observed(&before);
        match change {
            "interval-id" => after.interval.native_id = "18".into(),
            "interval-clock" => after.interval.until = "2026-11-19T14:00:01Z".into(),
            "properties" => {
                after.native_properties.insert("source-capability".into(), "different".into());
            }
            "waits" => {
                after.waits = Some(QueryWaitSemantics {
                    capture: WaitCaptureState::Enabled,
                    coverage: WaitCaptureCoverage::Unverified,
                    categories: [(
                        "3".into(),
                        NativeWaitMeaning {
                            native_name: "native-lock-category".into(),
                            disposition: WaitDisposition::Unknown,
                        },
                    )]
                    .into(),
                })
            }
            "unpartitioned" => {
                let catalog = after.population_catalog.as_mut().unwrap();
                catalog.partitioned = false;
                catalog.populations.clear();
                catalog.membership = RuntimePopulationMembership::NativeCatalog;
                after.population_binding = QueryHistoryPopulationBinding::Unpartitioned;
                after.dimensions.clear();
            }
            "native-catalog" => {
                after.population_catalog.as_mut().unwrap().membership = RuntimePopulationMembership::NativeCatalog
            }
            "extra-dimension" => {
                after.dimensions.insert("not-observed".into(), vec!["invented".into()]);
            }
            "membership-window" => {
                after.population_catalog.as_mut().unwrap().membership = RuntimePopulationMembership::ObservedWindow {
                    from: "2026-11-19T13:00:01Z".into(),
                    until: after.interval.until.clone(),
                }
            }
            _ => unreachable!(),
        }
        assert!(finish(&before, after).is_err(), "{change}");
    }
    let mut known = source();
    let mut native = observed(&known);
    native.population_catalog.as_mut().unwrap().membership = RuntimePopulationMembership::NativeCatalog;
    known.observation = SourceObservation::QueryHistory(native.clone());
    let mut changed = native;
    changed.population_catalog.as_mut().unwrap().populations[0]
        .native_properties
        .insert("role-history".into(), "new-role-fact".into());
    assert!(finish(&known, changed).is_err(), "known native catalog facts are immutable");
}
#[test]
fn native_record_ids_payload_hashes_and_original_canonical_rows_survive_only_header_completion() {
    let before = source();
    let after = finish(&before, observed(&before)).unwrap().unwrap();
    let query = NativeQueryIdentity {
        query_id: "41".into(),
        qualifiers: [("native-incarnation".into(), "original-query".into())].into(),
    };
    let definition_id = query.definition_id(&before).unwrap();
    let definition = TelemetryRecord::QueryDefinition(QueryDefinition {
        id: definition_id.clone(),
        identity: query.clone(),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        context: Default::default(),
        context_available: false,
        source_text_bytes: None,
        variant_family: None,
        sql: DefinitionPayload::sql("SELECT count_value FROM dbo.native_table", Engine::Mssql),
    });
    let runtime = TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("9007199254740993")
                .qualified("plan_id", "42")
                .qualified("replica_group_id", "7")
                .qualified("execution_type", "0"),
            cohort: None,
            references: StatementReferences {
                query: Some(NativeDefinitionReference {
                    native: query.native(),
                    definition_id: Some(definition_id.clone()),
                }),
                plan: None,
            },
        },
        executions: "9007199254740993".into(),
        metrics: vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 1.25)],
        page_server_reads: None,
        first_execution_at: Some("2026-11-19T13:00:00.123456789Z".into()),
        last_execution_at: Some("2026-11-19T13:59:59.123456789Z".into()),
    });
    let capture = CaptureIdentity { observation_id: "controlled-header-completion".into(), revision: None };
    for record in [definition, runtime.clone()] {
        let original = ExportedRecord::new(&before, &capture, record).unwrap();
        let replay = ExportedRecord::new(&after, &capture, original.record.clone()).unwrap();
        assert_eq!(original, replay);
        assert_eq!(original.record_id, replay.record_id);
        assert_eq!(original.payload_sha256().unwrap(), replay.payload_sha256().unwrap());
    }
    assert_eq!(definition_id, query.definition_id(&after).unwrap());
    let mut invalid = runtime;
    let TelemetryRecord::QueryRuntime(row) = &mut invalid else { panic!() };
    row.first_execution_at = Some("invalid-original-native-clock".into());
    assert!(ExportedRecord::new(&before, &capture, invalid.clone()).is_err());
    assert!(
        ExportedRecord::new(&after, &capture, invalid).is_err(),
        "final metadata cannot repair an invalid original row"
    );
}
#[test]
fn verified_cannot_skip_an_acquired_owner_completion_and_failed_completion_keeps_original_unknown() {
    let before = source();
    assert!(before.completed(SourceCompletion::Verified).is_err());
    assert!(before.completed(SourceCompletion::Changed).unwrap().is_none());
    assert!(matches!(history(&before).population_binding, QueryHistoryPopulationBinding::Unknown));
    let unchanged = finish(&before, completed(&before)).unwrap().unwrap();
    assert!(matches!(history(&unchanged).population_binding, QueryHistoryPopulationBinding::Unknown));
    assert!(history(&unchanged).population_catalog.is_none(), "empty runtime cannot invent a native population");
    let mut other_provider = before;
    let SourceObservation::QueryHistory(history) = &mut other_provider.observation else { panic!() };
    history.acquisition_owner = None;
    assert_eq!(
        other_provider.completed(SourceCompletion::Verified).unwrap(),
        Some(other_provider.clone()),
        "no acquired owner remains an explicit capability gap"
    );
}
