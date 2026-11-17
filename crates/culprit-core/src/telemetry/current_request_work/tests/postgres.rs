use super::*;

pub(super) fn copy_snapshot() -> CurrentRequestWorkSnapshot {
    let mut input = snapshot();
    input.profile = CurrentRequestWorkProfile::PostgresCopy;
    input.source = "pg_stat_progress_copy".into();
    input.coverage.source = input.source.clone();
    input.context.target =
        crate::DatabaseIdentity::server(Engine::Postgres, "sample", "fixture.invalid", 5432, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut input.context.partition else { panic!() };
    member.engine = Engine::Postgres;
    let point = &mut input.observations[0];
    point.actor = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "2020-01-01T10:00:00.000001Z".into() },
        request: Some(NativeActivityIdentity {
            id: "statement".into(),
            generation: "2020-01-01T11:00:00.000002Z".into(),
        }),
        transaction: None,
    };
    point.scope = ActivityScopeBasis::SessionDatabase;
    point.statement_identity = crate::engines::postgres_activity_statement_identity(
        &point.actor.session.id,
        &point.actor.session.generation,
        &point.actor.request.as_ref().unwrap().generation,
    );
    point.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    point.native = CurrentRequestWorkNative::PostgresCopy {
        progress: Box::new(PostgresCopyProgress {
            database_oid: "16384".into(),
            relation_oid: "0".into(),
            command: PostgresCopyCommand::To,
            io_type: PostgresCopyIo::Pipe,
            bytes_processed: Some("9223372036854775807".into()),
            bytes_total: Some("0".into()),
            tuples_processed: Some("9007199254740993".into()),
            tuples_excluded: Some("0".into()),
            tuples_skipped: None,
        }),
    };
    input
}

#[test]
fn native_copy_counts_zero_total_and_query_relation_survive_exact_export_and_readback() {
    let input = copy_snapshot();
    assert!(input.is_valid());
    let recovered: CurrentRequestWorkSnapshot = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(recovered, input);
    let CurrentRequestWorkNative::PostgresCopy { progress } = &recovered.observations[0].native else { panic!() };
    assert_eq!(progress.bytes_processed.as_deref(), Some("9223372036854775807"));
    assert_eq!(progress.bytes_total.as_deref(), Some("0"));
    assert_eq!(progress.tuples_processed.as_deref(), Some("9007199254740993"));
    assert_eq!(progress.tuples_skipped, None);
    assert_eq!(progress.relation_oid, "0");
    let descriptor = source(&input);
    let point = &input.observations[0];
    let row = ExportedRecord::new(
        &descriptor,
        &CaptureIdentity { observation_id: "copy-point".into(), revision: None },
        TelemetryRecord::Operational(OperationalObservation {
            source: input.source.clone(),
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point.clone())),
        }),
    )
    .unwrap();
    assert!(row.record.is_valid());
    let json = serde_json::to_value(&row).unwrap();
    assert!(!json.to_string().contains("percent"));
    assert!(!json.to_string().contains("phase"));
}

#[test]
fn copy_identity_never_borrows_mysql_or_a_replaced_backend_request_or_database() {
    for case in ["engine", "session", "backend", "query", "request", "scope", "statement", "database"] {
        let mut input = copy_snapshot();
        let point = &mut input.observations[0];
        match case {
            "engine" => {
                input.context.target =
                    crate::DatabaseIdentity::server(Engine::Mysql, "sample", "fixture.invalid", 3306, None).into()
            }
            "session" => point.actor.session.id = "43".into(),
            "backend" => point.actor.session.generation = "2020-01-01T10:00:01Z".into(),
            "query" => point.actor.request.as_mut().unwrap().generation = "2020-01-01T11:00:01Z".into(),
            "request" => point.actor.request.as_mut().unwrap().id = "42".into(),
            "scope" => point.scope = ActivityScopeBasis::RequestDatabase,
            "statement" => point.statement_identity = NativeIdentity::new("another-token"),
            _ => point.database = "other".into(),
        }
        assert!(!input.is_valid(), "{case}");
    }
    let mut input = copy_snapshot();
    input.context.target =
        crate::DatabaseIdentity::server(Engine::Yugabyte, "sample", "fixture.invalid", 5433, None).into();
    assert!(!input.is_valid(), "PostgreSQL SQL grammar does not authorize YSQL COPY facts");
}

#[test]
fn copy_unknowns_remain_unknown_and_malformed_native_numbers_are_not_repaired() {
    for field in ["database", "relation", "counter"] {
        for malformed in ["-1", "01", "18446744073709551616", "NaN"] {
            let mut input = copy_snapshot();
            let CurrentRequestWorkNative::PostgresCopy { progress } = &mut input.observations[0].native else {
                panic!()
            };
            match field {
                "database" => progress.database_oid = malformed.into(),
                "relation" => progress.relation_oid = malformed.into(),
                _ => progress.tuples_processed = Some(malformed.into()),
            }
            assert!(!input.is_valid(), "{field}={malformed}");
        }
    }
    let mut input = copy_snapshot();
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut input.observations[0].native else { panic!() };
    progress.database_oid = "0".into();
    assert!(!input.is_valid());
    let mut input = copy_snapshot();
    input.observations.clear();
    assert!(!input.is_valid());
    assert!(input.is_valid_read(false));
    assert_eq!(input.result.membership, FocusedRequestMembership::Observed);
    assert_eq!(input.coverage.records, 1);
}

#[test]
fn the_authenticated_profile_remains_required_even_when_the_native_body_is_missing() {
    let mut input = copy_snapshot();
    let mut wrong = source(&input);
    let SourceObservation::CurrentRequestWork { profile, .. } = &mut wrong.observation else { panic!() };
    *profile = CurrentRequestWorkProfile::MysqlStatementEvent;
    assert!(!wrong.partition_is_consistent());
    assert!(!input.observations[0].fits_source_scope(&wrong));
    input.observations.clear();
    assert!(input.is_valid_read(false));
    input.profile = CurrentRequestWorkProfile::MysqlStatementEvent;
    assert!(!input.is_valid_read(false));
    let value = serde_json::to_value(copy_snapshot()).unwrap();
    for replacement in [None, Some("unknown-profile")] {
        let mut damaged = value.clone();
        match replacement {
            None => {
                damaged.as_object_mut().unwrap().remove("profile");
            }
            Some(value) => damaged["profile"] = serde_json::json!(value),
        }
        assert!(serde_json::from_value::<CurrentRequestWorkSnapshot>(damaged).is_err());
    }
    let mut header = serde_json::to_value(source(&copy_snapshot())).unwrap();
    header["observation"]["data"].as_object_mut().unwrap().remove("profile");
    assert!(serde_json::from_value::<CollectionDescription>(header).is_err());
}

#[test]
fn a_logical_copy_origin_stays_structural_without_receiving_current_member_authority() {
    let input = copy_snapshot();
    let mut original = focus(&input, SourcePartition::Target);
    assert!(original.is_current_request_work_focus());
    original.target = crate::DatabaseIdentity::server(Engine::Yugabyte, "sample", "fixture.invalid", 5433, None).into();
    assert!(
        !original.is_current_request_work_focus(),
        "shared SQL grammar does not confer the PostgreSQL COPY profile"
    );
}

#[test]
fn native_copy_bigint_and_command_relation_contracts_reject_impossible_values_without_unsigned_repair() {
    let mut input = copy_snapshot();
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut input.observations[0].native else { panic!() };
    progress.bytes_processed = Some(i64::MAX.to_string());
    assert!(input.is_valid());
    let mut invalid = input.clone();
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut invalid.observations[0].native else { panic!() };
    progress.bytes_processed = Some("9223372036854775808".into());
    assert!(!invalid.is_valid());
    for counter in ["bytes_total", "tuples_processed", "tuples_excluded", "tuples_skipped"] {
        let mut invalid = input.clone();
        let CurrentRequestWorkNative::PostgresCopy { progress } = &mut invalid.observations[0].native else { panic!() };
        progress.command = PostgresCopyCommand::From;
        progress.relation_oid = "16420".into();
        let field = match counter {
            "bytes_total" => &mut progress.bytes_total,
            "tuples_processed" => &mut progress.tuples_processed,
            "tuples_excluded" => &mut progress.tuples_excluded,
            _ => &mut progress.tuples_skipped,
        };
        *field = Some("9223372036854775808".into());
        assert!(!invalid.is_valid(), "{counter}");
    }
    let mut impossible = input.clone();
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut impossible.observations[0].native else { panic!() };
    progress.command = PostgresCopyCommand::From;
    assert!(!impossible.is_valid(), "COPY FROM does not copy a SELECT query");
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut impossible.observations[0].native else { panic!() };
    progress.relation_oid = "16420".into();
    assert!(impossible.is_valid());
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut impossible.observations[0].native else { panic!() };
    progress.command = PostgresCopyCommand::To;
    progress.bytes_total = Some("1".into());
    assert!(!impossible.is_valid(), "COPY TO has no source-file size");
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut impossible.observations[0].native else { panic!() };
    progress.bytes_total = None;
    assert!(impossible.is_valid());
}
