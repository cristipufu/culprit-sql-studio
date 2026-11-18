use super::*;
use crate::{Engine, ErrorCode, Snapshot, SourceState};
fn read() -> MssqlAgentJobsSnapshot {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("PRIVATE-agent-machine"),
        native: NativeIdentity::new("PRIVATE-agent-instance"),
    };
    let facts = [
        OperationalFact::MssqlAgentJob(Box::new(MssqlAgentJob {
            observed_at: FROM.into(),
            job_id: JOB.into(),
            name: "PRIVATE-current-job-name".into(),
            enabled: true,
            start_step_id: "1".into(),
            version_number: "77".into(),
            created_without_offset: Some("2020-01-01T12:00:00.123".into()),
            modified_without_offset: None,
            activity: None,
        })),
        OperationalFact::MssqlAgentHistory(Box::new(MssqlAgentHistory {
            observed_at: UNTIL.into(),
            job_id: JOB.into(),
            instance_id: "2147483647".into(),
            step_id: "1".into(),
            stored_step_name: Some("PRIVATE-original-stored-label".into()),
            run_status: 0,
            run_date: "20200101".into(),
            run_time: "0".into(),
            run_duration: "250101".into(),
            sql_message_id: "0".into(),
            sql_severity: "0".into(),
            retries_attempted: "0".into(),
            reported_server: Some("PRIVATE-historical-server".into()),
        })),
    ];
    MssqlAgentJobsSnapshot {
        context: NativeReadContext {
            target: CollectionTarget::Shared(member.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(member),
                epoch: Some("native-process-generation".into()),
            },
            observed_from: FROM.into(),
            observed_until: UNTIL.into(),
            database_writable: None,
        },
        metadata: MssqlAgentJobsMetadata {
            engine_edition: 3,
            major_version: 16,
            availability: MssqlAgentAvailability::Supported,
            visibility: MssqlAgentVisibility::Sysadmin,
            capabilities: [
                MssqlAgentObject::Jobs,
                MssqlAgentObject::Steps,
                MssqlAgentObject::History,
                MssqlAgentObject::Activity,
                MssqlAgentObject::Sessions,
                MssqlAgentObject::Services,
            ]
            .into_iter()
            .enumerate()
            .map(|(index, object)| MssqlAgentCapability {
                object,
                access: MssqlAgentAccess::Available,
                columns_sha256: Some(digest(object.native_name().as_bytes())),
                object_id: Some((100 + index).to_string()),
            })
            .collect(),
            history_limit: 256,
            recheck: MssqlAgentMetadataRecheck::Unchanged,
        },
        sources: vec![
            StateSourceCoverage::collected(MSSQL_AGENT_JOBS_SOURCE.into(), 1),
            StateSourceCoverage::collected(MSSQL_AGENT_STEPS_SOURCE.into(), 0),
            StateSourceCoverage::observed(MSSQL_AGENT_HISTORY_SOURCE.into(), 1, false),
            StateSourceCoverage::unavailable(MSSQL_AGENT_CONTEXT_SOURCE.into(), ErrorCode::PermissionDenied, None),
        ],
        observations: facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| {
                let key = match &fact {
                    OperationalFact::MssqlAgentJob(point) => point.key().unwrap(),
                    OperationalFact::MssqlAgentHistory(point) => point.key().unwrap(),
                    _ => panic!("the fixture contains only Agent jobs and retained history"),
                };
                OperationalObservation {
                    source: if index == 0 { MSSQL_AGENT_JOBS_SOURCE } else { MSSQL_AGENT_HISTORY_SOURCE }.into(),
                    key,
                    fact,
                }
            })
            .collect(),
    }
}
const FROM: &str = "2020-01-01T12:00:00.000000001Z";
const UNTIL: &str = "2020-01-01T12:00:01.000000009Z";
const JOB: &str = "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb";
fn description(read: &MssqlAgentJobsSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: read.context.target.clone(),
        partition: read.context.partition.clone(),
        source: MSSQL_AGENT_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlAgentJobs {
            metadata: Box::new(read.metadata.clone()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: read.sources.iter().map(|source| source.records).sum(),
                sources: read.sources.clone(),
            },
        },
    }
}
#[test]
fn canonical_native_groups_keep_local_tokens_nullable_values_and_original_counts_without_current_definition_binding() {
    let read = read();
    assert!(read.is_valid_read(true));
    let source = description(&read);
    let capture = CaptureIdentity { observation_id: "agent-core-capture".into(), revision: None };
    for row in &read.observations {
        let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row.clone())).unwrap();
        assert_eq!(exported.record, TelemetryRecord::Operational(row.clone()));
    }
    let restored = JobsSnapshot::from_source(&source, read.observations.clone(), true).unwrap();
    assert!(restored.is_valid());
    assert!(restored.read_complete());
    assert_eq!(restored.context(), &read.context);
    assert_eq!(restored.sources(), read.sources.as_slice());
    assert_eq!(serde_json::from_slice::<JobsSnapshot>(&serde_json::to_vec(&restored).unwrap()).unwrap(), restored);
    let OperationalFact::MssqlAgentHistory(point) = &restored.observations()[1].fact else { panic!() };
    assert_eq!(point.run_time, "0");
    assert_eq!(point.run_duration, "250101");
    assert_eq!(point.stored_step_name.as_deref(), Some("PRIVATE-original-stored-label"));
}
#[test]
fn native_scope_profile_membership_duplicates_and_every_body_row_are_checked_before_presentation() {
    for change in [
        "member",
        "namespace",
        "engine",
        "epoch",
        "writable",
        "source",
        "duplicate",
        "guid",
        "clock",
        "overflow",
        "undeclared",
        "history-complete",
    ] {
        let mut changed = read();
        match change {
            "member" => {
                if let SourcePartition::InstanceLocal { member, .. } = &mut changed.context.partition {
                    *member = None;
                }
            }
            "namespace" => {
                changed.context.target =
                    crate::DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None).into()
            }
            "engine" => {
                if let CollectionTarget::Shared(target) = &mut changed.context.target {
                    target.engine = Engine::Postgres;
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.context.partition {
                    *epoch = None;
                }
            }
            "writable" => changed.context.database_writable = Some(true),
            "source" => changed.observations[0].source = MSSQL_AGENT_HISTORY_SOURCE.into(),
            "duplicate" => {
                changed.observations.push(changed.observations[1].clone());
                changed.sources[2].records += 1;
            }
            "history-complete" => {
                changed.sources[2] = StateSourceCoverage::collected(MSSQL_AGENT_HISTORY_SOURCE.into(), 1)
            }
            "undeclared" => changed.sources[0].source = "unreported-native-source".into(),
            _ => {
                let OperationalFact::MssqlAgentHistory(point) = &mut changed.observations[1].fact else { panic!() };
                match change {
                    "guid" => point.job_id = JOB.to_uppercase(),
                    "clock" => point.observed_at = "2020-01-01T12:00:02Z".into(),
                    _ => point.instance_id = "2147483648".into(),
                };
            }
        }
        assert!(!changed.is_valid_read(false), "{change}");
    }
}
#[test]
fn incomplete_archive_bodies_preserve_original_source_population_and_never_become_complete_or_healthy_empty() {
    let original = read();
    let source = description(&original);
    assert!(JobsSnapshot::from_source(&source, vec![], true).is_err());
    let read = JobsSnapshot::from_source(&source, vec![], false).unwrap();
    assert!(!read.read_complete());
    assert_eq!(read.sources(), original.sources);
    assert!(read.observations().is_empty());
    let mut changed = original.clone();
    changed.metadata.visibility = MssqlAgentVisibility::ExplicitSelect;
    assert!(!changed.is_valid_read(true), "owner-limited data cannot claim whole-instance completeness");
    for coverage in &mut changed.sources {
        if coverage.state == SourceState::Collected || coverage.state == SourceState::Empty {
            *coverage = StateSourceCoverage::observed(coverage.source.clone(), coverage.records, false);
        }
    }
    assert!(changed.is_valid_read(true));
}
#[test]
fn sharing_withholds_immutable_agent_identities_and_retains_one_repeatable_explicit_gap() {
    let read = read();
    let before = read.clone();
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.operations.jobs =
        Some(Box::new(JobsSnapshot::MssqlAgent { snapshot: Box::new(read), read_complete: true }));
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(shared.operations.jobs.is_none());
    let marker = shared.sources.iter().find(|source| source.source == MSSQL_AGENT_JOBS_SOURCE).unwrap();
    assert_eq!(marker.state, SourceState::Unavailable);
    assert_eq!(marker.reason_message, Some(crate::message!("view.mssql_agent_jobs.sharing_withheld")));
    assert!(!serde_json::to_string(&shared).unwrap().contains("PRIVATE-"));
    let repeated = crate::redact::redact_snapshot(&shared);
    assert_eq!(repeated.sources.iter().find(|source| source.source == MSSQL_AGENT_JOBS_SOURCE), Some(marker));
    let JobsSnapshot::MssqlAgent { snapshot: original, .. } = snapshot.operations.jobs.as_ref().unwrap().as_ref()
    else {
        panic!()
    };
    assert_eq!(original.as_ref(), &before);
}
