use super::*;

/// One native task point per execution context. Counter sizes, SQL and later check outcomes never form identity.
pub fn task_space_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (manifest, _) = fixture();
    let first = observation("0");
    let second = observation("1");
    let full = vec![record(first), record(second)];
    let mut partial = full.clone();
    for (execution, state) in [("2", TaskRequestState::Changed), ("3", TaskRequestState::Missing)] {
        let mut row = observation(execution);
        row.after_check.as_mut().unwrap().state = state;
        row.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
        row.statement_identity = None;
        row.plan_identity = None;
        partial.push(record(row));
    }
    let mut unassigned = observation("4");
    unassigned.request = None;
    unassigned.before_check = None;
    unassigned.after_check = None;
    unassigned.statement_identity = None;
    unassigned.plan_identity = None;
    unassigned.user_allocated_pages = None;
    unassigned.internal_deallocated_pages = None;
    partial.push(record(unassigned));
    [
        (full, culprit_core::SourceState::Collected),
        (partial, culprit_core::SourceState::Partial),
        (vec![], culprit_core::SourceState::Empty),
        (vec![], culprit_core::SourceState::Unavailable),
    ]
    .into_iter()
    .map(|(records, state)| {
        let mut source = manifest.source.clone();
        source.source = "native-task-capture".into();
        source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::TaskSpace,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![if state == culprit_core::SourceState::Unavailable {
                StateSourceCoverage::unavailable(
                    culprit_core::source_id::TASK_SPACE.into(),
                    culprit_core::ErrorCode::PermissionDenied,
                    None,
                )
            } else {
                StateSourceCoverage::observed(
                    culprit_core::source_id::TASK_SPACE.into(),
                    records.len() as u32,
                    state != culprit_core::SourceState::Partial,
                )
            }],
        });
        (source, records)
    })
    .collect()
}

fn observation(execution: &str) -> TaskSpaceObservation {
    let mut request = activity_sample();
    request.statement =
        DefinitionPayload::sql("SELECT note FROM dbo.orders WHERE id=9264", culprit_core::Engine::Mssql);
    TaskSpaceObservation {
        source: culprit_core::source_id::TASK_SPACE.into(),
        observed_at: "2020-01-01T12:00:00.250000000Z".into(),
        task: TaskSpaceIdentity {
            session_id: request.key.session.id.clone(),
            request_id: request.key.request.as_ref().unwrap().id.clone(),
            execution_context_id: execution.into(),
        },
        native_database_id: "2".into(),
        bytes_per_page: 8192,
        user_allocated_pages: Some("9223372036854775807".into()),
        user_deallocated_pages: Some("0".into()),
        internal_allocated_pages: None,
        internal_deallocated_pages: Some("9007199254740993".into()),
        request: Some(request),
        before_check: Some(TaskRequestCheck {
            observed_at: "2020-01-01T12:00:00.300000000Z".into(),
            state: TaskRequestState::SameStatement,
        }),
        after_check: Some(TaskRequestCheck {
            observed_at: "2020-01-01T12:00:00.700000000Z".into(),
            state: TaskRequestState::SameStatement,
        }),
        statement_identity: Some(NativeIdentity::new("native-task-sql-handle")),
        plan_identity: Some(NativeIdentity::new("native-task-plan-handle")),
    }
}

fn record(row: TaskSpaceObservation) -> TelemetryRecord {
    assert!(row.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: row.source.clone(),
        key: row.key().unwrap(),
        fact: OperationalFact::TaskSpace(Box::new(row)),
    })
}

pub fn assert_task_space_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    assert!(source.target.database().is_some());
    assert_eq!(source.observation.snapshot().unwrap().dataset, SnapshotDataset::TaskSpace);
}
