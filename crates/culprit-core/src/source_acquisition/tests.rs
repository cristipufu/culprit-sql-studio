use super::*;
use crate::{Engine, SourceOverheadEvidence};
fn original() -> CapturedSourceAcquisition {
    let evidence = SourceOverheadEvidence {
        capture: CaptureIdentity {
            observation_id: "a".repeat(32),
            revision: Some(CaptureRevision {
                outbox_id: "b".repeat(32),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        },
        engine: None,
        source: None,
        status: CollectionStatus::Incomplete(CollectionStop::SourceUnavailable),
        started_at: "2026-11-17T12:00:00Z".into(),
        finished_at: Some("2026-11-17T12:00:05Z".into()),
        source_queries: Some(1),
        read_metrics: Some(ReadWorkMetrics {
            connection_work: None,
            native_session_work: None,
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"native template"),
                statement: DefinitionPayload::sql("SELECT N'private-sql-canary'", Engine::Mssql),
                completed_tasks: 1,
                failures: 1,
                timeouts: 0,
                cancellations: 0,
                unconfirmed_cancellations: 0,
                stop_causes: vec![],
                rows_decoded: "9007199254740993".into(),
                charged_value_bytes: "18446744073709551615".into(),
                client_elapsed_ms: 8.098523999999999,
            }],
        }),
    };
    CapturedSourceAcquisition::captured(
        digest(b"original exact manifest"),
        SourceAcquisitionSeal::Sealed,
        evidence.summary().unwrap(),
    )
    .unwrap()
}
#[test]
fn sealed_failed_route_keeps_exact_counts_and_unknown_identity_without_private_text_or_native_authority() {
    let original = original();
    assert!(original.is_saveable());
    assert!(original.summary.engine.is_none());
    assert_eq!(original.summary.source_queries, Some(1));
    assert!(matches!(original.summary.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable)));
    let value = serde_json::to_value(&original).unwrap();
    assert_eq!(value["summary"]["read_metrics"]["templates"][0]["rows_decoded"], "9007199254740993");
    assert_eq!(value["summary"]["read_metrics"]["templates"][0]["charged_value_bytes"], "18446744073709551615");
    assert_eq!(value["summary"]["read_metrics"]["templates"][0]["statement"]["reason"], "restricted");
    assert!(value["summary"].get("source").is_none());
    assert!(!value.to_string().contains("private-sql-canary"));
}
#[test]
fn adversarial_floating_elapsed_values_roundtrip_with_their_consistency_fingerprint_without_rounding() {
    for elapsed in [
        51.248178375505404,
        52.314008204106244,
        97.45365320034685,
        8.098523999999999,
        0.4998322631578947,
        222.05013899999997,
        f64::from_bits(0x3ff0_0000_0000_0001),
        f64::from_bits(0x405e_dd3c_3611_3405),
    ] {
        let mut summary = original().summary;
        summary.read_metrics.as_mut().unwrap().templates[0].client_elapsed_ms = elapsed;
        let original =
            CapturedSourceAcquisition::captured(digest(b"original"), SourceAcquisitionSeal::Sealed, summary).unwrap();
        let bytes = serde_json::to_vec(&original).unwrap();
        let decoded: CapturedSourceAcquisition = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded.summary.read_metrics.as_ref().unwrap().templates[0].client_elapsed_ms.to_bits(),
            elapsed.to_bits()
        );
        assert_eq!(decoded, original);
        assert!(decoded.is_valid());
    }
}
#[test]
fn altered_summary_preview_native_body_and_missing_nullable_slots_cannot_restore_a_valid_saved_original() {
    for mutation in ["clock", "count", "elapsed", "manifest", "sql", "native", "missing"] {
        let original = original();
        let mut value = serde_json::to_value(&original).unwrap();
        match mutation {
            "clock" => value["summary"]["finished_at"] = serde_json::json!("2026-11-17T12:00:06Z"),
            "count" => value["summary"]["source_queries"] = serde_json::json!(2),
            "elapsed" => value["summary"]["read_metrics"]["templates"][0]["client_elapsed_ms"] = serde_json::json!(8.0),
            "manifest" => value["manifest_sha256"] = serde_json::json!(digest(b"other original")),
            "sql" => {
                value["summary"]["read_metrics"]["templates"][0]["statement"] =
                    serde_json::to_value(DefinitionPayload::sql("SELECT N'foreign-sql-canary'", Engine::Mssql)).unwrap()
            }
            "native" => {
                value["summary"]["read_metrics"]["private_native_context"] = serde_json::json!({"host":"private"})
            }
            _ => {
                value["summary"].as_object_mut().unwrap().remove("engine");
            }
        }
        assert!(
            serde_json::from_value::<CapturedSourceAcquisition>(value).map_or(true, |changed| !changed.is_valid()),
            "{mutation}"
        );
    }
    let mut changed = original();
    changed.summary.read_metrics.as_mut().unwrap().templates[0].statement =
        DefinitionPayload::sql("SELECT 1", Engine::Mssql);
    assert!(
        CapturedSourceAcquisition::captured(changed.manifest_sha256, changed.seal, changed.summary).is_err(),
        "a recomputed consistency hash cannot make a forbidden preview curated"
    );
}
#[test]
fn provisional_metadata_is_inspectable_but_never_replaces_a_later_sealed_original() {
    let mut summary = original().summary;
    summary.status = CollectionStatus::Incomplete(CollectionStop::Pending);
    summary.finished_at = None;
    summary.source_queries = None;
    let original = Box::new(
        CapturedSourceAcquisition::captured(digest(b"provisional"), SourceAcquisitionSeal::Provisional, summary)
            .unwrap(),
    );
    assert!(original.is_valid());
    assert!(!original.is_saveable());
    let mut recorded = vec![];
    assert!(append_source_acquisition(&mut recorded, original).is_err());
    assert!(recorded.is_empty());
}
#[test]
fn exact_replays_are_idempotent_conflicting_same_revisions_refuse_and_distinct_captures_coexist() {
    let first = Box::new(original());
    let mut recorded = vec![];
    append_source_acquisition(&mut recorded, first.clone()).unwrap();
    append_source_acquisition(&mut recorded, first.clone()).unwrap();
    assert_eq!(recorded.len(), 1);
    let changed = Box::new(
        CapturedSourceAcquisition::captured(digest(b"changed body"), first.seal, first.summary.clone()).unwrap(),
    );
    assert!(append_source_acquisition(&mut recorded, changed.clone()).is_err());
    recorded.push(RecordedSourceAcquisition::Captured { original: changed });
    assert!(source_acquisition_positions(&recorded)[0].is_err());
    let mut second = first.summary.clone();
    second.capture.revision.as_mut().unwrap().sequence = "3".into();
    let second = Box::new(CapturedSourceAcquisition::captured(digest(b"other capture"), first.seal, second).unwrap());
    let mut fresh = vec![RecordedSourceAcquisition::Captured { original: first }];
    append_source_acquisition(&mut fresh, second).unwrap();
    assert_eq!(fresh.len(), 2);
}
#[test]
fn bounded_history_refuses_overflow_without_dropping_durable_originals_or_healing_conflicts_after_a_prefix() {
    let first = original();
    let mut recorded = vec![];
    for sequence in 1..=MAX_RECORDED_SOURCE_ACQUISITIONS {
        let mut summary = first.summary.clone();
        summary.capture.revision.as_mut().unwrap().sequence = sequence.to_string();
        append_source_acquisition(
            &mut recorded,
            Box::new(
                CapturedSourceAcquisition::captured(digest(sequence.to_string().as_bytes()), first.seal, summary)
                    .unwrap(),
            ),
        )
        .unwrap();
    }
    let before = recorded.clone();
    let mut summary = first.summary.clone();
    summary.capture.revision.as_mut().unwrap().sequence = "99".into();
    let overflow = CapturedSourceAcquisition::captured(digest(b"extra"), first.seal, summary).unwrap();
    assert!(append_source_acquisition(&mut recorded, Box::new(overflow)).is_err());
    assert_eq!(recorded, before);
    let RecordedSourceAcquisition::Captured { original } = &recorded[0] else { panic!() };
    recorded.push(RecordedSourceAcquisition::Captured {
        original: Box::new(
            CapturedSourceAcquisition::captured(
                digest(b"conflict beyond cap"),
                original.seal,
                original.summary.clone(),
            )
            .unwrap(),
        ),
    });
    assert!(source_acquisition_positions(&recorded)[0].is_err());
}
