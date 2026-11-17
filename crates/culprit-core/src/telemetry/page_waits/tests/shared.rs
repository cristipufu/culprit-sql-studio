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
