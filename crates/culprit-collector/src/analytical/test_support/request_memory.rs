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
    [
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
