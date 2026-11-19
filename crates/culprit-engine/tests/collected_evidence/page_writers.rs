use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::{PageWriterCase, page_writer_evidence},
};
use culprit_core::{SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn authenticated_writer_rosters_replay_the_same_native_access_relationships_without_latch_or_cost_ownership() {
    for case in PageWriterCase::ALL {
        let (source, records) = page_writer_evidence(case);
        let (focus, native) = native(&source, &records);
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let outcome = SourceOutcome {
            source: coverage.source.clone(),
            state: coverage.state,
            reason: None,
            reason_message: None,
        };
        let expected =
            culprit_checks::page_waits::explain_writers(focus, &context, Some(native), Some(&outcome)).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        let result = collected_resources::page_wait_writers(&capture).unwrap();
        assert_eq!(result.context.source, source, "{case:?}");
        assert_eq!(result.context.records.len(), 1);
        assert!(result.context.read_gaps.is_empty());
        assert_eq!(
            result.context.records[0].row,
            ExportedRecord::new(&source, capture.capture(), records[0].clone()).unwrap()
        );
        assert_eq!(serde_json::to_value(&result.report).unwrap(), serde_json::to_value(expected).unwrap(), "{case:?}");
        assert_eq!(result.report.original_page, focus.page);
        assert_eq!(result.report.original_target, focus.target);
        assert_eq!(result.report.focus_key, focus.key().unwrap());
        assert_eq!(result.report.target_state, Some(native.target_state));
        assert_eq!(result.report.entries.len(), 2, "every original MARS request stays separate");
        assert_eq!(result.report.entries[0].request.session, result.report.entries[1].request.session);
        assert_ne!(result.report.entries[0].request.request, result.report.entries[1].request.request);
        for (entry, candidate) in result.report.entries.iter().zip(&native.candidates) {
            assert_eq!(entry.parent, candidate.parent);
            assert_eq!(entry.relation, candidate.relation);
            assert_eq!(entry.request_state, candidate.request_state);
            assert_eq!(entry.plan_identity, candidate.plan_identity);
            assert_eq!(entry.client, candidate.request.as_ref().and_then(|request| request.client.clone()));
            assert_eq!(
                entry.accesses,
                candidate.binding.as_ref().map_or_else(Vec::new, |proof| proof.accesses.clone())
            );
        }
        if matches!(case, PageWriterCase::TargetWriter | PageWriterCase::PartialSource) {
            assert_eq!(result.report.entries[0].relation, PageWriterRelation::TargetObjectWrite);
            assert_eq!(
                result.report.entries[0].accesses,
                [MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }]
            );
            assert_eq!(result.report.entries[1].relation, PageWriterRelation::ReadOnlyPlan);
            let Some(DefinitionPayload::Available { text, .. }) = &result.report.entries[0].sql else {
                panic!("authenticated current writer SQL must survive offline projection")
            };
            assert!(text.contains("INSERT INTO dbo.orders"));
            let Some(DefinitionPayload::Available { text, .. }) = &result.report.entries[1].sql else {
                panic!("the separate read-only request retains its own qualified SQL")
            };
            assert!(text.contains("SELECT customer_id FROM dbo.orders"));
        }
        if matches!(
            case,
            PageWriterCase::MissingRequest | PageWriterCase::ChangedRequest | PageWriterCase::PermissionUnavailable
        ) {
            assert!(result.report.entries.iter().all(|entry| entry.sql.is_none() && entry.client.is_none()));
        }
        assert_eq!(result.report.source_state, Some(coverage.state));
        let encoded = writer_content(&result.report);
        for body in [
            "<ShowPlanXML",
            "9264",
            "private-writer-machine-canary",
            "private-writer-instance-canary",
            "\"session_counters\"",
            "\"request_counters\"",
            "\"binding\"",
        ] {
            assert!(!encoded.contains(body), "{case:?}: {body}");
        }
    }
}

fn writer_content(report: &culprit_core::PageWaitWriterReport) -> String {
    let mut value = serde_json::to_value(report).unwrap();
    // The replay test verifies this exact authenticated identity separately. Its SHA can contain a short
    // numeric canary by coincidence; every other field remains part of the private-content scan.
    value.as_object_mut().unwrap().remove("focus_key").unwrap();
    serde_json::to_string(&value).unwrap()
}

#[test]
fn privacy_guard_excludes_only_focus_identity_and_still_catches_literal_content() {
    let (source, records) = page_writer_evidence(PageWriterCase::ReadOnly);
    let (focus, native) = native(&source, &records);
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let mut report = culprit_checks::page_waits::explain_writers(focus, &context, Some(native), None).unwrap();
    assert_eq!(report.focus_key, focus.key().unwrap());

    // A real export50 focus digest contains the four-digit SQL canary without carrying its literal value.
    let focus_digest = "c6ecbd98509cd2e9197827494451bd4f11e1852fc6ed4417d9264101787e032d";
    assert_eq!(focus_digest.len(), 64);
    assert!(focus_digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    report.focus_key = focus_digest.into();
    assert!(!writer_content(&report).contains("9264"));

    let summary = report.summary.clone();
    for content in ["9264", focus_digest] {
        report.summary = content.into();
        assert!(writer_content(&report).contains("9264"), "content fields receive no arbitrary hash exemption");
    }
    report.summary = summary;
    let sql = "SELECT customer_id FROM dbo.orders WHERE customer_id = 9264";
    report.entries[0].sql = Some(DefinitionPayload::Available {
        text: sql.into(),
        sha256: digest(sql.as_bytes()),
        redaction_version: REDACTION_VERSION,
    });
    assert!(writer_content(&report).contains("9264"), "an actual unredacted SQL literal must fail the guard");
}

#[tokio::test]
async fn preview_and_missing_writer_rows_preserve_original_header_but_never_manufacture_a_relationship() {
    for missing in [false, true] {
        let (source, records) = page_writer_evidence(PageWriterCase::TargetWriter);
        let focus = native(&source, &records).0.clone();
        let fixture = Fixture::new(source.clone(), records).await;
        let opens = fixture.recovery_object_reads();
        if missing {
            fixture.remove_first_row(TelemetryStream::OperationalSamples);
        }
        let capture = if missing {
            fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await
        } else {
            fixture.read_families(FAMILY).await
        };
        assert!(capture.records().is_empty());
        assert!(!capture.is_read_complete());
        assert_eq!(fixture.recovery_object_reads(), opens);
        let result = collected_resources::page_wait_writers(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert!(result.context.records.is_empty());
        assert_eq!(result.report.original_page, focus.page);
        assert_eq!(result.report.focus_key, focus.key().unwrap());
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert!(result.report.target_state.is_none());
        assert!(result.report.entries.is_empty());
        assert_eq!(result.context.read_gaps.len(), 1);
        assert_eq!(
            result.context.read_gaps[0].reason,
            if missing { EvidenceGapReason::MissingRows } else { EvidenceGapReason::DetailInRecovery }
        );
        assert_eq!(source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    }
}

#[test]
fn a_writer_row_cannot_publish_under_changed_native_member_epoch_role_or_original_page_receipt() {
    let (source, records) = page_writer_evidence(PageWriterCase::TargetWriter);
    let capture = page_writer_evidence_capture();
    assert!(ExportedRecord::new(&source, &capture, records[0].clone()).is_ok());
    for change in ["member", "epoch", "role", "parent", "file", "bounds"] {
        let mut changed = source.clone();
        match change {
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut changed.partition else { unreachable!() };
                member.as_mut().unwrap().native.id = "another-current-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = Some("recycled-current-native-process".into());
            }
            "role" => {
                let SourceObservation::PageWaitWriters { database_writable, .. } = &mut changed.observation else {
                    unreachable!()
                };
                *database_writable = Some(false);
            }
            "parent" => {
                let SourceObservation::PageWaitWriters { focus, .. } = &mut changed.observation else { unreachable!() };
                focus.page.manifest_sha256 = digest(b"another original page receipt");
            }
            "file" => {
                let SourceObservation::PageWaitWriters { focus, .. } = &mut changed.observation else { unreachable!() };
                focus.target.file.qualifiers.insert("create_lsn".into(), "recycled-file".into());
            }
            "bounds" => {
                let SourceObservation::PageWaitWriters { snapshot, .. } = &mut changed.observation else {
                    unreachable!()
                };
                snapshot.observed_until = snapshot.observed_from.clone();
            }
            _ => unreachable!(),
        }
        assert!(ExportedRecord::new(&changed, &capture, records[0].clone()).is_err(), "{change}");
    }
}

fn native<'a>(
    source: &'a CollectionDescription,
    records: &'a [TelemetryRecord],
) -> (&'a PageWaitWriterFocus, &'a PageWaitWriterObservation) {
    let SourceObservation::PageWaitWriters { focus, .. } = &source.observation else { unreachable!() };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWaitWriters(row), .. }) =
        &records[0]
    else {
        unreachable!()
    };
    (focus, row)
}

fn page_writer_evidence_capture() -> CaptureIdentity {
    let (manifest, _) = culprit_collector::analytical::test_support::fixture();
    manifest.capture
}
