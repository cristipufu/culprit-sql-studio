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

fn record(grant: ExecutionGrantObservation) -> TelemetryRecord {
    assert!(grant.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: grant.source.clone(),
        key: grant.key().unwrap(),
        fact: OperationalFact::ExecutionGrant(Box::new(grant)),
    })
}
