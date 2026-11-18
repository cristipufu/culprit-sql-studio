//! Controlled native-shaped Agent facts exercise transport, not native cost or production availability.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};
#[derive(Clone, Copy, Debug)]
pub enum MssqlAgentJobsCase {
    Failed,
    Succeeded,
    Retry,
    Cancelled,
    InProgress,
    NullServiceStatus,
    OrphanHistory,
    Empty,
    OwnerFilteredEmpty,
    Denied,
    ChangedLayout,
    AgentRestart,
    AzureNotApplicable,
}
impl MssqlAgentJobsCase {
    pub const ALL: &[Self] = &[
        Self::Failed,
        Self::Succeeded,
        Self::Retry,
        Self::Cancelled,
        Self::InProgress,
        Self::NullServiceStatus,
        Self::OrphanHistory,
        Self::Empty,
        Self::OwnerFilteredEmpty,
        Self::Denied,
        Self::ChangedLayout,
        Self::AgentRestart,
        Self::AzureNotApplicable,
    ];
}
pub fn mssql_agent_jobs_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    MssqlAgentJobsCase::ALL.iter().copied().map(mssql_agent_jobs_evidence).collect()
}
pub fn mssql_agent_jobs_evidence(case: MssqlAgentJobsCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut metadata = metadata();
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-agent-machine").qualified("kind", "sql-server-network-machine"),
        native: NativeIdentity::new("private-agent-instance").qualified("kind", "sql-server-instance"),
    };
    let mut target = CollectionTarget::Shared(member.clone());
    let mut partition = SourcePartition::InstanceLocal {
        member: Some(member),
        epoch: Some(digest(b"controlled-agent-process-generation")),
    };
    let mut facts = vec![];
    match case {
        MssqlAgentJobsCase::Empty | MssqlAgentJobsCase::OwnerFilteredEmpty => {}
        MssqlAgentJobsCase::Denied => {
            for capability in &mut metadata.capabilities {
                capability.access = MssqlAgentAccess::PermissionDenied;
            }
        }
        MssqlAgentJobsCase::AzureNotApplicable => {
            target =
                DatabaseIdentity::server(Engine::Mssql, "azure-agent-unavailable", "owned.invalid", 1433, None).into();
            partition = SourcePartition::Target;
            metadata.engine_edition = 5;
            metadata.availability = MssqlAgentAvailability::NotApplicable;
            metadata.visibility = MssqlAgentVisibility::Unknown;
            metadata.recheck = MssqlAgentMetadataRecheck::NotRead;
            for capability in &mut metadata.capabilities {
                capability.access = MssqlAgentAccess::NotApplicable;
                capability.object_id = None;
                capability.columns_sha256 = None;
            }
        }
        _ => {
            let mut before = context(MssqlAgentContextBound::Before);
            let mut after = context(MssqlAgentContextBound::After);
            if matches!(case, MssqlAgentJobsCase::NullServiceStatus) {
                before.services[0].status = None;
                after.services[0].status = None;
            }
            if matches!(case, MssqlAgentJobsCase::AgentRestart) {
                after.session.as_mut().unwrap().session_id = "2147483646".into();
            }
            facts.push(OperationalFact::MssqlAgentContext(Box::new(before)));
            facts.push(OperationalFact::MssqlAgentJob(Box::new(job())));
            facts.push(OperationalFact::MssqlAgentStep(Box::new(step())));
            let mut history = history();
            history.run_status = match case {
                MssqlAgentJobsCase::Succeeded => 1,
                MssqlAgentJobsCase::Retry => 2,
                MssqlAgentJobsCase::Cancelled => 3,
                MssqlAgentJobsCase::InProgress => 4,
                _ => 0,
            };
            if matches!(case, MssqlAgentJobsCase::OrphanHistory) {
                history.job_id = "dddddddd-1111-2222-3333-eeeeeeeeeeee".into();
            }
            facts.push(OperationalFact::MssqlAgentHistory(Box::new(history)));
            facts.push(OperationalFact::MssqlAgentContext(Box::new(after)));
        }
    }
    if matches!(case, MssqlAgentJobsCase::OwnerFilteredEmpty | MssqlAgentJobsCase::Denied) {
        metadata.visibility = MssqlAgentVisibility::ExplicitSelect;
    }
    if matches!(case, MssqlAgentJobsCase::ChangedLayout) {
        metadata.recheck = MssqlAgentMetadataRecheck::Changed;
    }
    let rows = facts
        .into_iter()
        .map(|fact| {
            let (source, key) = match &fact {
                OperationalFact::MssqlAgentJob(point) => (MSSQL_AGENT_JOBS_SOURCE, point.key()),
                OperationalFact::MssqlAgentStep(point) => (MSSQL_AGENT_STEPS_SOURCE, point.key()),
                OperationalFact::MssqlAgentHistory(point) => (MSSQL_AGENT_HISTORY_SOURCE, point.key()),
                OperationalFact::MssqlAgentContext(point) => (MSSQL_AGENT_CONTEXT_SOURCE, point.key()),
                _ => panic!("the fixture contains only native Agent records"),
            };
            OperationalObservation { source: source.into(), key: key.unwrap(), fact }
        })
        .collect::<Vec<_>>();
    let sources = MSSQL_AGENT_SOURCES
        .into_iter()
        .map(|source| {
            if matches!(case, MssqlAgentJobsCase::AzureNotApplicable) {
                return StateSourceCoverage::not_applicable(source.into());
            }
            if matches!(case, MssqlAgentJobsCase::Denied) {
                return StateSourceCoverage::unavailable(source.into(), ErrorCode::PermissionDenied, None);
            }
            let count = rows.iter().filter(|row| row.source == source).count() as u32;
            let complete = source != MSSQL_AGENT_HISTORY_SOURCE
                && (source != MSSQL_AGENT_CONTEXT_SOURCE || count == 2)
                && !matches!(
                    case,
                    MssqlAgentJobsCase::OwnerFilteredEmpty
                        | MssqlAgentJobsCase::ChangedLayout
                        | MssqlAgentJobsCase::AgentRestart
                );
            StateSourceCoverage::observed(source.into(), count, complete)
        })
        .collect();
    let source = CollectionDescription {
        target,
        partition,
        source: MSSQL_AGENT_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlAgentJobs {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: rows.len() as u32,
                sources,
            },
            metadata: Box::new(metadata),
        },
    };
    assert!(source.partition_is_consistent());
    assert!(JobsSnapshot::from_source(&source, rows.clone(), true).unwrap().is_valid());
    (source, rows.into_iter().map(TelemetryRecord::Operational).collect())
}
const FROM: &str = "2026-11-15T12:00:00.000000001Z";
const UNTIL: &str = "2026-11-15T12:00:01.000000009Z";
const JOB: &str = "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb";
fn metadata() -> MssqlAgentJobsMetadata {
    MssqlAgentJobsMetadata {
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
        .map(|(i, object)| MssqlAgentCapability {
            object,
            access: MssqlAgentAccess::Available,
            columns_sha256: Some(digest(object.native_name().as_bytes())),
            object_id: Some((100 + i).to_string()),
        })
        .collect(),
    }
}
fn context(bound: MssqlAgentContextBound) -> MssqlAgentContext {
    MssqlAgentContext {
        observed_at: match bound {
            MssqlAgentContextBound::Before => FROM,
            MssqlAgentContextBound::After => UNTIL,
        }
        .into(),
        bound,
        session: Some(MssqlAgentSession {
            session_id: "2147483647".into(),
            started_without_offset: Some("2026-11-15T13:55:00.123".into()),
        }),
        services: vec![MssqlAgentService {
            name: "SQL Server Agent (private-agent-instance)".into(),
            status: Some(4),
            started_with_offset: Some("2026-11-15T13:55:00.1234567+02:00".into()),
        }],
        services_limited: false,
    }
}
fn job() -> MssqlAgentJob {
    MssqlAgentJob {
        observed_at: FROM.into(),
        job_id: JOB.into(),
        name: "Nightly <script>private-agent-label</script>".into(),
        enabled: true,
        start_step_id: "1".into(),
        version_number: "77".into(),
        created_without_offset: Some("2026-10-15T12:00:00.000".into()),
        modified_without_offset: Some("2026-11-15T12:00:00.000".into()),
        activity: Some(MssqlAgentJobActivity {
            agent_session_id: "2147483647".into(),
            requested_without_offset: None,
            request_source: Some("4".into()),
            queued_without_offset: None,
            started_without_offset: Some("2026-11-15T13:59:00.000".into()),
            stopped_without_offset: None,
            last_executed_step_id: Some("0".into()),
            last_executed_step_without_offset: None,
            next_scheduled_without_offset: None,
        }),
    }
}
fn step() -> MssqlAgentStep {
    MssqlAgentStep {
        observed_at: FROM.into(),
        job_id: JOB.into(),
        step_id: "1".into(),
        name: "Current step".into(),
        subsystem: "TSQL".into(),
        database_name: Some("private-starting-database".into()),
    }
}
fn history() -> MssqlAgentHistory {
    MssqlAgentHistory {
        observed_at: UNTIL.into(),
        job_id: JOB.into(),
        instance_id: "2147483647".into(),
        step_id: "1".into(),
        stored_step_name: Some("Original recorded step".into()),
        run_status: 0,
        run_date: "20261115".into(),
        run_time: "123456".into(),
        run_duration: "250101".into(),
        sql_message_id: "0".into(),
        sql_severity: "0".into(),
        retries_attempted: "0".into(),
        reported_server: Some("private-recorded-server".into()),
    }
}
