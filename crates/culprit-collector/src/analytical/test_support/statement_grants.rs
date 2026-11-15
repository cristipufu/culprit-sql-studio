//! Synthetic original cache-entry facts exercise transport fidelity; no production cost or ownership is inferred.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode, ResetMarker, source_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementGrantCase {
    Complete,
    Partial,
    UnknownOwner,
    UnavailableStatement,
    Empty,
    UnavailableSource,
    LargeStatement,
}

pub fn statement_grant_evidence_cases() -> Vec<(StatementGrantCase, CollectionDescription, Vec<TelemetryRecord>)> {
    [
        StatementGrantCase::Complete,
        StatementGrantCase::Partial,
        StatementGrantCase::UnknownOwner,
        StatementGrantCase::UnavailableStatement,
        StatementGrantCase::Empty,
        StatementGrantCase::UnavailableSource,
    ]
    .into_iter()
    .map(|case| {
        let (source, records) = statement_grant_evidence(case);
        (case, source, records)
    })
    .collect()
}

pub fn statement_grant_evidence(case: StatementGrantCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut first = native_grant();
    let mut second = first.clone();
    second.context.identity.plan_handle = "0xAABB".into();
    second.context.identity.sql_handle = "0xCCDD".into();
    second.context.identity.plan_generation = "2".into();
    second.executions = Some("9007199254740993".into());
    second.total_grant_kibibytes = Some("9007199254740993".into());
    second.total_used_kibibytes = Some("0".into());
    second.last_grant_kibibytes = Some("1".into());
    second.last_used_kibibytes = Some("0".into());
    second.min_grant_kibibytes = Some("1".into());
    second.min_used_kibibytes = Some("0".into());
    second.max_grant_kibibytes = Some("1".into());
    second.max_used_kibibytes = Some("0".into());
    if case == StatementGrantCase::UnavailableStatement {
        first.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        second.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
    }
    if case == StatementGrantCase::LargeStatement {
        let text = format!(
            "SELECT {} FROM dbo.grant_input",
            (0..50).map(|index| format!("[éééééééééé{index}]")).collect::<Vec<_>>().join(",")
        );
        assert!(text.encode_utf16().count() <= 1024, "within the native reader's statement prefix bound");
        assert!(text.len() > 1024, "the warehouse preview is bounded in UTF-8 bytes");
        first.context.source_text_bytes = Some((text.encode_utf16().count() * 2).to_string());
        first.statement = DefinitionPayload::sql(&text, Engine::Mssql);
    }
    let records = if matches!(case, StatementGrantCase::Empty | StatementGrantCase::UnavailableSource) {
        Vec::new()
    } else {
        [first, second]
            .into_iter()
            .map(|row| {
                assert!(row.is_valid());
                TelemetryRecord::Operational(OperationalObservation {
                    source: source_id::GRANTS.into(),
                    key: row.context.identity.key().unwrap(),
                    fact: OperationalFact::MssqlStatementGrants(Box::new(row)),
                })
            })
            .collect::<Vec<_>>()
    };
    let qualified = case != StatementGrantCase::UnknownOwner;
    let coverage = if case == StatementGrantCase::UnavailableSource {
        StateSourceCoverage::unavailable(source_id::GRANTS.into(), ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(
            source_id::GRANTS.into(),
            records.len() as u32,
            qualified && case != StatementGrantCase::Partial,
        )
    };
    let source = CollectionDescription {
        target: CollectionTarget::Database(DatabaseIdentity::server(
            Engine::Mssql,
            "fixture",
            "fixture.invalid",
            1433,
            None,
        )),
        partition: SourcePartition::InstanceLocal {
            member: qualified.then(|| SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("synthetic-instance-namespace"),
                native: NativeIdentity::new("native-member-a"),
            }),
            epoch: qualified.then(|| "native-process-generation-1".into()),
        },
        source: source_id::GRANTS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::StatementGrants,
            observed_from: "2020-01-01T06:00:00Z".into(),
            observed_until: "2020-01-01T06:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![coverage],
        }),
    };
    (source, records)
}

fn native_grant() -> StatementGrantStatistics {
    let text = "SELECT value FROM dbo.grant_input WHERE id=9264";
    StatementGrantStatistics {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2019-12-31T22:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: Some((text.encode_utf16().count() * 2).to_string()),
            source_text_complete: true,
        },
        statement: DefinitionPayload::sql(text, Engine::Mssql),
        available_columns: [
            "execution_count",
            "total_grant_kb",
            "total_used_grant_kb",
            "last_grant_kb",
            "last_used_grant_kb",
            "min_grant_kb",
            "min_used_grant_kb",
            "max_grant_kb",
            "max_used_grant_kb",
            "last_execution_time",
        ]
        .map(str::to_string)
        .to_vec(),
        executions: Some("40".into()),
        last_execution_without_offset: Some("2020-01-01T02:59:59.997".into()),
        total_grant_kibibytes: Some("83886080".into()),
        total_used_kibibytes: Some("1638400".into()),
        last_grant_kibibytes: Some("102400".into()),
        last_used_kibibytes: Some("61440".into()),
        min_grant_kibibytes: Some("102400".into()),
        min_used_kibibytes: Some("10240".into()),
        max_grant_kibibytes: Some("3145728".into()),
        max_used_kibibytes: Some("81920".into()),
        reset: ResetMarker::Unknown,
    }
}

pub fn assert_statement_grant_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_operational_capture(capture, source, records);
    assert_eq!(source.observation.snapshot().unwrap().dataset, SnapshotDataset::StatementGrants);
    assert!(matches!(source.partition, SourcePartition::InstanceLocal { .. }));
    for row in capture.records() {
        let fields = row.row().record.definition_payloads().collect::<Vec<_>>();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].pointer, "/record/data/fact/data/statement");
    }
}
