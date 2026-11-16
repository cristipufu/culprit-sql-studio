use super::*;
use culprit_core::{MeasurementScope, ResetMarker};
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn sample(record: &TelemetryRecord) -> &StatementCounterSample {
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::StatementCounters(sample), ..
    }) = record
    else {
        panic!("native statement-counter point")
    };
    sample
}

fn column(fixture: &Fixture, name: &str) -> usize {
    fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == name)
        .expect("declared warehouse column")
}

#[tokio::test]
async fn statement_points_replay_exact_native_node_populations_counts_and_missing_values_without_recovery_payloads() {
    let mut large_count = false;
    let mut measured_zero = false;
    let mut missing_work = false;
    let mut partial_source = false;
    for (source, records) in test_support::statement_work_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_statement_work_capture(&capture, &source, &records);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().pages as usize, records.len());
        for row in capture.records() {
            let native = sample(&row.row().record);
            if native.scope != MeasurementScope::Node {
                continue;
            }
            let SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) } = &source.partition else {
                panic!("node points require native member and process qualification")
            };
            assert_eq!(source.target.engine().spec().statement_counter_member(&native.identity).as_ref(), Some(member));
            assert_eq!(native.reset, ResetMarker::Unknown);
            let expected = records.iter().map(sample).find(|expected| expected.identity == native.identity).unwrap();
            assert_eq!(native.totals, expected.totals);
            assert_eq!(native.observed_at, expected.observed_at);
            let work = statement_temporary_work(native, &source.source).unwrap();
            missing_work |= work.is_empty();
            for counter in &native.totals {
                if matches!(
                    counter.key,
                    RuntimeMetricKey::TemporaryBlocksRead | RuntimeMetricKey::TemporaryBlocksWritten
                ) {
                    let MeasurementValue::Integer(value) = &counter.total else {
                        panic!("native block counts must retain exact integer text")
                    };
                    large_count |= value.parse::<u64>().unwrap() > 1 << 53;
                    measured_zero |= value == "0";
                }
            }
            partial_source |=
                source.observation.snapshot().unwrap().sources[0].state == culprit_core::SourceState::Partial;
        }
    }
    assert!(large_count && measured_zero && missing_work && partial_source);
}

#[tokio::test]
async fn a_missing_node_point_retains_native_population_counts_and_partial_visibility_without_inventing_zero() {
    let (source, records) = test_support::node_statement_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let mut rows = fixture.rows.clone();
    rows.remove(0);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), records.len() - 1);
    assert_eq!(capture.source().observation.snapshot().unwrap().sources[0].records as usize, records.len());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for actual in capture.records() {
        let expected =
            records.iter().find(|record| sample(record).identity == sample(&actual.row().record).identity).unwrap();
        assert_eq!(&actual.row().record, expected);
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_node_epoch_or_block_count_fails_the_original_receipt_before_recovery_payloads_open() {
    let (source, records) = test_support::node_statement_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let details_column = column(&fixture, "Details");
    let scope_column = column(&fixture, "SourceId");
    let selected = fixture
        .rows
        .iter()
        .position(|row| {
            let details: Value = serde_json::from_str(row[details_column].as_str().unwrap()).unwrap();
            details["totals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|metric| metric["key"]["kind"] == json!("temporary-blocks-written"))
        })
        .expect("native fixture has a block count");
    for changed in ["node", "epoch", "count"] {
        let mut rows = fixture.rows.clone();
        if changed == "epoch" {
            let mut changed_source = source.clone();
            let SourcePartition::InstanceLocal { epoch, .. } = &mut changed_source.partition else { panic!() };
            *epoch = Some("another-native-process".into());
            rows[selected][scope_column] = json!(changed_source.captured_range().unwrap().scope_id);
        } else {
            let mut details: Value = serde_json::from_str(rows[selected][details_column].as_str().unwrap()).unwrap();
            if changed == "node" {
                details["identity"]["qualifiers"]["node"] = json!("11111111111111111111111111111111");
            } else {
                let block_count = details["totals"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|metric| metric["key"]["kind"] == json!("temporary-blocks-written"))
                    .expect("native fixture has a block count");
                block_count["total"]["value"] = json!("1");
            }
            rows[selected][details_column] = json!(serde_json::to_string(&details).unwrap());
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{changed}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened, "{changed}");
    }
}
