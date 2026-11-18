use super::*;
fn native(status: i32) -> MssqlAgentJobsSnapshot {
    let member = SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("agent-machine"),
        native: NativeIdentity::new("agent-instance"),
    };
    let point = MssqlAgentHistory {
        observed_at: UNTIL.into(),
        job_id: "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb".into(),
        instance_id: "2147483647".into(),
        step_id: "1".into(),
        stored_step_name: Some("old recorded label".into()),
        run_status: status,
        run_date: "20200101".into(),
        run_time: "0".into(),
        run_duration: "250101".into(),
        sql_message_id: "0".into(),
        sql_severity: "0".into(),
        retries_attempted: "0".into(),
        reported_server: None,
    };
    MssqlAgentJobsSnapshot {
        context: NativeReadContext {
            target: CollectionTarget::Shared(member.clone()),
            partition: SourcePartition::InstanceLocal { member: Some(member), epoch: Some("agent-process".into()) },
            observed_from: FROM.into(),
            observed_until: UNTIL.into(),
            database_writable: None,
        },
        metadata: MssqlAgentJobsMetadata {
            engine_edition: 3,
            major_version: 16,
            availability: MssqlAgentAvailability::Supported,
            visibility: MssqlAgentVisibility::Sysadmin,
            history_limit: 256,
            recheck: MssqlAgentMetadataRecheck::Unchanged,
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
                object_id: Some((index + 100).to_string()),
            })
            .collect(),
        },
        sources: vec![
            StateSourceCoverage::collected(MSSQL_AGENT_JOBS_SOURCE.into(), 0),
            StateSourceCoverage::collected(MSSQL_AGENT_STEPS_SOURCE.into(), 0),
            StateSourceCoverage::observed(MSSQL_AGENT_HISTORY_SOURCE.into(), 1, false),
            StateSourceCoverage::unavailable(
                MSSQL_AGENT_CONTEXT_SOURCE.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
        ],
        observations: vec![OperationalObservation {
            source: MSSQL_AGENT_HISTORY_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MssqlAgentHistory(Box::new(point)),
        }],
    }
}
const FROM: &str = "2020-01-01T12:00:00.000000001Z";
const UNTIL: &str = "2020-01-01T12:00:01.000000009Z";
fn explain_agent(read: MssqlAgentJobsSnapshot, read_complete: bool) -> Result<JobsReport> {
    explain(&JobsSnapshot::MssqlAgent { snapshot: Box::new(read), read_complete })
}
#[test]
fn each_native_history_status_has_its_own_explanation_without_current_failure_or_owner_invention() {
    for (status, expected) in [
        (0, t!("view.mssql_agent_jobs.history.failed")),
        (1, t!("view.mssql_agent_jobs.history.succeeded")),
        (2, t!("view.mssql_agent_jobs.history.retry")),
        (3, t!("view.mssql_agent_jobs.history.cancelled")),
        (4, t!("view.mssql_agent_jobs.history.in_progress")),
        (-1, t!("view.mssql_agent_jobs.history.unknown")),
    ] {
        let read = native(status);
        let original = serde_json::to_value(&read).unwrap();
        let report = explain_agent(read.clone(), true).unwrap();
        assert_eq!(report.entries[0].explanation, expected);
        assert_eq!(report.context, read.context);
        assert_eq!(report.entries[0].observation_key, read.observations[0].key);
        assert!(report.entries[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("250101")));
        assert!(report.entries[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("0")));
        assert!(report.limitations.contains(&t!("view.mssql_agent_jobs.limit.local_clock")));
        assert!(report.limitations.contains(&t!("view.mssql_agent_jobs.limit.definition")));
        assert_eq!(serde_json::to_value(read).unwrap(), original);
    }
}
#[test]
fn native_denial_bounded_history_and_missing_archive_bodies_keep_independent_qualifications() {
    let mut read = native(0);
    read.observations.clear();
    assert!(explain_agent(read.clone(), true).is_err());
    let report = explain_agent(read, false).unwrap();
    assert_eq!((report.native_rows, report.observed_rows), (1, 0));
    assert!(!report.read_complete);
    assert!(report.entries.is_empty());
    assert_eq!(report.sources[2].state, SourceState::Partial);
    assert_eq!(report.sources[3].state, SourceState::Unavailable);
    assert!(report.limitations.contains(&t!("view.jobs.limit.read")));
}
#[test]
fn bounded_presentation_validates_the_unpresented_tail_and_never_changes_original_counts() {
    let mut read = native(1);
    let mut rows = vec![];
    for id in 1..=MAX_JOB_ENTRIES + 1 {
        let mut row = read.observations[0].clone();
        let OperationalFact::MssqlAgentHistory(point) = &mut row.fact else { panic!() };
        point.instance_id = id.to_string();
        if id == MAX_JOB_ENTRIES + 1 {
            point.run_status = 0;
        }
        row.key = point.key().unwrap();
        rows.push(row);
    }
    read.metadata.history_limit = rows.len() as u32;
    read.sources[2].records = rows.len() as u32;
    read.observations = rows;
    let wrapped = JobsSnapshot::MssqlAgent { snapshot: Box::new(read.clone()), read_complete: true };
    let report = explain_bounded(&wrapped, 5).unwrap();
    assert_eq!(report.entries.len(), 5);
    assert_eq!(report.observed_rows, MAX_JOB_ENTRIES as u32 + 1);
    assert!(report.entries_limited);
    assert_eq!(report.entries[0].explanation, t!("view.mssql_agent_jobs.history.failed"));
    read.observations.last_mut().unwrap().key = digest(b"different-original-key");
    assert!(explain_bounded(&JobsSnapshot::MssqlAgent { snapshot: Box::new(read), read_complete: true }, 5).is_err());
}
