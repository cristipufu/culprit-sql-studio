use super::*;

fn source() -> CollectionDescription {
    let owner = SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine").qualified("kind", "sql-server-network-machine"),
        native: NativeIdentity::new("native-instance").qualified("kind", "sql-server-instance"),
    };
    let mut source = snapshot_source();
    source.target = CollectionTarget::Shared(owner.clone());
    source.partition = SourcePartition::InstanceLocal { member: Some(owner), epoch: Some(digest(b"native-process")) };
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = 1;
    header.sources = vec![StateSourceCoverage::collected(crate::source_id::LOG_CAPACITY.into(), 1)];
    source
}

fn row() -> OperationalObservation {
    let mut row = rows().remove(0);
    row.source = crate::source_id::LOG_CAPACITY.into();
    row.key = "shared-tempdb-log".into();
    let OperationalFact::Space(space) = &mut row.fact else { unreachable!() };
    space.population = StoragePopulation::SharedTempdbTransactionLog;
    space.kind = FileKind::Log;
    space.scope = FileScope::Server;
    space.database = None;
    space.native_name = Some("tempdb".into());
    space.parts.clear();
    row
}

#[test]
fn original_shared_owner_and_exact_log_values_survive_export_and_offline_storage_replay() {
    let source = source();
    let capture = CaptureIdentity { observation_id: "shared-tempdb-log".into(), revision: None };
    for name in [Some("tempdb".into()), None] {
        let mut row = row();
        row_space_mut(&mut row).native_name = name.clone();
        let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row.clone())).unwrap();
        let read = StorageRead::from_source(&source, vec![row], true).unwrap();
        let encoded = serde_json::to_string(&exported).unwrap();
        assert!(encoded.contains("shared-tempdb-transaction-log"));
        let restored: StorageRead = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
        assert_eq!(restored, read);
        let OperationalFact::Space(space) = &restored.observations[0].fact else { unreachable!() };
        assert_eq!(space.total_bytes.as_deref(), Some("9007199254740993"));
        assert_eq!(space.used_bytes.as_deref(), Some("0"));
        assert_eq!(space.database, None);
        assert_eq!(space.native_name, name);
        assert!(space.parts.is_empty());
    }
}

#[test]
fn database_routes_unknown_epochs_and_other_shared_kinds_never_acquire_tempdb_log_authority() {
    for invalid in
        ["database", "target-partition", "missing-member", "missing-epoch", "other-member", "pool", "node", "engine"]
    {
        let mut source = source();
        match invalid {
            "database" => source = snapshot_source(),
            "target-partition" => source.partition = SourcePartition::Target,
            "missing-member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut source.partition else { unreachable!() };
                *member = None;
            }
            "missing-epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { unreachable!() };
                *epoch = None;
            }
            "other-member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.native.id = "different-owner".into();
            }
            change => {
                let CollectionTarget::Shared(owner) = &mut source.target else { unreachable!() };
                match change {
                    "pool" => owner.kind = SharedResourceKind::Pool,
                    "node" => owner.kind = SharedResourceKind::Node,
                    _ => owner.engine = crate::Engine::Postgres,
                }
                let SourcePartition::InstanceLocal { member, .. } = &mut source.partition else { unreachable!() };
                *member = Some(owner.clone());
            }
        }
        assert!(!row_space(&row()).fits_source(&source), "{invalid}");
        assert!(StorageRead::from_source(&source, vec![row()], true).is_err(), "{invalid}");
        let capture = CaptureIdentity { observation_id: "shared-tempdb-log-owner-refusal".into(), revision: None };
        assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row())).is_err(), "{invalid}");
    }
}

#[test]
fn raw_shared_log_refuses_other_families_even_with_coherently_changed_original_coverage() {
    let capture = CaptureIdentity { observation_id: "shared-tempdb-log-source-refusal".into(), revision: None };
    for family in [crate::source_id::TEMP_CAPACITY, crate::source_id::FILE_CAPACITY] {
        let mut source = source();
        let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
        header.sources = vec![StateSourceCoverage::collected(family.into(), 1)];
        let mut row = row();
        row.source = family.into();
        assert!(source.partition_is_consistent());
        assert!(!row.is_valid());
        assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row.clone())).is_err());
        assert!(StorageRead::from_source(&source, vec![row], true).is_err());
    }
}

#[test]
fn shared_log_cannot_borrow_a_data_population_application_database_or_collector_clock() {
    for invalid in ["kind", "scope", "database", "parts", "source", "collector"] {
        let mut row = row();
        let space = row_space_mut(&mut row);
        match invalid {
            "kind" => space.kind = FileKind::Temp,
            "scope" => space.scope = FileScope::Database,
            "database" => space.database = Some("sample".into()),
            "parts" => {
                space.parts.push(SpaceAllocation { kind: SpaceAllocationKind::UserObjects, bytes: Some("1".into()) })
            }
            _ => {}
        }
        if invalid == "source" {
            row.source = crate::source_id::TEMP_CAPACITY.into();
        }
        let read = StorageRead::from_source(&source(), vec![row.clone()], true);
        if invalid == "collector" {
            let mut changed = read.unwrap();
            changed.context = super::read().context;
            assert!(!changed.is_valid());
        } else {
            assert!(read.is_err(), "{invalid}");
        }
    }
}

#[test]
fn log_point_must_fit_the_original_native_read_and_unknown_is_not_a_measured_zero() {
    let source = source();
    for at in ["2020-01-01T11:59:59.999Z", "2020-01-01T12:00:01.001Z"] {
        let mut row = row();
        row_space_mut(&mut row).observed_at = at.into();
        assert!(StorageRead::from_source(&source, vec![row], true).is_err());
    }
    for used in [None, Some("0".into())] {
        let mut row = row();
        row_space_mut(&mut row).used_bytes = used.clone();
        let read = StorageRead::from_source(&source, vec![row], true).unwrap();
        assert_eq!(row_space(&read.observations[0]).used_bytes, used);
    }
}

fn row_space(row: &OperationalObservation) -> &OperationalSpace {
    let OperationalFact::Space(space) = &row.fact else { unreachable!() };
    space
}

fn row_space_mut(row: &mut OperationalObservation) -> &mut OperationalSpace {
    let OperationalFact::Space(space) = &mut row.fact else { unreachable!() };
    space
}
