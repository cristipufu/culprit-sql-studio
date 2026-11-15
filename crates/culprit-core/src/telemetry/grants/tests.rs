use super::*;
use crate::{DatabaseIdentity, Engine, ResetMarker};

fn row(generation: &str) -> StatementGrantStatistics {
    let statement = "SELECT payload FROM dbo.orders";
    StatementGrantStatistics {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2020-01-01T10:00:00.0030000".into(),
                plan_generation: generation.into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: Some((statement.encode_utf16().count() * 2).to_string()),
            source_text_complete: true,
        },
        statement: DefinitionPayload::sql(statement, Engine::Mssql),
        available_columns: vec!["total_grant_kb".into(), "total_used_grant_kb".into()],
        executions: Some("40".into()),
        last_execution_without_offset: Some("2020-01-01T10:01:00.0070000".into()),
        total_grant_kibibytes: Some("9007199254740993".into()),
        total_used_kibibytes: Some("0".into()),
        last_grant_kibibytes: None,
        last_used_kibibytes: None,
        min_grant_kibibytes: None,
        min_used_kibibytes: None,
        max_grant_kibibytes: None,
        max_used_kibibytes: None,
        reset: ResetMarker::Unknown,
    }
}

fn read() -> StatementGrantSnapshot {
    StatementGrantSnapshot {
        capture: CaptureIdentity { observation_id: "grant-read-a".into(), revision: None },
        description: CollectionDescription {
            target: DatabaseIdentity::server(Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Node,
                    namespace: NativeIdentity::new("native-database-namespace"),
                    native: NativeIdentity::new("native-member-a"),
                }),
                epoch: Some("native-process-1".into()),
            },
            source: crate::source_id::GRANTS.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementGrants,
                observed_from: "2020-01-01T06:02:00Z".into(),
                observed_until: "2020-01-01T06:02:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(crate::source_id::GRANTS.into(), 1, false)],
            }),
        },
        observations: vec![row("1")],
    }
}

fn header(read: &mut StatementGrantSnapshot) -> &mut StateSnapshotDescription {
    let SourceObservation::Snapshot(snapshot) = &mut read.description.observation else { panic!("grant header") };
    snapshot
}

#[test]
fn cache_grants_keep_native_identity_independent_of_current_values_and_observation_limits_are_explicit() {
    let mut identity = row("1").context.identity;
    assert!(identity.is_valid());
    let original = identity.key().unwrap();
    let mut equivalent = identity.clone();
    equivalent.created_without_offset = "2020-01-01T10:00:00.003".into();
    equivalent.plan_generation = "01".into();
    assert_eq!(equivalent.key().unwrap(), original);
    equivalent.plan_handle = "0xAABB".into();
    let upper = equivalent.key().unwrap();
    equivalent.plan_handle = "0xaabb".into();
    assert_eq!(equivalent.key().unwrap(), upper);
    identity.plan_generation = "2".into();
    assert_ne!(identity.key().unwrap(), original);
    identity.statement_start_bytes = 3;
    assert!(!identity.is_valid(), "UTF-16 offsets are byte pairs");
    let mut budget = CollectionBudget::upper_limits();
    assert!(budget.validate().is_ok());
    budget.max_snapshot_rows += 1;
    assert!(budget.validate().is_err());
}

#[test]
fn capture_identity_separates_same_clock_reads_without_changing_a_qualified_entry_identity() {
    let first = read();
    let mut second = first.clone();
    second.capture.observation_id = "grant-read-b".into();
    assert!(first.is_valid() && second.is_valid());
    assert_eq!(first.description, second.description);
    assert_eq!(first.observations, second.observations);
    assert_ne!(first.key().unwrap(), second.key().unwrap());
    assert_eq!(first.scope_key().unwrap(), second.scope_key().unwrap());
    assert_eq!(first.entry_key(&first.observations[0]).unwrap(), second.entry_key(&second.observations[0]).unwrap());

    second.observations[0].total_grant_kibibytes = Some("9007199254740994".into());
    assert_eq!(first.entry_key(&first.observations[0]).unwrap(), second.entry_key(&second.observations[0]).unwrap());
    second.observations[0].context.identity.plan_generation = "2".into();
    assert_ne!(first.entry_key(&first.observations[0]).unwrap(), second.entry_key(&second.observations[0]).unwrap());
}

#[test]
fn missing_native_member_or_epoch_isolated_to_its_original_read_never_becomes_a_stable_scope() {
    for missing in ["member", "epoch", "both"] {
        let mut first = read();
        let SourcePartition::InstanceLocal { member, epoch } = &mut first.description.partition else { panic!() };
        if missing != "epoch" {
            *member = None;
        }
        if missing != "member" {
            *epoch = None;
        }
        let mut second = first.clone();
        second.capture.observation_id = "grant-read-b".into();
        assert!(first.is_valid() && second.is_valid(), "{missing}");
        assert!(!first.description.is_partition_qualified());
        assert_eq!(first.scope_key().unwrap(), first.key().unwrap());
        assert_ne!(first.scope_key().unwrap(), second.scope_key().unwrap());
        assert_ne!(
            first.entry_key(&first.observations[0]).unwrap(),
            second.entry_key(&second.observations[0]).unwrap()
        );
        assert_eq!(first.observations[0].total_grant_kibibytes.as_deref(), Some("9007199254740993"));
        let raw = serde_json::to_value(&first).unwrap();
        let decoded: StatementGrantSnapshot = serde_json::from_value(raw).unwrap();
        assert_eq!(decoded, first);
    }
}

#[test]
fn native_member_epoch_and_authenticated_target_partition_qualified_entry_keys() {
    let original = read();
    for changed in ["member", "epoch", "target"] {
        let mut other = original.clone();
        match changed {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut other.description.partition
                else {
                    panic!()
                };
                member.native = NativeIdentity::new("native-member-b");
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut other.description.partition else { panic!() };
                *epoch = Some("native-process-2".into());
            }
            _ => {
                other.description.target =
                    DatabaseIdentity::server(Engine::Mssql, "other-database", "fixture.invalid", 1433, None).into();
            }
        }
        assert!(other.is_valid(), "{changed}");
        assert_ne!(original.scope_key().unwrap(), other.scope_key().unwrap(), "{changed}");
        assert_ne!(
            original.entry_key(&original.observations[0]).unwrap(),
            other.entry_key(&other.observations[0]).unwrap()
        );
    }
}

#[test]
fn duplicate_entries_and_native_header_contradictions_are_rejected_even_for_partial_readback() {
    for problem in ["duplicate", "source", "dataset", "backwards", "clock", "sum", "decoded-count", "row-limit"] {
        let mut invalid = read();
        match problem {
            "duplicate" => {
                invalid.observations.push(row("01"));
                header(&mut invalid).records = 2;
                header(&mut invalid).sources[0].records = 2;
            }
            "source" => header(&mut invalid).sources[0].source = "other-native-source".into(),
            "dataset" => header(&mut invalid).dataset = SnapshotDataset::CurrentRequestMemory,
            "backwards" => header(&mut invalid).observed_until = "2020-01-01T06:01:59Z".into(),
            "clock" => header(&mut invalid).observed_until = "2020-01-01T06:02:01".into(),
            "sum" => header(&mut invalid).sources[0].records = 2,
            "decoded-count" => {
                header(&mut invalid).records = 0;
                header(&mut invalid).sources[0].records = 0;
            }
            _ => {
                let too_many = CollectionBudget::upper_limits().max_snapshot_rows + 1;
                header(&mut invalid).records = too_many;
                header(&mut invalid).sources[0].records = too_many;
            }
        }
        assert!(!invalid.is_valid(), "{problem}");
        assert!(!invalid.is_valid_read(false), "{problem}: archive gaps cannot repair a contradictory native header");
    }
}

#[test]
fn target_member_contradictions_and_unqualified_complete_scope_cannot_authorize_a_read() {
    for problem in ["cross-engine", "invalid-target", "target-partition", "unqualified-complete"] {
        let mut invalid = read();
        match problem {
            "cross-engine" => {
                invalid.description.target =
                    DatabaseIdentity::server(Engine::Postgres, "sample", "fixture.invalid", 5432, None).into();
            }
            "invalid-target" => {
                invalid.description.target =
                    DatabaseIdentity::server(Engine::Mssql, "", "fixture.invalid", 1433, None).into();
            }
            "target-partition" => invalid.description.partition = SourcePartition::Target,
            _ => {
                invalid.description.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
                header(&mut invalid).sources[0] = StateSourceCoverage::collected(crate::source_id::GRANTS.into(), 1);
            }
        }
        assert!(!invalid.is_valid(), "{problem}");
        assert!(invalid.key().is_err(), "{problem}");
        assert!(invalid.scope_key().is_err(), "{problem}");
    }
}

#[test]
fn partial_archive_readback_retains_the_original_native_header_without_rewriting_its_membership() {
    let mut original = read();
    original.observations.push(row("2"));
    header(&mut original).records = 2;
    header(&mut original).sources[0].records = 2;
    assert!(original.is_valid());
    let mut partial = original.clone();
    partial.observations.pop();
    assert!(!partial.is_valid(), "a complete read must account for both native rows");
    assert!(partial.is_valid_read(false));
    assert_eq!(partial.description, original.description);
    assert_eq!(partial.key().unwrap(), original.key().unwrap());
    assert_eq!(partial.observations, original.observations[..1]);
}

#[test]
fn available_statement_needs_complete_native_text_positive_bytes_and_nonzero_handles() {
    for missing in ["complete", "bytes", "zero-bytes", "sql-handle", "plan-handle"] {
        let mut value = row("1");
        match missing {
            "complete" => value.context.source_text_complete = false,
            "bytes" => {
                value.context.source_text_complete = false;
                value.context.source_text_bytes = None;
            }
            "zero-bytes" => value.context.source_text_bytes = Some("0".into()),
            "sql-handle" => value.context.identity.sql_handle = "0x0000".into(),
            _ => {
                value.context.identity.plan_handle = "0x0000".into();
                value.executions = None;
                value.total_grant_kibibytes = None;
                value.total_used_kibibytes = None;
            }
        }
        assert!(value.context.is_valid(), "{missing}: native context remains independently valid");
        assert!(!value.is_valid(), "{missing}: available text must not gain provenance from cache counters");
        value.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
        assert!(value.is_valid(), "{missing}: explicit missing text does not erase a native observation");
    }
}
