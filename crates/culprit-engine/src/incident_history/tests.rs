use super::*;
use culprit_collector::analytical::test_support::{IncidentHistoryCase, incident_history_evidence};
use culprit_core::{ErrorDetail, limits::OperationGuard};
fn bytes(case: IncidentHistoryCase) -> Vec<u8> {
    let (source, records) = incident_history_evidence(case);
    let capture = CaptureIdentity {
        observation_id: "selected-history".into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let mut bytes = vec![];
    for row in records.into_iter().filter(|row| row.stream() == TelemetryStream::OperationalSamples) {
        serde_json::to_writer(&mut bytes, &ExportedRecord::new(&source, &capture, row).unwrap()).unwrap();
        bytes.push(b'\n');
    }
    bytes
}
fn read(bytes: &[u8]) -> Result<IncidentHistoryReport> {
    let guard = OperationGuard::new(HISTORY_IMPORT_TIMEOUT).unwrap();
    import_reader(bytes, &guard.handle())
}
#[test]
fn canonical_jsonl_retains_ongoing_uncertain_and_resolved_history_as_unverified_selected_artifacts() {
    for case in IncidentHistoryCase::ALL {
        let report = read(&bytes(*case)).unwrap();
        assert_eq!(report.provenance, IncidentHistoryProvenance::ImportedArtifact);
        assert_eq!(report.source_state, None);
        assert!(report.entries.iter().all(|entry| entry.original.is_none()));
        assert!(!serde_json::to_string(&report).unwrap().contains("private-diagnostic-host"));
        if matches!(case, IncidentHistoryCase::Resolved) {
            assert_eq!(report.entries.last().unwrap().lifecycle, DiagnosticLifecycle::Resolved);
        }
    }
}
#[test]
fn raw_lines_credentials_paths_and_parser_errors_never_escape_import_failures() {
    let error = read(b"{private-password-canary\n").unwrap_err();
    let public = serde_json::to_string(&error).unwrap();
    assert!(!public.contains("private-password-canary"));
    let error = import_file(
        Path::new("/private/credential-path-does-not-exist"),
        &OperationGuard::new(HISTORY_IMPORT_TIMEOUT).unwrap().handle(),
    )
    .unwrap_err();
    assert!(!serde_json::to_string(&error).unwrap().contains("credential-path"));
    assert!(read(b"\n").is_err());
    assert!(read(b"").is_err());
}
#[test]
fn line_record_and_body_limits_refuse_without_returning_a_complete_prefix() {
    assert_eq!(
        read(&vec![b'x'; MAX_HISTORY_LINE_BYTES + 1]).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    let row = bytes(IncidentHistoryCase::Ongoing);
    let mut too_many = Vec::new();
    for _ in 0..=MAX_HISTORY_FILE_RECORDS {
        too_many.extend_from_slice(&row);
    }
    assert_eq!(read(&too_many).unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(
        read(&vec![b' '; MAX_HISTORY_FILE_BYTES as usize + 1]).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
}
#[test]
fn cancelled_or_expired_file_work_never_opens_or_reads_the_selected_artifact() {
    struct NeverRead;
    impl Read for NeverRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("cancelled file work must not read")
        }
    }
    let guard = OperationGuard::new(HISTORY_IMPORT_TIMEOUT).unwrap();
    let handle = guard.handle();
    drop(guard);
    assert_eq!(
        import_reader(NeverRead, &handle).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
    let guard = OperationGuard::new(std::time::Duration::ZERO).unwrap();
    assert_eq!(
        import_file(Path::new("/never-open"), &guard.handle()).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    );
}
#[test]
fn altered_versions_batches_and_revision_bodies_cannot_enter_even_an_unverified_interpretation() {
    let bytes = bytes(IncidentHistoryCase::Ongoing);
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for pointer in ["/format_version", "/representation_version", "/batch_id", "/record/data/fact/data/id"] {
        let mut row = original.clone();
        let value = row.pointer_mut(pointer).unwrap();
        *value = if value.is_number() { serde_json::json!(0) } else { serde_json::json!("unbound") };
        assert!(read(&serde_json::to_vec(&row).unwrap()).is_err(), "{pointer}");
    }
}

#[test]
fn exact_line_and_file_byte_boundaries_accept_eof_but_one_more_byte_refuses_the_whole_artifact() {
    let row = bytes(IncidentHistoryCase::Ongoing);
    let mut line = row[..row.len() - 1].to_vec();
    line.resize(MAX_HISTORY_LINE_BYTES - 1, b' ');
    line.push(b'\n');
    assert!(read(&line).is_ok());
    let mut file = line.repeat(MAX_HISTORY_FILE_BYTES as usize / MAX_HISTORY_LINE_BYTES);
    assert_eq!(file.len() as u64, MAX_HISTORY_FILE_BYTES);
    assert!(read(&file).is_ok());
    file.push(b'\n');
    assert_eq!(read(&file).unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
}
#[test]
fn cancellation_during_a_started_file_read_cannot_publish_a_partial_success() {
    struct CancelRead<'a> {
        bytes: &'a [u8],
        owner: Option<OperationGuard>,
    }
    impl Read for CancelRead<'_> {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            let count = self.bytes.read(target)?;
            self.owner.take();
            Ok(count)
        }
    }
    let bytes = bytes(IncidentHistoryCase::Resolved);
    let guard = OperationGuard::new(HISTORY_IMPORT_TIMEOUT).unwrap();
    let operation = guard.handle();
    let reader = CancelRead { bytes: &bytes, owner: Some(guard) };
    assert_eq!(
        import_reader(reader, &operation).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
}
