use super::*;
use culprit_collector::analytical::{evidence::EvidenceCapture, test_support::activity_evidence_cases};
use culprit_core::{PageWaitActivityEvidence, PageWaitBlockingIssue};

const PARENT_SEQUENCE: u64 = 910_001;
const PAGE_SEQUENCE: u64 = 910_002;

struct Investigation {
    parent_fixture: Fixture,
    parent: EvidenceCapture,
    parent_rows: Vec<TelemetryRecord>,
    page_source: CollectionDescription,
    page_rows: Vec<TelemetryRecord>,
}

impl Investigation {
    async fn new() -> Self {
        Self::with_owner(ActivityWaitOwner::Session {
            id: "57".into(),
            generation: Some("original-owner-login".into()),
        })
        .await
    }

    async fn with_owner(owner: ActivityWaitOwner) -> Self {
        Self::with_statement(owner, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }).await
    }

    async fn with_statement(owner: ActivityWaitOwner, statement: DefinitionPayload) -> Self {
        let (mut page_source, records) = page_wait_evidence_cases().remove(0);
        let mut page = observations(&records)[0].clone();
        page.request.statement_identity = page.statement_identity.clone();
        page.request.blocking_status = ActivityBlockingStatus::Known;
        page.request.wait.as_mut().unwrap().owners = vec![owner];
        let mut waiter = page.request.clone();
        waiter.statement = statement;
        let parent_rows =
            std::iter::once(waiter).chain((0..2).map(owner_context)).map(TelemetryRecord::from).collect::<Vec<_>>();
        let (mut parent_source, _) = activity_evidence_cases().remove(0);
        parent_source.target = page_source.target.clone();
        parent_source.partition = local_partition();
        let SourceObservation::Activity(native) = &mut parent_source.observation else { unreachable!() };
        native.epoch = Some("original-native-process".into());
        native.database_writable = Some(true);
        native.native_rows = parent_rows.len() as u32;
        native.observations = parent_rows.len() as u32;
        native.coverage = StateSourceCoverage::collected(parent_source.source.clone(), parent_rows.len() as u32);
        let parent_fixture = Fixture::with_sequence(parent_source, parent_rows.clone(), PARENT_SEQUENCE).await;
        let parent = parent_fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(parent.is_read_complete(), "{:?}", parent.gaps());
        let original = original_focus(&parent);
        // This SQL is later requester context. It cannot be copied into an original blocking-session reference.
        page.request.statement =
            DefinitionPayload::sql("SELECT later_waiter_current_sql_canary", culprit_core::Engine::Mssql);
        page_source.partition = local_partition();
        let SourceObservation::PageWaitTargets { mut snapshot, .. } = page_source.observation else { unreachable!() };
        snapshot.records = 1;
        snapshot.sources = vec![StateSourceCoverage::observed(page.source.clone(), 1, false)];
        page_source.observation = SourceObservation::FocusedPageWait {
            snapshot,
            result: Box::new(ActivityWaitFocusResult {
                focus: ActivityWaitFocus { participant: original, resource: page.resource.clone() },
                observed_from: page.observed_from.clone(),
                observed_until: page.observed_until.clone(),
                source: FocusSourceState::Matched,
                request: PageWaitRequestState::SameStatement,
                wait: PageWaitRelation::OriginalObserved,
            }),
        };
        let page_rows = vec![page_record(page)];
        Self { parent_fixture, parent, parent_rows, page_source, page_rows }
    }

    async fn page_capture(&self) -> (Fixture, EvidenceCapture) {
        page_capture(self.page_source.clone(), self.page_rows.clone()).await
    }
}

fn local_partition() -> SourcePartition {
    SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("original-native-machine"),
            native: NativeIdentity::new("original-native-member"),
        }),
        epoch: Some("original-native-process".into()),
    }
}

fn owner_context(index: u32) -> ActivityObservation {
    let mut owner = culprit_collector::analytical::test_support::activity_observation(DefinitionPayload::sql(
        "UPDATE dbo.original_owner_context_canary SET field = 17 WHERE field = 19",
        culprit_core::Engine::Mssql,
    ));
    owner.key.session = NativeActivityIdentity { id: "57".into(), generation: "original-owner-login".into() };
    owner.key.request =
        Some(NativeActivityIdentity { id: index.to_string(), generation: format!("original-owner-request-{index}") });
    owner.state = "running".into();
    owner.command = Some(if index == 0 { "UPDATE" } else { "SELECT" }.into());
    owner.wait = None;
    owner.waiting_on_lock = Some(false);
    owner.statement_identity = Some(NativeIdentity::new(format!("original-owner-statement-{index}")));
    owner
}

fn reference(
    capture: &EvidenceCapture,
    row: &culprit_collector::analytical::evidence::VerifiedEvidenceRecord,
) -> DiagnosticEvidenceRef {
    DiagnosticEvidenceRef::from_export(
        capture.source(),
        capture.capture_manifest_sha256(),
        row.row(),
        row.row_ordinal().checked_sub(1).unwrap(),
    )
    .unwrap()
}

fn original_focus(parent: &EvidenceCapture) -> ActivityRequestFocus {
    let row = &parent.records()[0];
    let TelemetryRecord::Activity(original) = &row.row().record else { unreachable!() };
    let native = parent.source().observation.activity().unwrap();
    ActivityRequestFocus {
        parent: reference(parent, row),
        target: parent.source().target.clone(),
        partition: parent.source().partition.clone(),
        database_writable: native.database_writable,
        source: parent.source().source.clone(),
        epoch: native.epoch.clone().unwrap(),
        original_observed_at: native.at.clone(),
        original: original.as_ref().clone(),
        statement_identity: original.statement_identity.clone().unwrap(),
    }
}

fn page_record(page: PageWaitObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(page)),
    })
}

async fn page_capture(source: CollectionDescription, rows: Vec<TelemetryRecord>) -> (Fixture, EvidenceCapture) {
    let fixture = Fixture::with_sequence(source, rows, PAGE_SEQUENCE).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    (fixture, capture)
}

#[tokio::test]
async fn authentic_original_parent_exposes_every_mars_context_and_exact_receipt_without_copying_sql_or_costs() {
    let investigation = Investigation::new().await;
    let parent_reads = investigation.parent_fixture.recovery_object_reads();
    let (page_fixture, page) = investigation.page_capture().await;
    let page_reads = page_fixture.recovery_object_reads();
    assert_ne!(page.capture(), investigation.parent.capture());
    let analysis = collected_resources::page_waits(&page, Some(&investigation.parent)).unwrap();
    let entry = &analysis.report.entries[0];
    assert_eq!(entry.blocking.context_count, 2);
    assert_eq!(entry.blocking.contexts.len(), 2);
    let expected =
        investigation.parent.records()[1..].iter().map(|row| reference(&investigation.parent, row)).collect::<Vec<_>>();
    assert_eq!(entry.blocking.references().cloned().collect::<Vec<_>>(), expected);
    assert_eq!(entry.blocking.contexts[0].command.as_deref(), Some("UPDATE"));
    assert_eq!(entry.blocking.contexts[1].command.as_deref(), Some("SELECT"));
    assert!(entry.blocking.issues.contains(&PageWaitBlockingIssue::MultipleOwnerContexts));
    assert_eq!(entry.blocking.basis.as_ref().unwrap().context.partition, local_partition());
    assert_eq!(analysis.context.records.len(), 1);
    let original = analysis.activity_context.as_ref().unwrap();
    assert_eq!(original.records.len(), investigation.parent_rows.len());
    for (expected, actual) in investigation.parent_rows.iter().zip(&original.records) {
        assert_eq!(
            actual.row,
            ExportedRecord::new(investigation.parent.source(), investigation.parent.capture(), expected.clone())
                .unwrap()
        );
    }
    let refs =
        investigation.parent.records().iter().map(|row| reference(&investigation.parent, row)).collect::<Vec<_>>();
    let borrowed = investigation
        .parent
        .records()
        .iter()
        .zip(&refs)
        .map(|(record, reference)| {
            let TelemetryRecord::Activity(observation) = &record.row().record else { unreachable!() };
            ActivityFrameRow { observation, reference: Some(reference) }
        })
        .collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(investigation.parent.source(), &borrowed, true).unwrap();
    let SourceObservation::FocusedPageWait { result, snapshot } = &page.source().observation else { unreachable!() };
    let source = SourceOutcome {
        source: snapshot.sources[0].source.clone(),
        state: SourceState::Partial,
        reason: None,
        reason_message: None,
    };
    let pure = culprit_checks::page_waits::explain(
        &observations(&investigation.page_rows),
        Some(&source),
        Some(PageWaitActivityEvidence { source: page.source(), original: result.focus.participant(), frame: &frame }),
    )
    .unwrap();
    assert_eq!(analysis.report, pure);
    let recorded = analysis.report.recorded_incident.as_ref().unwrap();
    assert_eq!(recorded.original, reference(&investigation.parent, &investigation.parent.records()[0]));
    assert_eq!(recorded.sql, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    let TelemetryRecord::Activity(waiter) = &investigation.parent_rows[0] else { unreachable!() };
    assert_eq!(recorded.client, waiter.client);
    let encoded = serde_json::to_string(&analysis.report).unwrap();
    for native_body in ["original_owner_context_canary", "later_waiter_current_sql_canary", "\"request_counters\""] {
        assert!(!encoded.contains(native_body), "{native_body}");
    }
    assert_eq!(page_fixture.recovery_object_reads(), page_reads);
    assert_eq!(investigation.parent_fixture.recovery_object_reads(), parent_reads);
    assert_eq!(analysis.context.metrics.recovery_objects, 0);
    assert_eq!(original.metrics.recovery_objects, 0);
}

#[tokio::test]
async fn owner_only_or_empty_original_warehouse_reads_keep_native_page_evidence_without_synthetic_parent_rows() {
    let investigation = Investigation::new().await;
    let (_, page) = investigation.page_capture().await;
    investigation.parent_fixture.remove_first_row(TelemetryStream::ActivityObservations);
    for expected_rows in [2, 0] {
        if expected_rows == 0 {
            investigation.parent_fixture.remove_family(TelemetryStream::ActivityObservations);
        }
        let partial = investigation.parent_fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(!partial.is_read_complete());
        assert_eq!(partial.records().len(), expected_rows);
        let analysis = collected_resources::page_waits(&page, Some(&partial)).unwrap();
        assert_eq!(analysis.report.entries.len(), 1);
        assert!(analysis.report.recorded_incident.is_none());
        assert_eq!(analysis.context.records.len(), 1);
        let activity = analysis.activity_context.as_ref().unwrap();
        assert_eq!(activity.records.len(), expected_rows);
        assert_eq!(activity.source, *investigation.parent.source());
        assert_eq!(activity.source.observation.activity().unwrap().coverage.state, SourceState::Collected);
        assert!(activity.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
        let blocking = &analysis.report.entries[0].blocking;
        assert!(blocking.basis.is_none());
        assert!(blocking.reported_owners.is_empty());
        assert!(blocking.contexts.is_empty());
        assert_eq!(blocking.issues, [PageWaitBlockingIssue::FrameUnavailable, PageWaitBlockingIssue::ReadIncomplete]);
    }
}

#[tokio::test]
async fn authenticated_primary_cannot_replace_original_capture_manifest_scope_role_generation_or_source() {
    let investigation = Investigation::new().await;
    for field in ["capture", "manifest", "scope", "role", "generation", "source", "clock", "original-body"] {
        let mut source = investigation.page_source.clone();
        let mut rows = investigation.page_rows.clone();
        let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { unreachable!() };
        let original = result.focus.participant_mut();
        match field {
            "capture" => original.parent.capture.revision.as_mut().unwrap().sequence = "999999".into(),
            "manifest" => original.parent.manifest_sha256 = digest(b"wrong parent manifest"),
            "scope" => original.parent.source_scope_id = digest(b"wrong parent scope"),
            "role" => original.database_writable = Some(false),
            "generation" => {
                original.original.key.session.generation = "reused-session-login".into();
                original.parent.native_key = NativeRecordIdentity::Activity(original.original.key.clone());
                let TelemetryRecord::Operational(record) = &mut rows[0] else { unreachable!() };
                let OperationalFact::PageWait(page) = &mut record.fact else { unreachable!() };
                page.request.key = original.original.key.clone();
                record.key = page.key().unwrap();
            }
            "source" => original.source = "different-original-source".into(),
            "clock" => original.original_observed_at = "2020-01-01T12:00:00.000150000Z".into(),
            _ => original.original.command = Some("DELETE".into()),
        }
        assert!(result.is_valid(), "{field}");
        let (_, primary) = page_capture(source, rows).await;
        let expected = if matches!(field, "capture" | "manifest" | "scope") {
            culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)
        } else {
            culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)
        };
        assert_eq!(
            collected_resources::page_waits(&primary, Some(&investigation.parent)).unwrap_err().detail,
            Some(expected),
            "{field}"
        );
    }
}

#[tokio::test]
async fn a_later_native_frame_with_the_same_ids_and_new_sql_cannot_substitute_for_the_original_parent() {
    let investigation = Investigation::new().await;
    let (_, page) = investigation.page_capture().await;
    let mut rows = investigation.parent_rows.clone();
    for record in &mut rows {
        let TelemetryRecord::Activity(row) = record else { unreachable!() };
        row.statement =
            DefinitionPayload::sql("SELECT substituted_current_owner_sql_canary", culprit_core::Engine::Mssql);
        row.request_counters.as_mut().unwrap().cpu_ms = Some("123456".into());
    }
    let later = Fixture::with_sequence(investigation.parent.source().clone(), rows, PARENT_SEQUENCE + 100).await;
    let later = later.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(later.is_read_complete());
    assert_eq!(
        later.records()[0].row().record.native_identity(),
        investigation.parent.records()[0].row().record.native_identity()
    );
    assert!(collected_resources::page_waits(&page, Some(&later)).is_err());
    let page_only = collected_resources::page_waits(&page, None).unwrap();
    assert!(page_only.activity_context.is_none());
    assert!(page_only.report.recorded_incident.is_none());
    assert!(page_only.report.entries[0].blocking.contexts.is_empty());
    assert_eq!(page_only.report.entries[0].blocking.issues, [PageWaitBlockingIssue::FrameUnavailable]);
}

#[tokio::test]
async fn unknown_or_recycled_reported_owner_generations_do_not_attach_visible_current_session_contexts() {
    for (owner, issue) in [
        (
            ActivityWaitOwner::Session { id: "57".into(), generation: None },
            PageWaitBlockingIssue::OwnerGenerationUnavailable,
        ),
        (
            ActivityWaitOwner::Session { id: "57".into(), generation: Some("previous-owner-login".into()) },
            PageWaitBlockingIssue::OwnerContextUnavailable,
        ),
        (
            ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch },
            PageWaitBlockingIssue::WaitOwnerUnavailable,
        ),
    ] {
        let investigation = Investigation::with_owner(owner.clone()).await;
        let (_, page) = investigation.page_capture().await;
        let analysis = collected_resources::page_waits(&page, Some(&investigation.parent)).unwrap();
        let blocking = &analysis.report.entries[0].blocking;
        assert_eq!(analysis.activity_context.as_ref().unwrap().records.len(), 3);
        assert_eq!(blocking.reported_owners, [owner]);
        assert!(blocking.contexts.is_empty());
        assert_eq!(blocking.references().count(), 0);
        assert!(blocking.issues.contains(&issue));
    }
}

#[tokio::test]
async fn empty_focused_page_results_still_require_the_exact_original_activity_role_and_clock() {
    let investigation = Investigation::new().await;
    let mut empty = investigation.page_source.clone();
    let SourceObservation::FocusedPageWait { snapshot, result } = &mut empty.observation else { unreachable!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::observed(snapshot.sources[0].source.clone(), 0, false);
    result.request = PageWaitRequestState::Missing;
    result.wait = PageWaitRelation::Unqualified;
    let (_, primary) = page_capture(empty.clone(), vec![]).await;
    let analyzed = collected_resources::page_waits(&primary, Some(&investigation.parent)).unwrap();
    assert!(analyzed.report.entries.is_empty());
    assert_eq!(analyzed.report.source_state, Some(SourceState::Partial));
    assert_eq!(analyzed.activity_context.as_ref().unwrap().records.len(), 3);
    assert_eq!(
        analyzed.report.recorded_incident.as_ref().unwrap().sql,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    );
    for field in ["role", "clock", "original-body"] {
        let mut changed = empty.clone();
        let SourceObservation::FocusedPageWait { result, .. } = &mut changed.observation else { unreachable!() };
        let original = result.focus.participant_mut();
        match field {
            "role" => original.database_writable = Some(false),
            "clock" => original.original_observed_at = "2020-01-01T12:00:00.000150000Z".into(),
            _ => original.original.command = Some("DELETE".into()),
        }
        let (_, primary) = page_capture(changed, vec![]).await;
        assert_eq!(
            collected_resources::page_waits(&primary, Some(&investigation.parent)).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)),
            "{field}"
        );
    }
}

#[tokio::test]
async fn recorded_waiter_sql_survives_missing_changed_and_ended_lookup_without_another_archive_read() {
    let sql = DefinitionPayload::sql(
        "SELECT recorded_waiter_payload FROM dbo.orders WHERE customer_id=9264",
        culprit_core::Engine::Mssql,
    );
    let investigation = Investigation::with_statement(
        ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch },
        sql.clone(),
    )
    .await;
    let original_rows = serde_json::to_value(investigation.parent.records()).unwrap();
    let parent_reads = investigation.parent_fixture.recovery_object_reads();
    for (request, wait) in [
        (PageWaitRequestState::Missing, PageWaitRelation::Unqualified),
        (PageWaitRequestState::Changed, PageWaitRelation::Unqualified),
        (PageWaitRequestState::SameStatement, PageWaitRelation::NoLongerObserved),
    ] {
        let mut source = investigation.page_source.clone();
        let SourceObservation::FocusedPageWait { snapshot, result } = &mut source.observation else { unreachable!() };
        snapshot.records = 0;
        snapshot.sources[0] = StateSourceCoverage::observed(snapshot.sources[0].source.clone(), 0, false);
        result.request = request;
        result.wait = wait;
        let focus = result.focus.clone();
        let (fixture, primary) = page_capture(source, vec![]).await;
        let page_reads = fixture.recovery_object_reads();
        let analysis = collected_resources::page_waits(&primary, Some(&investigation.parent)).unwrap();
        let incident = analysis.report.recorded_incident.as_ref().unwrap();
        assert!(analysis.report.entries.is_empty());
        assert_eq!(incident.original, reference(&investigation.parent, &investigation.parent.records()[0]));
        assert_eq!(incident.observed_at, investigation.parent.source().observation.activity().unwrap().at);
        assert_eq!(incident.request, focus.participant().original.key);
        assert_eq!(incident.resource, focus.resource);
        assert_eq!(incident.sql, sql);
        assert_eq!(incident.client, focus.participant().original.client);
        assert!(analysis.context.records.is_empty());
        assert_eq!(analysis.activity_context.as_ref().unwrap().records.len(), 3);
        assert!(!serde_json::to_string(&analysis.report).unwrap().contains("later_waiter_current_sql_canary"));
        assert_eq!(fixture.recovery_object_reads(), page_reads);
        assert_eq!(investigation.parent_fixture.recovery_object_reads(), parent_reads);
    }
    assert_eq!(serde_json::to_value(investigation.parent.records()).unwrap(), original_rows);
}

#[tokio::test]
async fn a_present_original_sql_row_in_an_incomplete_frame_cannot_be_promoted_from_its_positive_prefix() {
    let investigation = Investigation::with_statement(
        ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch },
        DefinitionPayload::sql("SELECT recorded_waiter_payload FROM dbo.orders", culprit_core::Engine::Mssql),
    )
    .await;
    let (_, page) = investigation.page_capture().await;
    investigation.parent_fixture.remove_row(TelemetryStream::ActivityObservations, 1);
    let partial = investigation.parent_fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(!partial.is_read_complete());
    assert_eq!(partial.records().len(), 2);
    assert_eq!(partial.records()[0].row(), investigation.parent.records()[0].row());
    let reads = investigation.parent_fixture.recovery_object_reads();
    let analysis = collected_resources::page_waits(&page, Some(&partial)).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    assert!(analysis.report.recorded_incident.is_none());
    assert_eq!(analysis.activity_context.as_ref().unwrap().records.len(), 2);
    assert_eq!(investigation.parent_fixture.recovery_object_reads(), reads);
}

#[tokio::test]
async fn absent_original_warehouse_row_cannot_authorize_a_changed_focus_copy_or_impossible_ordinal() {
    let investigation = Investigation::new().await;
    investigation.parent_fixture.remove_first_row(TelemetryStream::ActivityObservations);
    for retained in [2, 0] {
        if retained == 0 {
            investigation.parent_fixture.remove_family(TelemetryStream::ActivityObservations);
        }
        let partial = investigation.parent_fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(!partial.is_read_complete());
        assert_eq!(partial.records().len(), retained);
        for field in ["copy", "ordinal"] {
            let mut source = investigation.page_source.clone();
            let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { unreachable!() };
            let original = result.focus.participant_mut();
            if field == "copy" {
                original.original.command = Some("DELETE".into());
            } else {
                original.parent.row_ordinal = partial.source().observation.activity().unwrap().observations;
            }
            assert!(original.is_valid());
            let (_, primary) = page_capture(source, investigation.page_rows.clone()).await;
            assert_eq!(
                collected_resources::page_waits(&primary, Some(&partial)).unwrap_err().detail,
                Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)),
                "{field}, {retained} rows"
            );
        }
    }
}
