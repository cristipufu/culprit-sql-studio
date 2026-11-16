use super::*;
use crate::{DatabaseIdentity, PlanVariant, ResetMarker, source_id, telemetry::*};

fn private_context(snapshot: &Snapshot) -> NativeReadContext {
    let mut database = DatabaseIdentity::server(
        Engine::Mssql,
        &snapshot.server.database,
        "NEW-PRIVATE-host.example",
        15433,
        Some("NEW-PRIVATE-instance".into()),
    );
    database.incarnation = Some("NEW-PRIVATE-database-incarnation".into());
    NativeReadContext {
        target: CollectionTarget::Database(database),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("NEW-PRIVATE-namespace")
                    .qualified("source", "/NEW-PRIVATE/namespace/path"),
                native: NativeIdentity::new("NEW-PRIVATE-member").qualified("endpoint", "NEW-PRIVATE-compute.example"),
            }),
            epoch: Some("/NEW-PRIVATE/process/epoch".into()),
        },
        observed_from: "2026-11-01T06:00:00Z".into(),
        observed_until: "2026-11-01T06:00:01Z".into(),
        database_writable: Some(false),
    }
}

fn original_grant() -> StatementGrantStatistics {
    let text = "SELECT value FROM dbo.input WHERE token=N'NEW-PRIVATE-statement-value'".to_string();
    StatementGrantStatistics {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2026-11-01T04:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: Some((text.encode_utf16().count() * 2).to_string()),
            source_text_complete: true,
        },
        statement: DefinitionPayload::Available {
            sha256: digest(text.as_bytes()),
            text,
            redaction_version: REDACTION_VERSION,
        },
        available_columns: vec!["execution_count".into(), "total_grant_kb".into(), "total_used_grant_kb".into()],
        executions: Some("40".into()),
        last_execution_without_offset: Some("2026-11-01T05:59:59.997".into()),
        total_grant_kibibytes: Some("9007199254740993".into()),
        total_used_kibibytes: Some("1638400".into()),
        last_grant_kibibytes: None,
        last_used_kibibytes: None,
        min_grant_kibibytes: None,
        min_used_kibibytes: None,
        max_grant_kibibytes: None,
        max_used_kibibytes: None,
        reset: ResetMarker::Unknown,
    }
}

fn grant_read(context: &NativeReadContext) -> StatementGrantSnapshot {
    let first = original_grant();
    let mut unavailable = first.clone();
    unavailable.context.identity.plan_generation = "2".into();
    unavailable.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    StatementGrantSnapshot {
        capture: CaptureIdentity {
            observation_id: "NEW-PRIVATE-capture".into(),
            revision: Some(CaptureRevision {
                outbox_id: "/NEW-PRIVATE/outbox".into(),
                owner_epoch: "NEW-PRIVATE-owner".into(),
                sequence: "2".into(),
            }),
        },
        description: CollectionDescription {
            target: context.target.clone(),
            partition: context.partition.clone(),
            source: source_id::GRANTS.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementGrants,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                records: 2,
                sources: vec![StateSourceCoverage::observed(source_id::GRANTS.into(), 2, false)],
            }),
        },
        observations: vec![first, unavailable],
    }
}

fn current_grant(context: &NativeReadContext) -> RequestMemorySnapshot {
    let source = source_id::CURRENT_REQUEST_MEMORY;
    RequestMemorySnapshot {
        context: context.clone(),
        source: source.into(),
        observed_at: "2026-11-01T06:00:00.500Z".into(),
        observations: vec![ExecutionGrantObservation {
            source: source.into(),
            identity: NativeIdentity::new("reservation-1"),
            observed_at: "2026-11-01T06:00:00.500Z".into(),
            state: ExecutionGrantState::Granted,
            request_association: RequestGrantAssociation::InsufficientEvidence,
            statement_relationship: GrantStatementRelationship::Unresolved,
            request: None,
            scope: None,
            requested_without_offset: "2026-11-01T04:00:00.001".into(),
            granted_without_offset: Some("2026-11-01T04:00:00.002".into()),
            requested_kibibytes: Some("4096".into()),
            required_kibibytes: Some("1024".into()),
            granted_kibibytes: Some("4096".into()),
            used_kibibytes: Some("1024".into()),
            maximum_used_kibibytes: Some("2048".into()),
            ideal_kibibytes: None,
            wait_ms: Some("0".into()),
            timeout_seconds: None,
            degree_of_parallelism: None,
            reserved_workers: None,
            used_workers: None,
            maximum_used_workers: None,
            resource: None,
            workload_group: None,
            queue: ExecutionGrantQueue::default(),
            native_properties: Default::default(),
            available_columns: vec!["granted_memory_kb".into(), "used_memory_kb".into()],
            native_null_columns: vec![],
        }],
    }
}

#[test]
fn new_native_lookup_references_and_private_read_contexts_cannot_escape_in_shared_snapshots() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let context = private_context(&snapshot);
    assert!(context.is_valid());
    let read = grant_read(&context);
    let requests = current_grant(&context);
    assert!(read.is_valid() && requests.is_valid());
    let native_query = NativeQueryIdentity {
        query_id: "NEW-PRIVATE-query".into(),
        qualifiers: [("context".into(), "/NEW-PRIVATE/query/path".into())].into(),
    };
    let native_plan = NativePlanIdentity {
        plan_id: "NEW-PRIVATE-plan".into(),
        query: native_query.clone(),
        qualifiers: [("owner".into(), "NEW-PRIVATE-plan-host".into())].into(),
    };
    let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| query.plan.is_some()).unwrap();
    let query_id = query.id.clone();
    query.native_identity = Some(native_query.clone());
    query.native_plan_identity = Some(native_plan.clone());
    query.plans.push(PlanVariant {
        native_identity: Some(native_plan.clone()),
        id: "variant".into(),
        executions: 40.0,
        total_time_ms: 4000.0,
        mean_time_ms: 100.0,
        first_seen: None,
        last_seen: None,
        is_forced: None,
        min_time_ms: None,
        max_time_ms: None,
        stddev_time_ms: None,
        compiled: None,
        cpu_time_ms: Some(2000.0),
        logical_reads: Some(400.0),
    });
    let plan = query.plan.as_mut().unwrap();
    plan.facts.elapsed_ms = Some(123.0);
    plan.facts.memory_granted_kb = Some(4096.0);
    plan.facts.memory_max_used_kb = Some(1024.0);
    let retained = RetainedPlanProvenance {
        read: context.clone(),
        statement: read.observations[0].context.clone(),
        query: Some(native_query),
        native_plan: Some(native_plan),
    };
    assert!(retained.is_valid());
    plan.facts.retained = Some(Box::new(retained));
    query.plan_history = Some(crate::PlanHistory {
        source: "native-cache".into(),
        kind: crate::PlanHistoryKind::Cache,
        note: "native cache counters".into(),
        captured_plan_id: Some("variant".into()),
        captured_plan: Some(plan.clone()),
        source_message: None,
        note_message: None,
    });
    let pressure = snapshot.pressure.get_or_insert_default();
    pressure.grants = vec![read.clone()];
    pressure.request_memory = Some(requests.clone());
    assert!(
        serde_json::to_string(&snapshot).unwrap().contains("NEW-PRIVATE"),
        "the fixture must exercise the new leak surfaces"
    );

    let shared = redact_snapshot(&snapshot);
    let serialized = serde_json::to_value(&shared).unwrap();
    assert!(!serialized.to_string().to_ascii_lowercase().contains("new-private"));
    let query = shared.queries().iter().find(|query| query.id == query_id).unwrap();
    assert!(query.native_identity.is_none() && query.native_plan_identity.is_none());
    assert!(query.plans.iter().all(|plan| plan.native_identity.is_none()));
    for plan in [query.plan.as_ref().unwrap(), query.plan_history.as_ref().unwrap().captured_plan.as_ref().unwrap()] {
        assert!(plan.facts.retained.is_none(), "shared measured plans are not live cache lookup proof");
        assert_eq!(plan.facts.elapsed_ms, Some(123.0));
        assert_eq!(plan.facts.memory_granted_kb, Some(4096.0));
        assert_eq!(plan.facts.memory_max_used_kb, Some(1024.0));
    }
    let pressure = shared.pressure.as_ref().unwrap();
    let shared_read = &pressure.grants[0];
    assert!(shared_read.is_valid());
    assert!(shared_read.capture.revision.is_none());
    assert!(shared_read.capture.observation_id.starts_with("redacted-"));
    assert_eq!(shared_read.observations[0].context, read.observations[0].context);
    assert_eq!(shared_read.observations[0].total_grant_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(shared_read.observations[1].statement, read.observations[1].statement);
    assert!(shared_read.observations[0].statement.is_valid());
    let shared_requests = pressure.request_memory.as_ref().unwrap();
    assert!(shared_requests.is_valid());
    assert_eq!(shared_requests.observations, requests.observations);
    assert_eq!(shared_requests.context.target, shared_read.description.target);
    assert_eq!(shared_requests.context.partition, shared_read.description.partition);
    assert_eq!(shared_requests.context.database_writable, Some(false));
    assert_eq!(serialized, serde_json::to_value(redact_snapshot(&shared)).unwrap());
}

#[test]
fn sharing_keeps_unknown_source_owners_unknown_and_retains_their_independent_positive_measurements() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut context = private_context(&snapshot);
    context.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let read = grant_read(&context);
    let requests = current_grant(&context);
    assert!(read.is_valid() && requests.is_valid());
    let pressure = snapshot.pressure.get_or_insert_default();
    pressure.grants = vec![read.clone()];
    pressure.request_memory = Some(requests);
    let shared = redact_snapshot(&snapshot);
    let pressure = shared.pressure.as_ref().unwrap();
    assert!(matches!(
        pressure.grants[0].description.partition,
        SourcePartition::InstanceLocal { member: None, epoch: None }
    ));
    assert_eq!(pressure.grants[0].observations[0].total_used_kibibytes, read.observations[0].total_used_kibibytes);
    let current = pressure.request_memory.as_ref().unwrap();
    assert!(!current.context.has_qualified_local_owner());
    assert_eq!(current.observations[0].granted_kibibytes.as_deref(), Some("4096"));
    assert_eq!(current.observations[0].scope, None);
    assert!(pressure.grants[0].is_valid() && current.is_valid());
}

#[test]
fn standalone_finding_exports_do_not_retain_private_cached_grant_read_receipts() {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let read = grant_read(&private_context(&snapshot));
    let report: serde_json::Value =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.report.json")).unwrap();
    let mut finding: Finding = serde_json::from_value(report["findings"][0].clone()).unwrap();
    finding.facts.statement_grant = Some(crate::StatementGrantReference {
        snapshot_key: read.key().unwrap(),
        observation_key: read.observations[0].context.identity.key().unwrap(),
        capture: read.capture.clone(),
        identity: read.observations[0].context.identity.clone(),
    });
    assert!(serde_json::to_string(&finding).unwrap().contains("NEW-PRIVATE"));
    let shared = redact_finding(&finding, Engine::Mssql);
    assert!(shared.facts.statement_grant.is_none());
    assert!(!serde_json::to_string(&shared).unwrap().to_ascii_lowercase().contains("new-private"));
    assert_eq!(shared.metrics.len(), finding.metrics.len());
}
