//! Manufactured retained-history states shared by SDK, local originals and both physical codecs.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspectPageHistoryCase {
    Empty,
    RepairedOnly,
    Active,
    NativeTimeUnknown,
    Partial,
    Denied,
    Unsupported,
}
impl SuspectPageHistoryCase {
    pub const ALL: [Self; 7] = [
        Self::Empty,
        Self::RepairedOnly,
        Self::Active,
        Self::NativeTimeUnknown,
        Self::Partial,
        Self::Denied,
        Self::Unsupported,
    ];
}
pub fn suspect_page_history_evidence(case: SuspectPageHistoryCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = database_performance_counter_evidence(DatabasePerformanceCounterCase::Exact);
    let SourceObservation::DatabasePerformanceCounters { mut snapshot, native_database, database_writable } =
        source.observation
    else {
        unreachable!()
    };
    let unavailable = matches!(case, SuspectPageHistoryCase::Denied | SuspectPageHistoryCase::Unsupported);
    let rows = if unavailable {
        vec![]
    } else {
        let (active, total) = match case {
            SuspectPageHistoryCase::Empty => ("0", "0"),
            SuspectPageHistoryCase::RepairedOnly => ("0", "3"),
            _ => ("2", "4"),
        };
        let point = MssqlSuspectPageHistory {
            database: "shop".into(),
            native_database_id: native_database.id.clone(),
            observed_at: "2020-01-01T12:00:00.500000000Z".into(),
            database_writable: Some(true),
            count: active.into(),
            retained_rows: total.into(),
            last_reported_time_without_offset: if active == "0" || case == SuspectPageHistoryCase::NativeTimeUnknown {
                None
            } else {
                Some("2019-12-30T15:00:00.123".into())
            },
            reset: culprit_core::ResetMarker::Unknown,
        };
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: culprit_core::source_id::SUSPECT_PAGES.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::SuspectPages(Box::new(point)),
        })]
    };
    snapshot.dataset = SnapshotDataset::SuspectPages;
    snapshot.records = rows.len() as u32;
    snapshot.sources = vec![if unavailable {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::SUSPECT_PAGES.into(),
            if case == SuspectPageHistoryCase::Denied {
                culprit_core::ErrorCode::PermissionDenied
            } else {
                culprit_core::ErrorCode::Unsupported
            },
            None,
        )
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::SUSPECT_PAGES.into(),
            1,
            case != SuspectPageHistoryCase::Partial,
        )
    }];
    source.source = "suspect-page-history-state".into();
    source.observation = SourceObservation::SuspectPages { snapshot, native_database, database_writable };
    assert!(source.partition_is_consistent());
    (source, rows)
}
pub fn assert_suspect_page_history_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, rows);
    assert!(suspect_page_context(capture.source()).is_ok());
    for row in capture.records() {
        let TelemetryRecord::Operational(row) = &row.row().record else { panic!() };
        let OperationalFact::SuspectPages(point) = &row.fact else { panic!() };
        assert!(point.fits_source(source));
    }
}
