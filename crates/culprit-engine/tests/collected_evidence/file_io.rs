use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{assert_operational_capture, file_io_evidence_cases},
};
use culprit_core::{FileKind, FileScope, IoAccounting, ResetMarker, SourceState, telemetry::*};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn counters(record: &TelemetryRecord) -> &FileIoCounters {
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::FileIo(file), .. }) = record
    else {
        panic!("native file-I/O point")
    };
    file
}

#[tokio::test]
async fn authenticated_file_io_readback_keeps_exact_native_counters_gauges_and_source_headers_as_raw_evidence() {
    for (source, records) in file_io_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.families(), FAMILY);
        assert_eq!(capture.source().observation.snapshot().unwrap().dataset, SnapshotDataset::Operational);
        for row in capture.records() {
            let native = counters(&row.row().record);
            assert_eq!(native.reset, ResetMarker::Unknown);
            assert!(native.sample_uptime_ms.is_none());
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(capture.metrics().recovery_bytes, 0);
    }
}

#[tokio::test]
async fn missing_and_unpublished_file_evidence_never_rewrites_the_native_header_or_synthesizes_zero_rows() {
    let (source, records) = file_io_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), records.len() - 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for retained in capture.records() {
        assert!(records.iter().any(|original| &retained.row().record == original));
    }
    assert_eq!(capture.source().observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.source().observation.snapshot().unwrap().records as usize, records.len());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::NotPublished));
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn unknown_capacities_native_maximum_variants_and_shared_accounting_remain_distinct_from_measured_zero() {
    let mut large_queue = false;
    let mut zero_queue = false;
    let mut unknown_queue = false;
    let mut not_applicable_size = false;
    let mut maximum_variants = std::collections::BTreeSet::new();
    let mut temporary_server = false;
    let mut cache_unknown_capacity = false;
    let mut remote_unknown_capacity = false;
    for (source, records) in file_io_evidence_cases() {
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(FAMILY).await;
        for row in capture.records() {
            let native = counters(&row.row().record);
            large_queue |= native.queued_read_ms.as_deref() == Some("9007199254740993")
                || native.queued_write_ms.as_deref() == Some("9223372036854775807");
            zero_queue |= native.queued_read_ms.as_deref() == Some("0");
            unknown_queue |= native.queued_read_ms.is_none();
            not_applicable_size |= native.logical_size == Some(FileSize::NotApplicable);
            if let Some(maximum) = &native.maximum_size {
                maximum_variants.insert(match maximum {
                    FileSizeLimit::NoGrowth => "no-growth",
                    FileSizeLimit::Unlimited => "unlimited",
                    FileSizeLimit::Bytes { bytes } => {
                        assert!(bytes.parse::<u64>().unwrap() > 0);
                        "bytes"
                    }
                });
            }
            if let Some(FileSize::Bytes { bytes }) = &native.logical_size {
                assert!(bytes.parse::<u64>().unwrap() <= i32::MAX as u64 * 8192);
            }
            if native.kind == FileKind::Temp {
                assert_eq!(native.scope, FileScope::Server);
                assert_eq!(native.database.as_deref(), Some("tempdb"));
                temporary_server = true;
            }
            if native.context.read == IoAccounting::CachePage {
                assert_eq!(native.scope, FileScope::Server);
                assert!(native.database.is_none() && native.context.replica_id.is_none());
                assert!(native.logical_size.is_none() && native.maximum_size.is_none());
                assert!(native.size_on_disk_bytes.is_some());
                cache_unknown_capacity = true;
            }
            if native.context.read == IoAccounting::PageServerResponse {
                assert_eq!(native.context.write, IoAccounting::Simulated);
                assert!(native.context.replica_id.is_some());
                assert!(native.logical_size.is_none() && native.maximum_size.is_none());
                assert!(native.size_on_disk_bytes.is_some());
                remote_unknown_capacity = true;
            }
        }
    }
    assert!(large_queue && zero_queue && unknown_queue && not_applicable_size);
    assert_eq!(maximum_variants, ["bytes", "no-growth", "unlimited"].into());
    assert!(temporary_server && cache_unknown_capacity && remote_unknown_capacity);
}
