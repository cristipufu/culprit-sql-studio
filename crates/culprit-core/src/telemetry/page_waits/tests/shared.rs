use super::*;

fn shared_observation() -> PageWaitObservation {
    let mut row = observation();
    row.resource.native.id = "2:1:42".into();
    row.request.wait.as_mut().unwrap().identified_resources = vec![row.resource.clone()];
    row.observed_address.as_mut().unwrap().database_id = 2;
    let resource = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    let scope = PageTargetScope::SharedResource {
        context: Box::new(NativeReadContext {
            target: CollectionTarget::Shared(resource.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(resource),
                epoch: Some("native-process-epoch".into()),
            },
            observed_from: "2026-11-13T10:00:00.100000Z".into(),
            observed_until: "2026-11-13T10:00:00.800000Z".into(),
            database_writable: None,
        }),
    };
    let target = row.target.as_mut().unwrap();
    target.header.database_id = 2;
    target.header.page_type = Some(11);
    target.header.object_id = None;
    target.header.index_id = None;
    target.header.partition_id = None;
    target.kind = PageWaitTargetKind::Pfs;
    target.object = None;
    let (database, file) = scope
        .bind_identities(
            NativeIdentity::new("2").qualified("native-tempdb-epoch", "native-tempdb-guid"),
            NativeIdentity::new("native-tempdb-file").qualified("create_lsn", "1"),
        )
        .unwrap();
    target.scope = scope;
    target.database = database;
    target.file = file;
    row
}

fn primary_file_observation(kind: PageWaitTargetKind) -> PageWaitObservation {
    let (page_id, page_type) = match kind {
        PageWaitTargetKind::Pfs => (1, 11),
        PageWaitTargetKind::Gam => (2, 8),
        PageWaitTargetKind::Sgam => (3, 9),
        _ => panic!("a primary-file witness is limited to intrinsic allocation maps"),
    };
    let mut row = shared_observation();
    row.resource.native.id = format!("2:1:{page_id}");
    row.request.wait.as_mut().unwrap().identified_resources = vec![row.resource.clone()];
    row.observed_address.as_mut().unwrap().page_id = page_id;
    let target = row.target.as_mut().unwrap();
    target.identity_basis = PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch;
    target.header.page_id = page_id;
    target.header.page_type = Some(page_type);
    target.header.header_version = None;
    target.header.allocation_unit_id = None;
    target.kind = kind;
    target.database = NativeIdentity::new("2")
        .qualified("kind", MSSQL_SHARED_TEMPDB_DATABASE_KIND)
        .qualified("instance_epoch", "native-process-epoch")
        .qualified("database_created_without_offset", "2026-11-13T09:00:00.0000000")
        .qualified("database_guid", "native-tempdb-guid")
        .qualified("recovery_fork_guid", "native-tempdb-fork");
    target.file = NativeIdentity::new("1")
        .qualified("kind", MSSQL_TEMPDB_PRIMARY_FILE_KIND)
        .qualified("file_type", "0")
        .qualified("file_state", "0");
    bind_target(target);
    row
}

fn bind_target(target: &mut MappedPageTarget) {
    let (database, file) = target.scope.bind_identities(target.database.clone(), target.file.clone()).unwrap();
    target.database = database;
    target.file = file;
}

fn source(row: &PageWaitObservation) -> CollectionDescription {
    let PageTargetScope::SharedResource { context } = &row.target.as_ref().unwrap().scope else { panic!() };
    CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitTargets {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(SOURCE.into(), 1, false)],
            },
            database_writable: None,
            original_activity: None,
        },
    }
}

#[test]
fn shared_target_authority_precedes_the_wait_without_moving_the_original_request_or_inventing_ownership() {
    let row = shared_observation();
    let original = observation().request;
    assert_eq!(row.request.key, original.key);
    assert_eq!(row.request.database, original.database);
    assert_eq!(row.request.wait.as_ref().unwrap().owners, original.wait.unwrap().owners);
    assert!(row.is_valid());
    assert!(row.fits_source_scope(&source(&row)));
    let mut snapshot = snapshot();
    snapshot.observations = vec![row.clone()];
    assert!(snapshot.is_valid());
    assert!(row.target.as_ref().unwrap().is_valid());
    assert_eq!(serde_json::from_str::<PageWaitObservation>(&serde_json::to_string(&row).unwrap()).unwrap(), row);
}

#[test]
fn export_binds_a_valid_shared_page_to_its_own_authenticated_owner_and_native_epoch() {
    let row = shared_observation();
    let capture = CaptureIdentity { observation_id: "shared-page-export".into(), revision: None };
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: SOURCE.into(),
        key: row.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(row.clone())),
    });
    assert!(ExportedRecord::new(&source(&row), &capture, record.clone()).is_ok());
    for changed in ["owner", "epoch", "bounds"] {
        let mut description = source(&row);
        match changed {
            "owner" => {
                let CollectionTarget::Shared(resource) = &mut description.target else { panic!() };
                resource.native.id = "another-native-resource".into();
                let SourcePartition::InstanceLocal { member, .. } = &mut description.partition else { panic!() };
                *member = Some(resource.clone());
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut description.partition else { panic!() };
                *epoch = Some("another-native-epoch".into());
            }
            "bounds" => {
                let SourceObservation::PageWaitTargets { snapshot, .. } = &mut description.observation else {
                    panic!()
                };
                snapshot.observed_from = row.observed_from.clone();
            }
            _ => unreachable!(),
        }
        assert!(row.is_valid(), "the original point remains valid independently of {changed}");
        assert!(ExportedRecord::new(&description, &capture, record.clone()).is_err(), "{changed}");
    }
}

#[test]
fn shared_identity_bindings_reject_changed_member_epoch_database_or_file_without_claiming_a_native_generation() {
    for change in ["member", "epoch", "database", "file-database", "unknown-member", "wrong-target", "app-scope"] {
        let mut row = shared_observation();
        let target = row.target.as_mut().unwrap();
        let PageTargetScope::SharedResource { context } = &mut target.scope else { panic!() };
        match change {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else {
                    panic!()
                };
                member.native.id = "another-member".into();
                context.target = CollectionTarget::Shared(member.clone());
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
                *epoch = Some("another-native-process".into());
            }
            "database" => {
                target.database.qualifiers.insert("native-tempdb-epoch".into(), "recreated-tempdb".into());
            }
            "file-database" => {
                target.file.qualifiers.insert("page_target_database".into(), "wrong-database".into());
            }
            "unknown-member" => {
                context.partition =
                    SourcePartition::InstanceLocal { member: None, epoch: Some("native-process-epoch".into()) }
            }
            "wrong-target" => {
                let CollectionTarget::Shared(resource) = &mut context.target else { panic!() };
                resource.native.id = "other-resource".into();
            }
            "app-scope" => target.scope = PageTargetScope::RequestDatabase,
            _ => unreachable!(),
        }
        assert!(!row.is_valid(), "{change}");
    }
}

#[test]
fn shared_scope_and_original_native_bounds_must_match_the_actual_capture_not_an_application_label() {
    let row = shared_observation();
    let mut description = source(&row);
    if let SourcePartition::InstanceLocal { epoch, .. } = &mut description.partition {
        *epoch = Some("another-process".into());
    }
    assert!(!row.fits_source_scope(&description));
    let mut description = source(&row);
    let SourceObservation::PageWaitTargets { snapshot: frame, .. } = &mut description.observation else { panic!() };
    frame.observed_from = row.observed_from.clone();
    assert!(
        !row.fits_source_scope(&description),
        "earlier native authority is not inside the later wait-only interval"
    );
    let mut snapshot = snapshot();
    snapshot.observations = vec![row.clone()];
    snapshot.observed_from = row.observed_from.clone();
    assert!(!snapshot.is_valid());
    let mut changed = row;
    let PageTargetScope::SharedResource { context } = &mut changed.target.as_mut().unwrap().scope else { panic!() };
    context.observed_until = "2026-11-13T10:00:00.600000Z".into();
    assert!(!changed.is_valid(), "the later request check must be bracketed by shared-source proof");
}

#[test]
fn sharing_a_shared_page_snapshot_redacts_resource_authority_and_rebinds_identities_idempotently() {
    let mut original: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut pages = snapshot();
    pages.observations = vec![shared_observation()];
    original.operations.page_waits = Some(pages.clone());
    let shared = crate::redact::redact_snapshot(&original);
    let public = shared.operations.page_waits.as_ref().unwrap();
    assert!(public.is_valid());
    assert_eq!(
        original.operations.page_waits.as_ref().unwrap(),
        &pages,
        "sharing never mutates native archive evidence"
    );
    let original_target = pages.observations[0].target.as_ref().unwrap();
    let shared_target = public.observations[0].target.as_ref().unwrap();
    let PageTargetScope::SharedResource { context: before } = &original_target.scope else { panic!() };
    let PageTargetScope::SharedResource { context: after } = &shared_target.scope else { panic!() };
    assert_eq!(before.observed_from, after.observed_from);
    assert_eq!(before.observed_until, after.observed_until);
    assert_eq!(before.database_writable, after.database_writable);
    assert_ne!(before.target, after.target);
    assert_ne!(before.partition, after.partition);
    assert!(shared_target.is_valid());
    let encoded = serde_json::to_string(public).unwrap();
    for canary in
        ["native-machine", "native-instance", "native-process-epoch", "native-tempdb-guid", "native-tempdb-file"]
    {
        assert!(!encoded.contains(canary), "{canary}");
    }
    let twice = crate::redact::redact_snapshot(&shared);
    assert_eq!(twice.operations.page_waits.as_ref(), Some(public));
}

#[test]
fn a_tempdb_primary_epoch_maps_only_native_intrinsic_pages_without_a_file_guid_or_header_version() {
    for kind in [PageWaitTargetKind::Pfs, PageWaitTargetKind::Gam, PageWaitTargetKind::Sgam] {
        for header_version in [None, Some(1)] {
            let mut row = primary_file_observation(kind);
            row.target.as_mut().unwrap().header.header_version = header_version;
            let target = row.target.as_ref().unwrap();
            assert!(target.is_valid());
            assert!(row.is_valid());
            assert!(row.fits_source_scope(&source(&row)));
            assert_eq!(target.file.id, "1");
            assert!(!target.file.qualifiers.contains_key("file_guid"));
            assert!(!target.file.qualifiers.contains_key("create_lsn"));
            assert!(target.object.is_none());
            assert!(
                row.request
                    .wait
                    .as_ref()
                    .unwrap()
                    .owners
                    .iter()
                    .all(|owner| { matches!(owner, ActivityWaitOwner::Unknown { .. }) })
            );
            let encoded = serde_json::to_string(&row).unwrap();
            assert!(encoded.contains("local-tempdb-primary-file-epoch"));
            assert_eq!(serde_json::from_str::<PageWaitObservation>(&encoded).unwrap(), row);
            let record = TelemetryRecord::Operational(OperationalObservation {
                source: SOURCE.into(),
                key: row.key().unwrap(),
                fact: OperationalFact::PageWait(Box::new(row.clone())),
            });
            let capture = CaptureIdentity { observation_id: "tempdb-primary-epoch".into(), revision: None };
            assert!(ExportedRecord::new(&source(&row), &capture, record).is_ok());
        }
    }
    assert!(shared_observation().is_valid(), "the original GUID-based contract remains unchanged");
}

#[test]
fn primary_file_identity_requires_each_native_qualifier_even_after_content_rebinding() {
    for key in ["kind", "instance_epoch", "database_created_without_offset", "database_guid", "recovery_fork_guid"] {
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        let target = row.target.as_mut().unwrap();
        target.database.qualifiers.remove(key);
        bind_target(target);
        assert!(!target.is_valid(), "missing database {key}");
    }
    for key in ["kind", "file_type", "file_state"] {
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        let target = row.target.as_mut().unwrap();
        target.file.qualifiers.remove(key);
        bind_target(target);
        assert!(!target.is_valid(), "missing file {key}");
    }
    for change in [
        "database",
        "file",
        "database-kind",
        "file-kind",
        "epoch",
        "created",
        "file-type",
        "file-state",
        "file_guid",
        "create_lsn",
        "drop_lsn",
        "header-database",
        "header-file",
        "header-kind",
        "unclassified",
        "object",
        "native-object",
        "version",
    ] {
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        let target = row.target.as_mut().unwrap();
        match change {
            "database" => target.database.id = "5".into(),
            "file" => target.file.id = "2".into(),
            "database-kind" => {
                target.database.qualifiers.insert("kind".into(), "ordinary-database".into());
            }
            "file-kind" => {
                target.file.qualifiers.insert("kind".into(), "sql-server-database-file".into());
            }
            "epoch" => {
                target.database.qualifiers.insert("instance_epoch".into(), "another-epoch".into());
            }
            "created" => {
                target
                    .database
                    .qualifiers
                    .insert("database_created_without_offset".into(), "2026-11-13T09:00:00Z".into());
            }
            "file-type" => {
                target.file.qualifiers.insert("file_type".into(), "1".into());
            }
            "file-state" => {
                target.file.qualifiers.insert("file_state".into(), "6".into());
            }
            "file_guid" | "create_lsn" | "drop_lsn" => {
                target.file.qualifiers.insert(change.into(), "native-file-epoch-value".into());
            }
            "header-database" => target.header.database_id = 5,
            "header-file" => target.header.file_id = 2,
            "header-kind" => {
                target.kind = PageWaitTargetKind::Iam;
                target.header.page_type = Some(10);
            }
            "unclassified" => target.kind = PageWaitTargetKind::Unclassified,
            "object" => target.object = observation().target.unwrap().object,
            "native-object" => target.header.object_id = Some("128".into()),
            _ => target.header.header_version = Some(2),
        }
        bind_target(target);
        assert!(!target.is_valid(), "{change}");
    }
}

#[test]
fn primary_file_witness_cannot_swap_basis_owner_scope_or_native_clock() {
    for change in [
        "database-basis",
        "file-basis",
        "app-scope",
        "engine",
        "node",
        "epoch",
        "missing-epoch",
        "clock",
        "clock-regression",
    ] {
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        let target = row.target.as_mut().unwrap();
        match change {
            "database-basis" => target.identity_basis = PageTargetIdentityBasis::DatabaseIncarnation,
            "file-basis" => target.identity_basis = PageTargetIdentityBasis::LocalDatabaseFileEpoch,
            "app-scope" => {
                target.scope = PageTargetScope::RequestDatabase;
                target.database.qualifiers.remove("page_target_scope");
                target.file.qualifiers.remove("page_target_scope");
                target.file.qualifiers.remove("page_target_database");
            }
            _ => {
                let PageTargetScope::SharedResource { context } = &mut target.scope else { panic!() };
                match change {
                    "engine" | "node" => {
                        let CollectionTarget::Shared(resource) = &mut context.target else { panic!() };
                        if change == "engine" {
                            resource.engine = Engine::Postgres;
                        } else {
                            resource.kind = SharedResourceKind::Node;
                        }
                        let SourcePartition::InstanceLocal { member, .. } = &mut context.partition else { panic!() };
                        *member = Some(resource.clone());
                    }
                    "epoch" => {
                        let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
                        *epoch = Some("replacement-process".into());
                    }
                    "missing-epoch" => {
                        let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
                        *epoch = None;
                    }
                    "clock" => context.observed_from = "2026-11-13T10:00:00".into(),
                    _ => context.observed_until = "2026-11-13T09:00:00Z".into(),
                }
            }
        }
        if target.scope.is_valid() {
            bind_target(target);
        }
        assert!(!target.is_valid(), "{change}");
    }
    let mut swapped = shared_observation();
    swapped.target.as_mut().unwrap().identity_basis = PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch;
    assert!(!swapped.is_valid(), "a GUID-based identity cannot be relabelled as a primary-file witness");
}

#[test]
fn primary_file_database_epoch_qualifiers_cannot_be_substituted_without_rebinding_its_native_scope() {
    for key in ["database_guid", "recovery_fork_guid", "database_created_without_offset"] {
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        let target = row.target.as_mut().unwrap();
        target.database.qualifiers.insert(key.into(), "substituted-native-value".into());
        assert!(!target.is_valid(), "{key}");
    }
    let row = primary_file_observation(PageWaitTargetKind::Pfs);
    let mut changed = source(&row);
    let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
    *epoch = Some("replacement-process".into());
    assert!(!row.fits_source_scope(&changed));
}

#[test]
fn sharing_primary_file_witnesses_keeps_structural_facts_and_redacts_native_authority_idempotently() {
    let mut original: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut pages = snapshot();
    pages.observations = [PageWaitTargetKind::Pfs, PageWaitTargetKind::Gam, PageWaitTargetKind::Sgam]
        .into_iter()
        .map(primary_file_observation)
        .collect();
    original.operations.page_waits = Some(pages.clone());
    let shared = crate::redact::redact_snapshot(&original);
    let public = shared.operations.page_waits.as_ref().unwrap();
    assert!(public.is_valid());
    assert_eq!(original.operations.page_waits.as_ref(), Some(&pages));
    for row in &public.observations {
        let target = row.target.as_ref().unwrap();
        assert_eq!(target.identity_basis, PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch);
        assert_eq!(target.database.id, "2");
        assert_eq!(target.file.id, "1");
        assert_eq!(target.database.qualifiers["kind"], MSSQL_SHARED_TEMPDB_DATABASE_KIND);
        assert_eq!(target.file.qualifiers["kind"], MSSQL_TEMPDB_PRIMARY_FILE_KIND);
        assert_eq!(target.file.qualifiers["file_type"], "0");
        assert_eq!(target.file.qualifiers["file_state"], "0");
        assert_eq!(target.database.qualifiers["database_created_without_offset"], "2026-11-13T09:00:00.0000000");
        assert!(target.is_valid());
    }
    let encoded = serde_json::to_string(public).unwrap();
    for canary in
        ["native-machine", "native-instance", "native-process-epoch", "native-tempdb-guid", "native-tempdb-fork"]
    {
        assert!(!encoded.contains(canary), "{canary}");
    }
    assert_eq!(crate::redact::redact_snapshot(&shared).operations.page_waits.as_ref(), Some(public));
}

#[test]
fn sharing_cannot_launder_a_primary_file_witness_after_its_basis_was_swapped() {
    for basis in [PageTargetIdentityBasis::DatabaseIncarnation, PageTargetIdentityBasis::LocalDatabaseFileEpoch] {
        let mut original: crate::Snapshot =
            serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
                .unwrap();
        let mut row = primary_file_observation(PageWaitTargetKind::Pfs);
        row.target.as_mut().unwrap().identity_basis = basis;
        assert!(!row.target.as_ref().unwrap().is_valid());
        let mut pages = snapshot();
        pages.observations = vec![row];
        original.operations.page_waits = Some(pages);
        let shared = crate::redact::redact_snapshot(&original);
        let row = &shared.operations.page_waits.as_ref().unwrap().observations[0];
        assert!(row.target.is_none());
        assert_eq!(row.resolution, PageWaitResolution::UnverifiedSource);
        assert!(row.statement_identity.is_none());
        assert!(row.plan_identity.is_none());
        assert!(row.is_valid());
    }
}
