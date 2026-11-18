//! Controlled original inputs for canonical warehouse replay. Historical counters and current grants remain
//! separate captures; neither native acquisition support nor a causal relationship is established by this corpus.
use super::*;
use crate::analytical::evidence::EvidenceCapture;

pub fn output_wait_counter_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut records) = rbio_wait_evidence();
    records.truncate(1);
    let TelemetryRecord::WaitCounter(counter) = &mut records[0] else { panic!("native wait fixture") };
    counter.wait_type = "ASYNC_NETWORK_IO".into();
    counter.mechanism = Some(WaitMechanism::ClientResultOutput);
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!("wait snapshot") };
    snapshot.records = 1;
    snapshot.sources[0].records = 1;
    (source, records)
}

pub fn output_wait_grant_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, mut records) = request_memory_evidence_cases().remove(0);
    for record in &mut records {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::ExecutionGrant(grant), ..
        }) = record
        else {
            panic!("native current grant fixture")
        };
        grant.resource = Some(
            NativeIdentity::new("2")
                .qualified("resource_semaphore_id", "0")
                .qualified("kind", "sql-server-resource-governor"),
        );
        grant.native_properties.insert("pool_id".into(), "2".into());
        grant.native_null_columns.retain(|column| column != "pool_id");
        let request = grant.request.as_mut().expect("generation-checked native request");
        request.statement_identity = Some(NativeIdentity::new("fixture-current-statement"));
        request.wait = Some(ActivityWait {
            class: None,
            native_type: Some(
                match grant.state {
                    ExecutionGrantState::Granted => "ASYNC_NETWORK_IO",
                    ExecutionGrantState::Waiting => "RESOURCE_SEMAPHORE",
                }
                .into(),
            ),
            elapsed_ms: Some("3000".into()),
            owners: vec![],
            identified_resources: vec![],
            resources: None,
        });
        if grant.state == ExecutionGrantState::Granted {
            grant.granted_kibibytes = Some("9007199254740993".into());
        }
        assert!(grant.is_valid());
    }
    (source, records)
}

pub fn assert_grant_output_capture(
    capture: &EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), records.len());
    for (index, (actual, native)) in capture.records().iter().zip(records).enumerate() {
        assert_eq!(actual.row_ordinal() as usize, index + 1);
        assert_eq!(actual.row(), &ExportedRecord::new(source, capture.capture(), native.clone()).unwrap());
    }
    match &capture.records()[0].row().record {
        TelemetryRecord::WaitCounter(counter) => {
            assert_eq!(records.len(), 1);
            assert_eq!(source.partition, SourcePartition::Target);
            assert_eq!(counter.wait_type, "ASYNC_NETWORK_IO");
            assert_eq!(counter.mechanism, Some(WaitMechanism::ClientResultOutput));
            assert_eq!(counter.scope, culprit_core::MeasurementScope::Database);
            assert_eq!(counter.reset, culprit_core::ResetMarker::Unknown);
            assert_eq!(counter.waiting_tasks_count, "9007199254740993");
            assert_eq!(counter.wait_time_ms, "9007199254740993");
            assert_eq!(counter.signal_wait_time_ms, "0");
            assert_eq!(counter.max_wait_time_ms, "7");
        }
        TelemetryRecord::Operational(_) => {
            assert_memory_participants_capture(capture, source, records);
            assert!(NativeReadContext::from_snapshot(source).unwrap().has_qualified_local_owner());
            let grants: Vec<_> = capture
                .records()
                .iter()
                .map(|record| match &record.row().record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::ExecutionGrant(grant),
                        ..
                    }) => grant.as_ref(),
                    _ => panic!("native current grant"),
                })
                .collect();
            assert_eq!(grants.len(), 2);
            let holder = grants[0];
            let waiter = grants[1];
            assert_eq!(holder.state, ExecutionGrantState::Granted);
            assert_eq!(waiter.state, ExecutionGrantState::Waiting);
            assert_eq!(holder.granted_kibibytes.as_deref(), Some("9007199254740993"));
            assert_eq!(waiter.requested_kibibytes.as_deref(), Some("9007199254740993"));
            assert_eq!(waiter.granted_kibibytes.as_deref(), Some("0"));
            assert!(holder.shares_admission_resource(waiter, "db"));
            assert_ne!(holder.identity, waiter.identity);
            for (grant, expected_wait) in [(holder, "ASYNC_NETWORK_IO"), (waiter, "RESOURCE_SEMAPHORE")] {
                let request = grant.request.as_ref().unwrap();
                let wait = request.wait.as_ref().unwrap();
                assert_eq!(wait.native_type.as_deref(), Some(expected_wait));
                assert!(wait.owners.is_empty());
                assert!(wait.identified_resources.is_empty());
                assert!(wait.resources.is_none());
                assert_eq!(grant.observed_at, "2020-01-01T12:00:00.000000000Z");
                assert_eq!(grant.requested_without_offset, "2020-01-01T10:00:00.003");
                assert_eq!(
                    grant.identity.qualifiers.get("requested_without_offset"),
                    Some(&grant.requested_without_offset)
                );
            }
        }
        _ => panic!("separate historical wait or current grant capture"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantOutputCase {
    Qualified,
    SharedLoginEstablished,
    NegativeMarsPeer,
    UnknownOwner,
    MissingStatementIdentity,
    MissingResource,
    SqlUnavailable,
}
impl GrantOutputCase {
    pub const ALL: [Self; 7] = [
        Self::Qualified,
        Self::SharedLoginEstablished,
        Self::NegativeMarsPeer,
        Self::UnknownOwner,
        Self::MissingStatementIdentity,
        Self::MissingResource,
        Self::SqlUnavailable,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Qualified => "qualified",
            Self::SharedLoginEstablished => "shared_login",
            Self::NegativeMarsPeer => "negative_mars_peer",
            Self::UnknownOwner => "unknown_owner",
            Self::MissingStatementIdentity => "missing_statement_identity",
            Self::MissingResource => "missing_resource",
            Self::SqlUnavailable => "sql_unavailable",
        }
    }
}

/// These are controlled original source inputs, not real-user ownership, cost or live-client classifications.
pub fn grant_output_evidence(case: GrantOutputCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut records) = output_wait_grant_evidence();
    if matches!(case, GrantOutputCase::SharedLoginEstablished | GrantOutputCase::NegativeMarsPeer) {
        let mut peer = records[0].clone();
        let grant = grant_mut(&mut peer);
        grant.identity.id = "61:1".into();
        grant.resource = None;
        let request = grant.request.as_mut().unwrap();
        request.key.request.as_mut().unwrap().id = "1".into();
        request.wait = None;
        request.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle: if case == GrantOutputCase::NegativeMarsPeer {
                SessionLifecycle::PoolResetPrelogin
            } else {
                SessionLifecycle::Running
            },
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        assert!(grant.is_valid());
        peer = TelemetryRecord::Operational(OperationalObservation {
            key: grant.key().unwrap(),
            source: grant.source.clone(),
            fact: OperationalFact::ExecutionGrant(Box::new(grant.clone())),
        });
        records.push(peer);
    }
    let holder = grant_mut(&mut records[0]);
    match case {
        GrantOutputCase::UnknownOwner => {
            source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
        }
        GrantOutputCase::MissingStatementIdentity => holder.request.as_mut().unwrap().statement_identity = None,
        GrantOutputCase::MissingResource => holder.resource = None,
        GrantOutputCase::SqlUnavailable => {
            holder.statement_relationship = GrantStatementRelationship::Unresolved;
            holder.request.as_mut().unwrap().statement =
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        }
        _ => {}
    }
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!("current grant snapshot") };
    snapshot.records = records.len() as u32;
    snapshot.sources = vec![StateSourceCoverage::observed(
        culprit_core::source_id::CURRENT_REQUEST_MEMORY.into(),
        records.len() as u32,
        !matches!(case, GrantOutputCase::UnknownOwner | GrantOutputCase::NegativeMarsPeer),
    )];
    (source, records)
}

fn grant_mut(record: &mut TelemetryRecord) -> &mut ExecutionGrantObservation {
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. }) =
        record
    else {
        panic!("native current grant fixture")
    };
    grant.as_mut()
}
