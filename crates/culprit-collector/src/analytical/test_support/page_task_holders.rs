//! Synthetic replay inputs retain separate original Activity/page receipts and two current native task points.
//! Controller/native tests must rebind both original receipts under their actual outbox before admission.
use super::page_writers::{reference, retained};
use super::*;

const PARENT_SEQUENCE: u64 = 830_001;
const PAGE_SEQUENCE: u64 = 830_002;
const FROM: &str = "2020-01-01T12:00:02Z";
const FIRST: &str = "2020-01-01T12:00:02.1Z";
const LAST: &str = "2020-01-01T12:00:03.2Z";
const UNTIL: &str = "2020-01-01T12:00:03.9Z";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTaskHolderCase {
    QualifiedBoth,
    QualifiedOne,
    DifferentHolders,
    TaskOnly,
    Sentinel,
    UnknownPointer,
    OtherDatabase,
    Ambiguous,
    ChangedRequest,
    ChangedPage,
    FirstMissFinalPositive,
    PermissionUnavailable,
    SqlUnavailable,
    OversizedSql,
    LongSql,
    /// Keep the producer row intact; a warehouse test removes it only after authenticated publication.
    PartialWarehouseBody,
}

impl PageTaskHolderCase {
    pub const ALL: [Self; 16] = [
        Self::QualifiedBoth,
        Self::QualifiedOne,
        Self::DifferentHolders,
        Self::TaskOnly,
        Self::Sentinel,
        Self::UnknownPointer,
        Self::OtherDatabase,
        Self::Ambiguous,
        Self::ChangedRequest,
        Self::ChangedPage,
        Self::FirstMissFinalPositive,
        Self::PermissionUnavailable,
        Self::SqlUnavailable,
        Self::OversizedSql,
        Self::LongSql,
        Self::PartialWarehouseBody,
    ];
}

pub struct PageTaskHolderFixture {
    pub activity_source: CollectionDescription,
    pub activity_rows: Vec<TelemetryRecord>,
    pub page_source: CollectionDescription,
    pub page_rows: Vec<TelemetryRecord>,
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
}

pub fn page_task_holder_evidence(case: PageTaskHolderCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let fixture = page_task_holder_fixture(case);
    (fixture.source, fixture.records)
}

pub fn page_task_holder_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    PageTaskHolderCase::ALL.into_iter().map(page_task_holder_evidence).collect()
}

pub fn page_task_holder_fixture(case: PageTaskHolderCase) -> PageTaskHolderFixture {
    let mut original = page_writer_fixture(PageWriterCase::TargetWriter);
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
        &original.page_rows[0]
    else {
        unreachable!()
    };
    let mut page = page.as_ref().clone();
    let mut task = original_task(&page);
    let TelemetryRecord::Activity(waiter) = &mut original.activity_rows[0] else { unreachable!() };
    // The parallel task's native latch wait does not replace its coordinator's independent wait.
    let coordinator = waiter.wait.as_mut().unwrap();
    coordinator.native_type = Some("CXCONSUMER".into());
    coordinator.identified_resources.clear();
    coordinator.owners.clear();
    waiter.waiting_on_lock = Some(false);
    page.request = waiter.as_ref().clone();
    page.resource = task.resource.clone();
    page.task_wait = Some(task.clone());
    let frame = original.activity_source.observation.activity().unwrap();
    assert!(ObservationBounds::new(&frame.observed_until, &page.observed_from).is_some());
    let mut snapshot = original.page_source.observation.snapshot().unwrap().clone();
    // This is a separately acquired original page point, after the complete Activity frame. The generic
    // page corpus's wider window starts before that frame ends and cannot be reused as paired admission.
    snapshot.observed_from = page.observed_from.clone();
    snapshot.observed_until = page.observed_until.clone();
    snapshot.records = 1;
    snapshot.sources = vec![StateSourceCoverage::observed(page.source.clone(), 1, false)];
    original.page_source.source = page.source.clone();
    // Explicit evidence in this synthetic ORIGINAL page read, not a role borrowed from a later holder.
    original.page_source.observation =
        SourceObservation::PageWaitTargets { snapshot, database_writable: Some(true), original_activity: None };
    assert!(
        NativeReadContext::from_snapshot(&original.page_source)
            .unwrap()
            .matches_earlier_scope(&NativeReadContext::from_activity(&original.activity_source).unwrap())
    );
    original.page_rows = vec![page_record(page.clone())];
    let focus = focus(&original, &page, task.clone());
    let TelemetryRecord::Activity(owner) = &original.activity_rows[1] else { unreachable!() };
    let owner = owner.as_ref().clone();

    page.observed_from = FIRST.into();
    page.observed_until = "2020-01-01T12:00:02.8Z".into();
    page.before_check.as_mut().unwrap().observed_at = "2020-01-01T12:00:02.2Z".into();
    page.after_check.as_mut().unwrap().observed_at = "2020-01-01T12:00:02.7Z".into();
    task.observed_at = FIRST.into();
    let (before_task, before) = point(case, task.clone(), owner.clone(), false);
    page.task_wait = Some(before_task);
    task.observed_at = LAST.into();
    let (after_task, after) = point(case, task, owner, true);
    let mut final_target =
        PageTaskHolderTargetCheck { observed_at: "2020-01-01T12:00:03Z".into(), target: page.target.clone().unwrap() };
    if case == PageTaskHolderCase::ChangedPage {
        final_target
            .target
            .object
            .as_mut()
            .unwrap()
            .native
            .qualifiers
            .insert("modified_without_offset".into(), "2019-01-03T10:00:00.0000000".into());
    }
    let unavailable =
        matches!(case, PageTaskHolderCase::PermissionUnavailable | PageTaskHolderCase::FirstMissFinalPositive);
    let row = PageTaskHolderObservation {
        observed_from: FROM.into(),
        observed_until: UNTIL.into(),
        focus_key: focus.key().unwrap(),
        target_state: match case {
            PageTaskHolderCase::ChangedRequest => PageWriterTargetState::NoLongerObserved,
            PageTaskHolderCase::ChangedPage => PageWriterTargetState::ChangedTarget,
            _ if unavailable => PageWriterTargetState::Unavailable,
            _ => PageWriterTargetState::CurrentMapped,
        },
        page: (!unavailable).then(|| Box::new(page)),
        before,
        final_target: (!unavailable).then(|| Box::new(final_target)),
        after_task: (case != PageTaskHolderCase::PermissionUnavailable).then_some(after_task),
        after,
    };
    let mut coverage = StateSourceCoverage::observed(culprit_core::source_id::PAGE_WAIT_TARGETS.into(), 1, false);
    match case {
        PageTaskHolderCase::PermissionUnavailable | PageTaskHolderCase::SqlUnavailable => {
            coverage.error_code = Some(culprit_core::ErrorCode::PermissionDenied);
            coverage.stopped = Some(CollectionStop::SourceUnavailable);
        }
        PageTaskHolderCase::OversizedSql => coverage.stopped = Some(CollectionStop::ByteLimit),
        _ => {}
    }
    let source = CollectionDescription {
        target: original.page_source.target.clone(),
        partition: original.page_source.partition.clone(),
        source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageTaskHolders {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageTaskHolders,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: 1,
                sources: vec![coverage],
            },
            focus: Box::new(focus),
            database_writable: Some(true),
        },
    };
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let SourceObservation::PageTaskHolders { focus, .. } = &source.observation else { unreachable!() };
    assert!(focus.accepts_observation(&context, &row), "{case:?}");
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: source.source.clone(),
        key: row.key().unwrap(),
        fact: OperationalFact::PageTaskHolders(Box::new(row)),
    });
    PageTaskHolderFixture {
        activity_source: original.activity_source,
        activity_rows: original.activity_rows,
        page_source: original.page_source,
        page_rows: original.page_rows,
        source,
        records: vec![record],
    }
}

fn original_task(page: &PageWaitObservation) -> MssqlPageTaskWait {
    MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        task_address: "0000000000000061".into(),
        session_id: page.request.key.session.id.parse().unwrap(),
        request_id: page.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: 2,
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("9007199254740993".into()),
        resource: culprit_core::ObservedWaitResource {
            kind: culprit_core::WaitResourceKind::MemoryPage,
            native: page.resource.native.clone().qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
        },
        resource_address: Some("00000000AABB0042".into()),
        blocking_task_address: Some("00000000BBAA0073".into()),
        blocking_session_id: Some(73),
        blocking_execution_context_id: Some(0),
    }
}

fn focus(original: &PageWriterFixture, page: &PageWaitObservation, task: MssqlPageTaskWait) -> PageTaskHolderFocus {
    let activity = retained(&original.activity_source, &original.activity_rows, PARENT_SEQUENCE);
    let pages = retained(&original.page_source, &original.page_rows, PAGE_SEQUENCE);
    let frame = original.activity_source.observation.activity().unwrap();
    let focus = PageTaskHolderFocus {
        participant: ActivityRequestFocus {
            parent: reference(&activity, &original.activity_rows[0], 0),
            target: original.activity_source.target.clone(),
            partition: original.activity_source.partition.clone(),
            database_writable: frame.database_writable,
            source: original.activity_source.source.clone(),
            epoch: frame.epoch.clone().unwrap(),
            original_observed_at: frame.at.clone(),
            original: page.request.clone(),
            statement_identity: page.statement_identity.clone().unwrap(),
        },
        page: reference(&pages, &original.page_rows[0], 0),
        address: page.observed_address.unwrap(),
        task,
        target: page.target.clone().unwrap(),
        original_observed_until: page.observed_until.clone(),
    };
    assert!(focus.matches_page(&original.page_source, page, &focus.page));
    let references = original
        .activity_rows
        .iter()
        .enumerate()
        .map(|(ordinal, row)| reference(&activity, row, ordinal))
        .collect::<Vec<_>>();
    let rows = original
        .activity_rows
        .iter()
        .zip(&references)
        .map(|(row, reference)| {
            let TelemetryRecord::Activity(observation) = row else { unreachable!() };
            ActivityFrameRow { observation, reference: Some(reference) }
        })
        .collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(&original.activity_source, &rows, true).unwrap();
    assert!(focus.matches_activity_frame(&frame));
    focus
}

fn page_record(page: PageWaitObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(page)),
    })
}

fn point(
    case: PageTaskHolderCase,
    mut task: MssqlPageTaskWait,
    mut request: ActivityObservation,
    final_point: bool,
) -> (MssqlPageTaskWait, MssqlPageTaskHolderPoint) {
    if case == PageTaskHolderCase::DifferentHolders && final_point {
        task.blocking_task_address = Some("00000000BBAA0074".into());
        task.blocking_session_id = Some(74);
        request.key.session = NativeActivityIdentity { id: "74".into(), generation: "native-current-holder-74".into() };
        request.key.request.as_mut().unwrap().generation = "native-current-request-74".into();
        request.statement_identity = Some(NativeIdentity::new("native-current-statement-74"));
        request.statement = DefinitionPayload::sql("SELECT customer_id FROM dbo.orders", culprit_core::Engine::Mssql);
    }
    match case {
        PageTaskHolderCase::SqlUnavailable => {
            request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::SourceUnavailable }
        }
        PageTaskHolderCase::OversizedSql => {
            request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
        }
        PageTaskHolderCase::LongSql => {
            let values = std::iter::repeat_n("9264", 600).collect::<Vec<_>>().join(",");
            request.key.request.as_mut().unwrap().generation = "current-long-holder-request".into();
            request.statement_identity = Some(NativeIdentity::new("current-long-holder-statement"));
            request.statement = DefinitionPayload::sql(
                &format!("SELECT customer_id FROM dbo.orders WHERE customer_id IN ({values})"),
                culprit_core::Engine::Mssql,
            );
            let DefinitionPayload::Available { text, .. } = &request.statement else { unreachable!() };
            assert!(text.len() > 1024, "long exact SQL must require authenticated payload recovery after preview");
        }
        _ => {}
    }
    let mut holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::QualifiedRequest,
        matching_owner_rows: 1,
        task: Some(MssqlPageTaskIdentity {
            task_address: task.blocking_task_address.clone().unwrap(),
            session_id: task.blocking_session_id.map(|id| id as u32),
            request_id: request.key.request.as_ref().map(|key| key.id.parse().unwrap()),
            execution_context_id: 0,
        }),
        request: Some(Box::new(request)),
    };
    match case {
        PageTaskHolderCase::TaskOnly | PageTaskHolderCase::OtherDatabase => {
            holder.relation = if case == PageTaskHolderCase::TaskOnly {
                PageTaskHolderRelation::TaskOnly
            } else {
                PageTaskHolderRelation::OtherDatabase
            };
            holder.request = None;
        }
        PageTaskHolderCase::Sentinel => {
            task.blocking_session_id = Some(-4);
            holder = unresolved(PageTaskHolderRelation::Sentinel, 0);
        }
        PageTaskHolderCase::UnknownPointer | PageTaskHolderCase::QualifiedOne
            if case == PageTaskHolderCase::UnknownPointer || final_point =>
        {
            task.blocking_task_address = None;
            holder = unresolved(PageTaskHolderRelation::NotReported, 0);
        }
        PageTaskHolderCase::Ambiguous => {
            holder = unresolved(PageTaskHolderRelation::Ambiguous, MAX_PAGE_TASK_HOLDER_ROWS)
        }
        PageTaskHolderCase::PermissionUnavailable => holder = unresolved(PageTaskHolderRelation::Unavailable, 0),
        PageTaskHolderCase::FirstMissFinalPositive if !final_point => {
            holder = unresolved(PageTaskHolderRelation::Unavailable, 0)
        }
        PageTaskHolderCase::ChangedRequest if final_point => {
            holder.relation = PageTaskHolderRelation::Changed;
            holder.request = None;
        }
        _ => {}
    }
    let (request, wait) = match case {
        PageTaskHolderCase::PermissionUnavailable => {
            (PageWaitRequestState::SameStatement, PageWaitRelation::Unqualified)
        }
        PageTaskHolderCase::FirstMissFinalPositive if !final_point => {
            (PageWaitRequestState::SameStatement, PageWaitRelation::NoLongerObserved)
        }
        PageTaskHolderCase::ChangedRequest if final_point => {
            (PageWaitRequestState::Changed, PageWaitRelation::Unqualified)
        }
        _ => (PageWaitRequestState::SameStatement, PageWaitRelation::OriginalObserved),
    };
    let point = MssqlPageTaskHolderPoint {
        observed_at: task.observed_at.clone(),
        database_writable: Some(true),
        request,
        wait,
        holder,
    };
    (task, point)
}

fn unresolved(relation: PageTaskHolderRelation, matching_owner_rows: u32) -> MssqlPageTaskHolder {
    MssqlPageTaskHolder { relation, matching_owner_rows, task: None, request: None }
}
