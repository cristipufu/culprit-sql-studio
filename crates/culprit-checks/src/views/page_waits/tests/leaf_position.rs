use super::*;

fn position_message(followed: bool) -> String {
    if followed { t!("view.page_waits.leaf.followed") } else { t!("view.page_waits.leaf.end") }
}

fn assert_no_position(entry: &PageWaitExplanation) {
    assert!(!entry.explanation.contains(&position_message(false)));
    assert!(!entry.explanation.contains(&position_message(true)));
}

#[test]
fn recorded_leaf_links_distinguish_the_observed_end_without_identifying_a_writer_or_cause() {
    for (next_file, next_page, followed) in [(0, 0, false), (1, 4145, true), (2, 8, true)] {
        for state in [SourceState::Collected, SourceState::Partial] {
            let mut row = observation("61", WaitResourceKind::MemoryPage);
            let target = row.target.as_mut().unwrap();
            target.header.next_file_id = Some(next_file);
            target.header.next_page_id = Some(next_page);
            let original = row.clone();
            let report = explain(&[&row], Some(&source(state)), None).unwrap();
            let entry = &report.entries[0];
            assert!(entry.explanation.contains(&position_message(followed)));
            assert!(!entry.explanation.contains(&position_message(!followed)));
            assert_eq!(entry.investigation, t!("view.page_waits.investigation.leaf_position"));
            assert_eq!(entry.verify, t!("view.page_waits.verify"));
            assert_eq!(entry.request_key, row.request.key);
            assert_eq!(entry.resource, row.resource);
            assert_eq!(entry.blocking, culprit_core::PageWaitBlockingParticipation::unavailable());
            assert!(entry.recorded_request.is_none());
            assert!(report.recorded_incident.is_none());
            assert!(report.limitations.contains(&t!("view.page_waits.limit.ownership")));
            assert!(report.limitations.contains(&t!("view.page_waits.limit.bounds")));
            assert!(!entry.investigation.contains("ALTER "));
            assert!(!entry.investigation.contains("OPTIMIZE_FOR_SEQUENTIAL_KEY"));
            assert_eq!(row, original, "native links and waiting-request facts remain in their original row");
        }
    }
}

#[test]
fn missing_partial_and_self_referencing_links_keep_the_leaf_position_unknown() {
    for (file, page) in
        [(None, None), (None, Some(0)), (Some(0), None), (Some(0), Some(9)), (Some(1), Some(0)), (Some(1), Some(4144))]
    {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let header = &mut row.target.as_mut().unwrap().header;
        header.next_file_id = file;
        header.next_page_id = page;
        let report = explain(&[&row], None, None).unwrap();
        let entry = &report.entries[0];
        assert_no_position(entry);
        assert_eq!(entry.investigation, t!("view.page_waits.investigation.leaf"));
        assert_eq!(row.target.as_ref().unwrap().header.next_file_id, file);
        assert_eq!(row.target.as_ref().unwrap().header.next_page_id, page);
    }
}

#[test]
fn io_and_other_page_mechanisms_do_not_borrow_a_leaf_position_diagnosis() {
    for (file, page) in [(0, 0), (1, 4145)] {
        let mut row = observation("61", WaitResourceKind::PageIo);
        let header = &mut row.target.as_mut().unwrap().header;
        header.next_file_id = Some(file);
        header.next_page_id = Some(page);
        let report = explain(&[&row], None, None).unwrap();
        assert_no_position(&report.entries[0]);
        assert_eq!(report.entries[0].investigation, t!("view.page_waits.investigation.io"));
    }
    for (kind, native_type, level, index, investigation) in [
        (PageWaitTargetKind::IndexBranch, 2, 1, "1", "view.page_waits.investigation.branch"),
        (PageWaitTargetKind::HeapData, 1, 0, "0", "view.page_waits.investigation.heap"),
        (PageWaitTargetKind::Pfs, 11, 0, "0", "view.page_waits.investigation.pfs"),
        (PageWaitTargetKind::Gam, 8, 0, "0", "view.page_waits.investigation.gam"),
        (PageWaitTargetKind::Sgam, 9, 0, "0", "view.page_waits.investigation.sgam"),
        (PageWaitTargetKind::Iam, 10, 0, "0", "view.page_waits.investigation.iam"),
        (PageWaitTargetKind::Unclassified, 99, 0, "0", "view.page_waits.investigation.unclassified"),
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let target = row.target.as_mut().unwrap();
        target.kind = kind;
        target.header.page_type = Some(native_type);
        target.header.level = Some(level);
        target.header.index_id = Some(index.into());
        target.object = None;
        let report = explain(&[&row], None, None).unwrap();
        assert_no_position(&report.entries[0]);
        assert_eq!(report.entries[0].investigation, culprit_core::i18n::t(investigation, &[]));
    }
}

#[test]
fn changed_unavailable_or_foreign_targets_cannot_lend_their_leaf_links() {
    for resolution in
        [PageWaitResolution::ChangedMetadata, PageWaitResolution::Unavailable, PageWaitResolution::UnverifiedSource]
    {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let original = row.request.clone();
        row.target = None;
        row.resolution = resolution;
        row.statement_identity = None;
        let report = explain(&[&row], Some(&source(SourceState::Partial)), None).unwrap();
        assert_no_position(&report.entries[0]);
        assert_eq!(report.entries[0].request_key, original.key);
        assert_eq!(report.entries[0].resource, row.resource);
        assert_eq!(report.entries[0].investigation, t!("view.page_waits.investigation.unresolved"));
        assert_eq!(row.request, original);
    }
    for (page, version) in [(4145, 1), (4144, 42)] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let header = &mut row.target.as_mut().unwrap().header;
        header.page_id = page;
        header.header_version = Some(version);
        assert!(explain(&[&row], None, None).is_err(), "foreign pages and unrecognized native formats stay refused");
    }
}

#[test]
fn ended_or_changed_requesters_keep_only_the_recorded_leaf_position_and_original_waiter() {
    for (request, wait, message) in [
        (
            PageWaitRequestState::SameStatement,
            PageWaitRelation::NoLongerObserved,
            "view.page_waits.recheck.no_longer_observed",
        ),
        (PageWaitRequestState::SameStatement, PageWaitRelation::Different, "view.page_waits.recheck.different"),
        (PageWaitRequestState::Missing, PageWaitRelation::Unqualified, "view.page_waits.recheck.missing"),
        (PageWaitRequestState::Changed, PageWaitRelation::Unqualified, "view.page_waits.recheck.changed"),
    ] {
        let mut row = observation("61", WaitResourceKind::MemoryPage);
        let original = row.request.clone();
        row.after_check.as_mut().unwrap().request = request;
        row.after_check.as_mut().unwrap().wait = wait;
        if request != PageWaitRequestState::SameStatement {
            row.statement_identity = None;
        }
        let report = explain(&[&row], Some(&source(SourceState::Partial)), None).unwrap();
        let entry = &report.entries[0];
        assert!(entry.explanation.contains(&position_message(false)));
        assert!(entry.explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert_eq!(entry.request_key, original.key);
        assert_eq!(entry.resource, row.resource);
        assert_eq!(entry.blocking, culprit_core::PageWaitBlockingParticipation::unavailable());
        assert!(entry.recorded_request.is_none());
        assert!(report.recorded_incident.is_none());
        let rendered = serde_json::to_string(&report).unwrap();
        for private in ["SELECT payload", "9264", "private-app-label"] {
            assert!(!rendered.contains(private));
        }
        assert_eq!(row.request, original);
    }
}

#[test]
fn recorded_position_words_localize_without_changing_target_or_participant_identity() {
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            let row = observation("61", WaitResourceKind::MemoryPage);
            let report = explain(&[&row], None, None).unwrap();
            let entry = &report.entries[0];
            assert!(entry.explanation.contains(&t!("view.page_waits.leaf.end")));
            assert_eq!(entry.investigation, t!("view.page_waits.investigation.leaf_position"));
            assert_eq!(entry.request_key, row.request.key);
            assert_eq!(entry.resource, row.resource);
        });
    }
}
