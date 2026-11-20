use super::*;

/// Current reservations and waiters share a bounded read, not an inferred causal edge. Unknown request ownership
/// remains present in the partial case; zero-row cases retain native empty and unavailable qualification.
pub fn request_memory_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let sql =
        DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql);
    let (source, rows) = request_memory_evidence(sql, true);
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(holder), .. }) =
        &rows[0]
    else {
        panic!("shared request-memory fixture")
    };
    let mut waiter = holder.as_ref().clone();
    waiter.identity.id = "62:0".into();
    waiter.request.as_mut().unwrap().key.session.id = "62".into();
    waiter.state = ExecutionGrantState::Waiting;
    waiter.granted_without_offset = None;
    waiter.granted_kibibytes = Some("0".into());
    waiter.used_kibibytes = Some("0".into());
    waiter.wait_ms = Some("42000".into());
    let mut unowned = holder.as_ref().clone();
    unowned.identity.id = "63:0".into();
    unowned.request = None;
    unowned.scope = None;
    unowned.request_association = RequestGrantAssociation::ChangedRequest;
    unowned.statement_relationship = GrantStatementRelationship::Unresolved;
    let known = vec![record(holder.as_ref().clone()), record(waiter)];
    let mut partial = known.clone();
    partial.push(record(unowned));
    let mut cases: Vec<_> = [
        (known, culprit_core::SourceState::Collected),
        (partial, culprit_core::SourceState::Partial),
        (vec![], culprit_core::SourceState::Empty),
        (vec![], culprit_core::SourceState::Unavailable),
    ]
    .into_iter()
    .map(|(rows, state)| {
        let mut source = source.clone();
        let SourceObservation::Snapshot(native) = &mut source.observation else { panic!("snapshot fixture") };
        native.records = rows.len() as u32;
        let source_id = native.sources[0].source.clone();
        native.sources = vec![if state == culprit_core::SourceState::Unavailable {
            StateSourceCoverage::unavailable(source_id, culprit_core::ErrorCode::PermissionDenied, None)
        } else {
            StateSourceCoverage::observed(source_id, rows.len() as u32, state != culprit_core::SourceState::Partial)
        }];
        (source, rows)
    })
    .collect();
    let (mut source, mut rows) = cases[0].clone();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
        panic!("native fixture owner")
    };
    member.kind = SharedResourceKind::Instance;
    for record in &mut rows {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::ExecutionGrant(grant), ..
        }) = record
        else {
            panic!("shared request-memory fixture")
        };
        grant.request.as_mut().unwrap().statement_identity =
            Some(NativeIdentity::new(format!("native-statement-{}", grant.identity.id)));
        grant.resource = Some(NativeIdentity::new("2").qualified("resource_semaphore_id", "0"));
        grant.required_kibibytes = Some("64".into());
        grant.requested_kibibytes = Some("1024".into());
        grant.resource_observation = Some(ExecutionGrantResourceObservation {
            available_kibibytes: Some(if grant.state == ExecutionGrantState::Waiting { "-64" } else { "1024" }.into()),
            granted_kibibytes: Some("9007199254740993".into()),
            grantee_count: Some("0".into()),
            waiter_count: None,
        });
        if grant.state == ExecutionGrantState::Waiting {
            grant.queue.next_candidate = Some(true);
        }
        assert!(grant.is_valid());
    }
    cases.push((source.clone(), rows.clone()));
    for record in &mut rows {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::ExecutionGrant(grant), ..
        }) = record
        else {
            unreachable!()
        };
        grant.resource =
            grant.resource.take().map(|resource| resource.qualified("kind", "sql-server-resource-governor"));
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
    }
    cases.push((source, rows));
    cases
}

/// One large observed reservation and several smaller positive reservations share the same native resource
/// population. The resource has four native grantees; this database capture deliberately retains only a subset.
pub fn reservation_demand_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (source, rows) = request_memory_evidence_cases()
        .into_iter()
        .find(|(_, rows)| {
            rows.iter().any(|row| {
                matches!(row,
            TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. })
                if grant.resource_observation.is_some() && grant.workload_group_tempdb.is_none())
            })
        })
        .expect("native admission-resource fixture");
    let grant = |state| {
        rows.iter()
            .find_map(|row| match row {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) if grant.state == state => Some(grant.as_ref().clone()),
                _ => None,
            })
            .unwrap()
    };
    let template = grant(ExecutionGrantState::Granted);
    let waiter = grant(ExecutionGrantState::Waiting);
    [(vec![12288], true), (vec![2048, 4096, 4096], true), (vec![2048, 4096], false)]
        .into_iter()
        .map(|(amounts, complete)| {
            let mut grants = vec![waiter.clone()];
            for (index, amount) in amounts.into_iter().enumerate() {
                let mut grant = template.clone();
                let session = (71 + index).to_string();
                grant.identity.id = format!("{session}:0");
                let request = grant.request.as_mut().unwrap();
                request.key.session.id = session.clone();
                request.statement_identity = Some(NativeIdentity::new(format!("native-statement-{session}")));
                grant.granted_kibibytes = Some(amount.to_string());
                grant.maximum_used_kibibytes = Some("9007199254740993".into());
                grants.push(grant);
            }
            for grant in &mut grants {
                grant.resource = Some(
                    NativeIdentity::new("2")
                        .qualified("kind", "sql-server-resource-governor")
                        .qualified("resource_semaphore_id", "0"),
                );
                grant.resource_observation = Some(ExecutionGrantResourceObservation {
                    available_kibibytes: Some("-64".into()),
                    granted_kibibytes: Some("16384".into()),
                    grantee_count: Some("4".into()),
                    waiter_count: Some("1".into()),
                });
            }
            let records = grants.into_iter().map(record).collect::<Vec<_>>();
            let mut source = source.clone();
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            snapshot.records = records.len() as u32;
            snapshot.sources = vec![StateSourceCoverage::observed(source.source.clone(), snapshot.records, complete)];
            (source, records)
        })
        .collect()
}

fn record(grant: ExecutionGrantObservation) -> TelemetryRecord {
    assert!(grant.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: grant.source.clone(),
        key: grant.key().unwrap(),
        fact: OperationalFact::ExecutionGrant(Box::new(grant)),
    })
}

/// The same original native resource roster distinguishes a rebuild's reservation from an affected rebuild.
/// Partial visibility retains the positive participants without claiming that they caused the queue.
pub fn maintenance_memory_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (source, rows) = reservation_demand_evidence_cases().remove(0);
    [(ExecutionGrantState::Granted, true), (ExecutionGrantState::Waiting, true), (ExecutionGrantState::Granted, false)]
        .into_iter()
        .map(|(maintenance_state, complete)| {
            let mut records = rows.clone();
            for record in &mut records {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) = record
                else {
                    unreachable!()
                };
                if grant.state == maintenance_state {
                    grant.request.as_mut().unwrap().statement = DefinitionPayload::sql(
                        "ALTER INDEX ix_orders_customer ON dbo.orders REBUILD",
                        culprit_core::Engine::Mssql,
                    );
                }
            }
            let mut source = source.clone();
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            snapshot.sources = vec![StateSourceCoverage::observed(source.source.clone(), snapshot.records, complete)];
            (source, records)
        })
        .collect()
}

/// The original admission queue and the completed selected plan have separate native clocks. Their shared
/// request token is context only: neither this corpus nor a retained plan invents a grant blocker or parameter cause.
pub fn scalar_grant_pressure_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (plan_source, definitions) = parameter_evidence(ParameterEvidenceCase::RecordedGrantPressure);
    let TelemetryRecord::ObjectDefinition(definition) = &definitions[0] else { unreachable!() };
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition.fact else { unreachable!() };
    let (mut source, records) = output_wait_grant_evidence();
    source.target = plan_source.target;
    source.partition = plan_source.partition;
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.observed_from = "2020-01-01T11:59:58.1000000Z".into();
    snapshot.observed_until = "2020-01-01T11:59:58.9000000Z".into();
    let rows = records
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = row
            else {
                unreachable!()
            };
            let mut grant = *grant;
            grant.observed_at = "2020-01-01T11:59:58.5000000Z".into();
            grant.request.as_mut().unwrap().database = Some(source.target.database().unwrap().database.clone());
            if grant.state == ExecutionGrantState::Waiting {
                let request = grant.request.as_mut().unwrap();
                request.statement_identity = Some(
                    NativeIdentity::new(native.context.identity.sql_handle.trim_start_matches("0x"))
                        .qualified("native_object", "sys.dm_exec_requests.sql_handle")
                        .qualified("database_id", native.context.database_id.to_string())
                        .qualified("statement_start_offset", native.context.identity.statement_start_bytes.to_string())
                        .qualified("statement_end_offset", native.context.identity.statement_end_bytes.to_string()),
                );
                request.query_hash =
                    native.context.query_hash.as_deref().map(|value| value.trim_start_matches("0x").to_owned());
                request.statement = native.statement.payload.clone();
            }
            record(grant)
        })
        .collect();
    (source, rows)
}
