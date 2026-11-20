use super::*;
use crate::analytical::schema::LogicalSchema;
use crate::analytical::test_support::fixture;
use culprit_core::telemetry::{
    CollectionTarget, NativeIdentity, SharedResourceIdentity, SharedResourceKind, TelemetryRecord,
};

#[test]
fn shared_capture_context_keeps_resource_identity_and_kind_without_exporting_private_namespace() {
    for (kind, expected) in [
        (SharedResourceKind::Instance, "instance"),
        (SharedResourceKind::Pool, "pool"),
        (SharedResourceKind::Cluster, "cluster"),
        (SharedResourceKind::Node, "node"),
    ] {
        let (mut manifest, _) = fixture();
        manifest.source.target = CollectionTarget::Shared(SharedResourceIdentity {
            engine: culprit_core::Engine::Postgres,
            kind,
            namespace: NativeIdentity::new("private-namespace-canary"),
            native: NativeIdentity::new("private-resource-canary"),
        });
        let context = capture_context(&manifest, &"a".repeat(64)).unwrap();
        assert_eq!(context["resource_id"], manifest.source.target.telemetry_id().unwrap());
        assert_eq!(context["resource_kind"], expected);
        assert_eq!(context["engine"], "postgres");
        assert!(context.get("database_id").is_none());
        let wire = serde_json::to_string(&context).unwrap();
        assert!(!wire.contains("private-namespace-canary"));
        assert!(!wire.contains("private-resource-canary"));
    }
}

#[test]
fn envelope_keeps_collection_measurement_and_native_observation_times_distinct() {
    let (manifest, row) = fixture();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    let input = envelope.input(1, &row).unwrap();
    let schema = LogicalSchema::new("FixtureV1", COMMON.iter().copied()).unwrap();
    let projected = schema.project(&input).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["RangeId"], manifest.range_id);
    assert_eq!(wire["RecordId"], row.record_id.as_ref().unwrap().as_str());
    assert_eq!(wire["CollectedFrom"], "2020-01-01T12:00:00.000000000Z");
    assert_eq!(wire["IntervalFrom"], "2020-01-01T06:00:00.000000000Z");
    assert_eq!(wire["ObservedAt"], "2020-01-01T06:02:00.000000000Z");
    assert!(wire["ObservedUntil"].is_null());
    assert!(wire["IntervalId"].is_null());
    assert!(wire.get("HasIntervalId").is_none());
    assert_eq!(wire["OwnerEpoch"], 1);
    assert_eq!(wire["CaptureSequence"], 2);
    assert_eq!(input["record"]["data"]["at"], "2020-01-01T06:00:15.000000000Z");
}
#[test]
fn unexpected_rows_wrong_ordinals_and_native_payload_changes_are_rejected() {
    let (manifest, row) = fixture();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    assert!(envelope.input(0, &row).is_err());
    assert!(envelope.input(2, &row).is_err());
    let mut changed = row.clone();
    changed.capture.revision.as_mut().unwrap().outbox_id = "other".into();
    assert!(envelope.input(1, &changed).is_err());
    let mut changed = row.clone();
    let TelemetryRecord::ResourceSample(sample) = &mut changed.record else { unreachable!() };
    sample.metrics[0].value = Some(99.5);
    assert!(envelope.input(1, &changed).is_err());
    assert!(Envelope::new(&manifest, &"a".repeat(64)).is_err());
}

#[test]
fn a_valid_receipt_cannot_bind_a_foreign_database_or_a_forged_native_record_id() {
    let (mut manifest, mut row) = fixture();
    let CollectionTarget::Database(database) = &mut manifest.source.target else { unreachable!() };
    database.database = "a-different-database".into();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    assert!(envelope.input(1, &row).is_err(), "the receipt alone cannot supply native source identity");

    let (mut manifest, _) = fixture();
    row.record_id = Some("a".repeat(64));
    manifest.batches[0].rows[0] = crate::delivery::evidence::RowReceipt::from_export(&row, 1).unwrap();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    assert!(envelope.input(1, &row).is_err(), "rebuilding receipts cannot authorize a non-native record id");
}

#[test]
fn suspect_page_envelope_uses_original_current_clock_scope_without_private_history_or_native_proof() {
    use crate::analytical::test_support::{self, Recovery, SuspectPageHistoryCase};
    let (template, _) = fixture();
    for case in SuspectPageHistoryCase::ALL {
        let (source, rows) = test_support::suspect_page_history_evidence(case);
        let recovery = Recovery::capture(
            template.contract.destination(),
            template.contract.schema_digest(),
            source.clone(),
            rows.clone(),
            2,
        );
        let context = capture_context(&recovery.manifest, &"a".repeat(64)).unwrap();
        let snapshot = source.observation.snapshot().unwrap();
        assert_eq!(
            context["dataset"],
            serde_json::to_value(culprit_core::telemetry::SnapshotDataset::SuspectPages).unwrap()
        );
        assert_eq!(context["range_kind"], "current");
        assert_eq!(context["observed_from"], snapshot.observed_from);
        assert_eq!(context["observed_until"], snapshot.observed_until);
        assert_eq!(context["source_id"], source.captured_range().unwrap().scope_id);
        for absent in ["native_database", "database_guid", "recovery_fork_guid", "last_reported_time_without_offset"] {
            assert!(context.get(absent).is_none(), "{absent}");
        }
        if let Some(record) = rows.first() {
            let original = ExportedRecord::new(&source, &recovery.manifest.capture, record.clone()).unwrap();
            let envelope = Envelope::new(&recovery.manifest, &original.batch_id).unwrap();
            assert!(envelope.input(1, &original).is_ok());
        }
    }
}
#[test]
fn a_changed_original_history_header_cannot_reuse_the_same_opaque_receipt_or_current_database_label() {
    use crate::analytical::test_support::{self, Recovery, SuspectPageHistoryCase};
    use culprit_core::telemetry::{SourceObservation, SourcePartition};
    let (template, _) = fixture();
    let (source, rows) = test_support::suspect_page_history_evidence(SuspectPageHistoryCase::Active);
    let recovery = Recovery::capture(
        template.contract.destination(),
        template.contract.schema_digest(),
        source.clone(),
        rows.clone(),
        2,
    );
    let original = ExportedRecord::new(&source, &recovery.manifest.capture, rows[0].clone()).unwrap();
    for cause in ["proof", "id", "role", "generic"] {
        let mut manifest = recovery.manifest.clone();
        if cause == "generic" {
            manifest.source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
        } else {
            let SourceObservation::SuspectPages { native_database, database_writable, .. } =
                &mut manifest.source.observation
            else {
                panic!()
            };
            match cause {
                "proof" => {
                    native_database.qualifiers.insert("database_guid".into(), "foreign-current-guid".into());
                }
                "id" => native_database.id = "8".into(),
                _ => *database_writable = false,
            }
            let SourcePartition::InstanceLocal { epoch, .. } = &mut manifest.source.partition else { panic!() };
            *epoch = Some(native_database.key().unwrap());
        }
        match Envelope::new(&manifest, &original.batch_id) {
            Err(_) => {}
            Ok(envelope) => assert!(envelope.input(1, &original).is_err(), "{cause}"),
        }
    }
}
