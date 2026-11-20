use super::*;

/// The native rows and logical database are identical. Only independently observed member/epoch/role facts vary.
pub fn replica_activity_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (baseline, rows) = activity_evidence_cases().remove(0);
    [
        (Some("private-compute-a"), Some("start-a"), Some(true)),
        (Some("private-compute-b"), Some("start-a"), Some(false)),
        (Some("private-compute-a"), Some("new-start-a"), Some(true)),
        (Some("private-compute-a"), Some("start-a"), Some(false)),
        (None, None, Some(false)),
        (Some("private-compute-a"), None, None),
        (None, Some("start-a"), Some(false)),
    ]
    .into_iter()
    .map(|(member, epoch, writable)| {
        let mut source = baseline.clone();
        source.partition = SourcePartition::InstanceLocal {
            member: member.map(|id| SharedResourceIdentity {
                engine: source.target.engine(),
                kind: SharedResourceKind::Node,
                namespace: NativeIdentity::new("private-compute-namespace"),
                native: NativeIdentity::new(id),
            }),
            epoch: epoch.map(str::to_owned),
        };
        let qualified = source.partition.is_qualified();
        let SourceObservation::Activity(frame) = &mut source.observation else { unreachable!() };
        frame.epoch = epoch.map(str::to_owned);
        frame.database_writable = writable;
        frame.coverage = StateSourceCoverage::observed(source.source.clone(), rows.len() as u32, qualified);
        assert!(frame.is_valid());
        assert!(source.partition_is_consistent());
        (source, rows.clone())
    })
    .collect()
}

/// Replicated history belongs to the declared database, not the route that observed the retained interval.
pub fn replicated_history_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (source, rows) = query_evidence();
    ["private-compute-a", "private-compute-b"]
        .into_iter()
        .map(|member| {
            let mut source = source.clone();
            source.partition = SourcePartition::Target;
            let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
            history.native_properties.insert("reader_member".into(), member.into());
            (source, rows.clone())
        })
        .collect()
}

pub fn assert_replica_activity_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_activity_capture(capture, source, records);
    assert_eq!(capture.source().partition, source.partition);
    if !source.is_partition_qualified() {
        let frame = capture.source().observation.activity().unwrap();
        assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
        assert!(!frame.coverage.complete_scope);
    }
}

/// One common statement is observed on both native instances; only the second read-serving member also observed
/// the other statement. All four families retain the same original native scope, without a Query Store join.
pub fn replica_workload_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let mut cases = Vec::new();
    for (member, writable, count) in [("native-primary", true, 1), ("native-secondary", false, 2)] {
        let partition = SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new(format!("{member}-machine"))
                    .qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new(format!("{member}-instance")).qualified("kind", "sql-server-instance"),
            }),
            epoch: Some(format!("{member}-database-process-epoch")),
        };
        let (mut activity, _) = replica_activity_evidence_cases().remove(0);
        activity.partition = partition.clone();
        activity.target = culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "ag-listener.invalid",
            1433,
            None,
        )
        .into();
        let SourceObservation::Activity(frame) = &mut activity.observation else { unreachable!() };
        frame.epoch = partition.native_epoch().map(str::to_owned);
        frame.database_writable = Some(writable);
        frame.native_rows = count;
        frame.observations = count;
        frame.coverage = StateSourceCoverage::collected(activity.source.clone(), count);
        let rows = (0..count)
            .map(|index| {
                let mut row =
                    activity_observation(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
                row.key.session.id = (61 + index).to_string();
                row.idle = true;
                row.key.request = None;
                row.key.transaction = Some(format!("native-transaction-{index}"));
                row.scope = ActivityScopeBasis::TransactionDatabase;
                row.open_transactions = Some(1);
                row.command = None;
                row.wait = None;
                row.waiting_on_lock = None;
                row.request_counters = None;
                row.statement_identity = None;
                row.elapsed_ms = None;
                row.transaction_age_ms = Some("1".into());
                TelemetryRecord::from(row)
            })
            .collect();
        cases.push((activity, rows));
        for dataset in
            [SnapshotDataset::CachedRuntime, SnapshotDataset::CachedDefinitions, SnapshotDataset::RetainedPlans]
        {
            let (mut source, rows) = match dataset {
                SnapshotDataset::CachedRuntime => cached_runtime_evidence_cases().remove(1),
                SnapshotDataset::CachedDefinitions => object_definition_evidence_cases().remove(0),
                SnapshotDataset::RetainedPlans => object_definition_evidence_cases().remove(1),
                _ => unreachable!(),
            };
            source.partition = partition.clone();
            // A caller's stable endpoint alias is deliberately identical: only native member/epoch facts isolate it.
            source.target = culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "db",
                "ag-listener.invalid",
                1433,
                None,
            )
            .into();
            let source_name = match dataset {
                SnapshotDataset::CachedRuntime => "statement-runtime-cache",
                SnapshotDataset::CachedDefinitions => "cached-definitions-state",
                SnapshotDataset::RetainedPlans => "retained-plans-state",
                _ => unreachable!(),
            };
            source.source = source_name.into();
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            snapshot.records = count;
            snapshot.sources = vec![StateSourceCoverage::collected("plan-cache".into(), count)];
            let records = (0..count).map(|index| workload_record(&rows[0], index)).collect();
            assert!(source.partition_is_consistent());
            cases.push((source, records));
        }
    }
    cases
}

fn workload_record(original: &TelemetryRecord, index: u32) -> TelemetryRecord {
    let sql = if index == 0 {
        "SELECT COUNT_BIG(*) AS common_work FROM dbo.marker"
    } else {
        "SELECT MAX(DATALENGTH(owner)) AS secondary_work FROM dbo.marker"
    };
    let mut record = original.clone();
    let context = match &mut record {
        TelemetryRecord::Operational(OperationalObservation {
            source,
            fact: OperationalFact::MssqlCachedRuntime(runtime),
            ..
        }) => {
            *source = "plan-cache".into();
            runtime.executions = Some("1".into());
            runtime.context.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
            runtime.context.source_text_complete = true;
            for counter in runtime.counters.values_mut() {
                *counter = CachedCounterDistribution {
                    total: "1".into(),
                    last: "1".into(),
                    minimum: "1".into(),
                    maximum: "1".into(),
                };
            }
            &mut runtime.context
        }
        TelemetryRecord::ObjectDefinition(definition) => match &mut definition.fact {
            ObjectDefinitionFact::MssqlCachedDefinition(native) => {
                native.module_parameters = None;
                native.relational_input_proof = None;
                native.statement = workload_payload(sql, DefinitionPayload::sql);
                native.estimated_plan = workload_payload(&workload_plan(sql, false, index), DefinitionPayload::plan);
                &mut native.context
            }
            ObjectDefinitionFact::MssqlRetainedPlan(native) => {
                native.module_parameters = None;
                native.relational_input_proof = None;
                native.parameter_context_proof = None;
                native.statement = workload_payload(sql, DefinitionPayload::sql);
                native.batch_plan = workload_payload(&workload_plan(sql, true, index), DefinitionPayload::plan);
                native.statement_executions = Some("1".into());
                &mut native.context
            }
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    context.identity.plan_handle = format!("0x0102030{index}");
    context.identity.sql_handle = format!("0x0506070{index}");
    context.identity.plan_generation = "1".into();
    context.query_hash = Some(format!("0x0123456789ABCDE{index}"));
    context.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
    context.source_text_complete = true;
    match record {
        TelemetryRecord::Operational(mut row) => {
            let OperationalFact::MssqlCachedRuntime(runtime) = &row.fact else { unreachable!() };
            row.key = runtime.context.identity.key().unwrap();
            TelemetryRecord::Operational(row)
        }
        TelemetryRecord::ObjectDefinition(row) => {
            TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new("plan-cache".into(), row.fact).unwrap())
        }
        _ => unreachable!(),
    }
}

fn workload_payload(raw: &str, redact: fn(&str, culprit_core::Engine) -> DefinitionPayload) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
        native_sha256: Some(digest(raw.as_bytes())),
        payload: redact(raw, culprit_core::Engine::Mssql),
    }
}

fn workload_plan(sql: &str, actual: bool, index: u32) -> String {
    let runtime = if actual {
        r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation>"#
    } else {
        ""
    };
    format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDE{index}" StatementType="SELECT"><QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1" Parallel="false">{runtime}<TableScan><Object Database="[db]" Schema="[dbo]" Table="[marker]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    )
}
