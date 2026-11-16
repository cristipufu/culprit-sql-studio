//! Real native field layouts with controlled exact integer boundary values; no live database or inferred owner.
use super::*;

const ORIGINAL_AT: &str = "2020-01-01T12:00:00Z";
const POINT_AT: &str = "2020-01-01T12:00:01Z";

pub(super) fn evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    vec![
        evidence("vacuum-11", MaintenanceCommand::Vacuum, vacuum(110_000), None, "scanning heap", false),
        evidence("vacuum-17", MaintenanceCommand::Vacuum, vacuum(170_000), None, "vacuuming indexes", false),
        evidence("vacuum-18", MaintenanceCommand::Vacuum, vacuum(180_000), None, "cleaning up indexes", false),
        evidence(
            "analyze-13",
            MaintenanceCommand::Analyze,
            analyze(130_000),
            None,
            "acquiring inherited sample rows",
            false,
        ),
        evidence(
            "analyze-18",
            MaintenanceCommand::Analyze,
            analyze(180_000),
            None,
            "computing extended statistics",
            false,
        ),
        evidence("cluster-12", MaintenanceCommand::Cluster, cluster(), Some("16422"), "rebuilding index", false),
        evidence("vacuum-full-17", MaintenanceCommand::VacuumFull, cluster(), Some("0"), "writing new heap", false),
        evidence(
            "partial-vacuum-18",
            MaintenanceCommand::Vacuum,
            vacuum(180_000),
            None,
            "performing final cleanup",
            true,
        ),
    ]
}

fn vacuum(version: u32) -> MaintenanceProgressWork {
    MaintenanceProgressWork::PostgreSqlVacuum {
        progress: Box::new(PostgresVacuumProgress {
            heap_blks_total: Some("1000".into()),
            heap_blks_scanned: Some("0".into()),
            heap_blks_vacuumed: None,
            index_vacuum_count: Some("0".into()),
            max_dead_tuples: (version < 170_000).then(|| "291".into()),
            num_dead_tuples: (version < 170_000).then(|| "0".into()),
            indexes_total: (version >= 170_000).then(|| "2".into()),
            indexes_processed: (version >= 170_000).then(|| "0".into()),
            max_dead_tuple_bytes: (version >= 170_000).then(|| "67108864".into()),
            dead_tuple_bytes: (version >= 170_000).then(|| "0".into()),
            num_dead_item_ids: (version >= 170_000).then(|| "0".into()),
            cost_delay_ms: (version >= 180_000).then(|| "0".into()),
        }),
    }
}

fn analyze(version: u32) -> MaintenanceProgressWork {
    MaintenanceProgressWork::PostgreSqlAnalyze {
        progress: Box::new(PostgresAnalyzeProgress {
            sample_blks_total: Some("1000".into()),
            sample_blks_scanned: Some("0".into()),
            ext_stats_total: Some("2".into()),
            ext_stats_computed: None,
            child_tables_total: Some("4".into()),
            child_tables_done: Some("1".into()),
            current_child_table_relid: Some("0".into()),
            cost_delay_ms: (version >= 180_000).then(|| "0.125".into()),
        }),
    }
}

fn cluster() -> MaintenanceProgressWork {
    MaintenanceProgressWork::PostgreSqlCluster {
        progress: Box::new(PostgresClusterProgress {
            // Exact int8-domain stress remains raw counter text, never a size, rate or completion proof.
            heap_tuples_scanned: Some("9007199254740993".into()),
            heap_tuples_written: Some("0".into()),
            heap_blks_total: None,
            heap_blks_scanned: None,
            index_rebuild_count: Some("1".into()),
        }),
    }
}

fn evidence(
    name: &str,
    command: MaintenanceCommand,
    work: MaintenanceProgressWork,
    index_oid: Option<&str>,
    phase: &str,
    partial: bool,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let original = original_source(Engine::Postgres);
    let focus = original_focus(&original, name, command);
    let native_source = match command {
        MaintenanceCommand::Vacuum => "pg_stat_progress_vacuum",
        MaintenanceCommand::Analyze => "pg_stat_progress_analyze",
        MaintenanceCommand::Cluster | MaintenanceCommand::VacuumFull => "pg_stat_progress_cluster",
        _ => unreachable!("the corpus explicitly lists non-index commands"),
    };
    let point = MaintenanceProgressObservation {
        observed_at: POINT_AT.into(),
        actor: focus.original.key.clone(),
        statement: Some(focus.statement_identity.clone()),
        database_oid: "16384".into(),
        relation_oid: "16420".into(),
        index_oid: index_oid.map(str::to_owned),
        command,
        phase: phase.into(),
        work,
    };
    let mut coverage = StateSourceCoverage::observed(native_source.into(), 1, false);
    if partial {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let source = CollectionDescription {
        target: original.target.clone(),
        partition: original.partition.clone(),
        source: native_source.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MaintenanceProgress {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::MaintenanceProgress,
                observed_from: POINT_AT.into(),
                observed_until: POINT_AT.into(),
                records: 1,
                sources: vec![coverage],
            },
            result: Box::new(FocusedRequestResult {
                focus: Some(Box::new(focus.clone())),
                database_writable: Some(true),
                binding: FocusedRequestBinding::Matched,
                membership: FocusedRequestMembership::Observed,
            }),
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::MaintenanceProgress { focus: Box::new(focus) },
        budget: CollectionBudget::default(),
    }));
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: native_source.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MaintenanceProgress(Box::new(point)),
    });
    ExportedRecord::new(&source, &fixture().0.capture, record.clone()).unwrap();
    (source, vec![record])
}

fn original_focus(source: &CollectionDescription, name: &str, command: MaintenanceCommand) -> ActivityRequestFocus {
    let mut actor = activity_observation(DefinitionPayload::sql(
        &format!("{} private_maintenance_table", command.as_native()),
        Engine::Postgres,
    ));
    actor.key = ActivityKey {
        session: NativeActivityIdentity { id: "61".into(), generation: "2020-01-01T11:00:00Z".into() },
        request: Some(NativeActivityIdentity { id: "statement".into(), generation: "2020-01-01T11:59:59Z".into() }),
        transaction: None,
    };
    actor.scope = ActivityScopeBasis::SessionDatabase;
    actor.command = None;
    actor.state = "active".into();
    actor.wait = None;
    actor.session_counters = ActivityCounters::default();
    actor.request_counters = Some(ActivityCounters::default());
    actor.elapsed_ms = Some("1000".into());
    actor.statement_identity = Some(NativeIdentity::new(format!("native-maintenance-{name}")));
    assert!(actor.is_valid(), "the native request retains an explicit unknown-counter container");
    let recovery = Recovery::capture(
        "fixture:non-index-maintenance-parent",
        &digest(b"maintenance-native-schema"),
        source.clone(),
        vec![TelemetryRecord::from(actor.clone())],
        831_001,
    );
    let exported = ExportedRecord::new(source, &recovery.manifest.capture, TelemetryRecord::from(actor)).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(source, &recovery.manifest.capture_manifest_sha256, &exported, 0).unwrap();
    let TelemetryRecord::Activity(actor) = exported.record else { unreachable!() };
    let focus = ActivityRequestFocus {
        parent,
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: Some(true),
        source: source.source.clone(),
        epoch: source.partition.native_epoch().unwrap().into(),
        original_observed_at: ORIGINAL_AT.into(),
        statement_identity: actor.statement_identity.clone().unwrap(),
        original: *actor,
    };
    assert!(focus.matches_parent(source, &focus.original, &focus.parent));
    focus
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_scope_header_and_command_work_contradictions_cannot_be_exported_as_valid_points() {
        let (source, rows) = evidence_cases().remove(0);
        for change in ["member", "epoch", "role", "database", "header", "command", "statement"] {
            let mut source = source.clone();
            let mut record = rows[0].clone();
            match change {
                "member" => {
                    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                        panic!()
                    };
                    member.native.id = "another-native-member".into();
                }
                "epoch" => {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                    *epoch = Some("another-native-process".into());
                }
                "role" => {
                    let SourceObservation::MaintenanceProgress { result, .. } = &mut source.observation else {
                        panic!()
                    };
                    result.database_writable = Some(false);
                }
                "database" => {
                    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
                    database.database = "another_database".into();
                }
                "header" => {
                    let SourceObservation::MaintenanceProgress { snapshot, .. } = &mut source.observation else {
                        panic!()
                    };
                    snapshot.records = 0;
                }
                _ => {
                    let TelemetryRecord::Operational(row) = &mut record else { panic!() };
                    let OperationalFact::MaintenanceProgress(point) = &mut row.fact else { panic!() };
                    if change == "command" {
                        point.command = MaintenanceCommand::Analyze;
                    } else {
                        point.statement = Some(NativeIdentity::new("another-native-statement"));
                    }
                }
            }
            assert!(ExportedRecord::new(&source, &fixture().0.capture, record).is_err(), "{change}");
        }
    }
}
