//! Source-owned selected writer facts built from canonical original Activity and mapped-page receipts.
use super::*;
use culprit_core::Engine;

const PARENT_SEQUENCE: u64 = 820_001;
const PAGE_SEQUENCE: u64 = 820_002;
const WRITER_SQL: &str = "INSERT INTO dbo.orders(customer_id) VALUES (9264)";
const READ_SQL: &str = "SELECT customer_id FROM dbo.orders WHERE customer_id = 9264";
const OTHER_SQL: &str = "INSERT INTO dbo.unrelated(customer_id) VALUES (9264)";
const FROM: &str = "2020-01-01T12:00:02.000000000Z";
const UNTIL: &str = "2020-01-01T12:00:02.900000000Z";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageWriterCase {
    TargetWriter,
    EndOfLeafInsert,
    ReportedBlockingSession,
    ReadOnly,
    OtherTarget,
    MissingRequest,
    ChangedRequest,
    PayloadUnavailable,
    ChangedTarget,
    NoLongerObserved,
    PartialSource,
    PermissionUnavailable,
}

impl PageWriterCase {
    pub const ALL: [Self; 12] = [
        Self::TargetWriter,
        Self::EndOfLeafInsert,
        Self::ReportedBlockingSession,
        Self::ReadOnly,
        Self::OtherTarget,
        Self::MissingRequest,
        Self::ChangedRequest,
        Self::PayloadUnavailable,
        Self::ChangedTarget,
        Self::NoLongerObserved,
        Self::PartialSource,
        Self::PermissionUnavailable,
    ];
}

pub fn page_writer_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    PageWriterCase::ALL.into_iter().map(page_writer_evidence).collect()
}

pub struct PageWriterFixture {
    pub activity_source: CollectionDescription,
    pub activity_rows: Vec<TelemetryRecord>,
    pub page_source: CollectionDescription,
    pub page_rows: Vec<TelemetryRecord>,
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
}

/// The two original candidates share a session but retain different native request generations. Only one selected
/// plan writes the checked object/index; this fixture grants no page, partition, latch-owner or cost attribution.
pub fn page_writer_evidence(case: PageWriterCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let fixture = page_writer_fixture(case);
    (fixture.source, fixture.records)
}

/// Original inputs are exposed separately for tests that seal them under a real outbox/recovery owner and rebind
/// the requested receipts. The fixed synthetic replay identities above never replace those new receipts.
pub fn page_writer_fixture(case: PageWriterCase) -> PageWriterFixture {
    let (mut page_source, records) = page_wait_evidence_cases().remove(0);
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
        &records[0]
    else {
        unreachable!()
    };
    let mut page = page.as_ref().clone();
    page.request.statement_identity = page.statement_identity.clone();
    page.request.elapsed_ms = Some("1000".into());
    if case == PageWriterCase::ReportedBlockingSession {
        page.request.wait.as_mut().unwrap().owners =
            vec![ActivityWaitOwner::Session { id: "73".into(), generation: Some("original-writer-login".into()) }];
    }
    qualify_target(page.target.as_mut().unwrap());
    if case == PageWriterCase::EndOfLeafInsert {
        let target = page.target.as_mut().unwrap();
        target.header.next_file_id = Some(0);
        target.header.next_page_id = Some(0);
        let mut point = super::page_waits::index_point(target, PageIndexOperationRelation::CheckedTarget);
        let PageIndexOperations::Observed { counters, .. } = &mut point else { unreachable!() };
        counters.leading_key_is_identity = Some(true);
        counters.optimize_for_sequential_key = Some(false);
        page.index_operations = Some(point);
    }
    let (parent_source, originals) = original_activity(&page_source, &page, case);
    page_source.partition = parent_source.partition.clone();
    let parent_recovery = retained(&parent_source, &originals, PARENT_SEQUENCE);
    let original = ActivityRequestFocus {
        parent: reference(&parent_recovery, &originals[0], 0),
        target: parent_source.target.clone(),
        partition: parent_source.partition.clone(),
        database_writable: Some(true),
        source: parent_source.source.clone(),
        epoch: parent_source.observation.activity().unwrap().epoch.clone().unwrap(),
        original_observed_at: parent_source.observation.activity().unwrap().at.clone(),
        original: page.request.clone(),
        statement_identity: page.statement_identity.clone().unwrap(),
    };
    let page_record = operational(page.clone());
    let SourceObservation::PageWaitTargets { snapshot, .. } = &mut page_source.observation else { unreachable!() };
    snapshot.records = 1;
    snapshot.sources = vec![StateSourceCoverage::observed(page.source.clone(), 1, false)];
    let page_recovery = retained(&page_source, std::slice::from_ref(&page_record), PAGE_SEQUENCE);
    let focus = PageWaitWriterFocus {
        waiter: ActivityWaitFocus { participant: original, resource: page.resource.clone() },
        page: reference(&page_recovery, &page_record, 0),
        address: page.observed_address.unwrap(),
        target: page.target.clone().unwrap(),
        candidates: originals[1..]
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let TelemetryRecord::Activity(request) = row else { unreachable!() };
                PageWaitWriterRequest {
                    parent: reference(&parent_recovery, row, ordinal + 1),
                    key: request.key.clone(),
                    statement_identity: request.statement_identity.clone().unwrap(),
                }
            })
            .collect(),
    };
    assert!(focus.matches_page(&page_source, &page, &focus.page));
    let candidates = originals[1..]
        .iter()
        .zip(&focus.candidates)
        .enumerate()
        .map(|(index, (original, selected))| {
            let TelemetryRecord::Activity(original) = original else { unreachable!() };
            candidate(case, index, original, selected, &focus.target)
        })
        .collect();
    move_page_point(&mut page);
    let target_state = match case {
        PageWriterCase::ChangedTarget => PageWriterTargetState::ChangedTarget,
        PageWriterCase::NoLongerObserved => PageWriterTargetState::NoLongerObserved,
        PageWriterCase::PermissionUnavailable => PageWriterTargetState::Unavailable,
        _ => PageWriterTargetState::CurrentMapped,
    };
    let row = PageWaitWriterObservation {
        observed_from: FROM.into(),
        observed_until: UNTIL.into(),
        focus_key: focus.key().unwrap(),
        target_state,
        page: (target_state == PageWriterTargetState::CurrentMapped).then(|| Box::new(page)),
        candidates,
    };
    let complete = !matches!(
        case,
        PageWriterCase::PartialSource | PageWriterCase::PayloadUnavailable | PageWriterCase::PermissionUnavailable
    );
    let mut coverage = StateSourceCoverage::observed(culprit_core::source_id::PAGE_WAIT_TARGETS.into(), 1, complete);
    if case == PageWriterCase::PermissionUnavailable {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let source = CollectionDescription {
        target: page_source.target.clone(),
        partition: page_source.partition.clone(),
        source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitWriters {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitWriters,
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
    let SourceObservation::PageWaitWriters { focus, .. } = &source.observation else { unreachable!() };
    assert!(focus.accepts_observation(&context, &row), "{case:?}");
    let record = OperationalObservation {
        source: source.source.clone(),
        key: row.key().unwrap(),
        fact: OperationalFact::PageWaitWriters(Box::new(row)),
    };
    PageWriterFixture {
        activity_source: parent_source,
        activity_rows: originals,
        page_source,
        page_rows: vec![page_record],
        source,
        records: vec![TelemetryRecord::Operational(record)],
    }
}

fn original_activity(
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
    case: PageWriterCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = activity_evidence_cases().remove(0);
    source.target = page_source.target.clone();
    source.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("private-writer-machine-canary"),
            native: NativeIdentity::new("private-writer-instance-canary"),
        }),
        epoch: Some("original-writer-native-process".into()),
    };
    let candidates = (0..2).map(|index| {
        let sql = original_sql(case, index);
        let mut row = activity_observation(DefinitionPayload::sql(sql, Engine::Mssql));
        row.key.session = NativeActivityIdentity { id: "73".into(), generation: "original-writer-login".into() };
        row.key.request = Some(NativeActivityIdentity {
            id: index.to_string(),
            generation: format!("original-writer-request-{index}"),
        });
        row.command = Some(if index == 0 && case != PageWriterCase::ReadOnly { "INSERT" } else { "SELECT" }.into());
        row.state = "running".into();
        row.elapsed_ms = Some("1".into());
        row.wait = None;
        row.waiting_on_lock = Some(false);
        row.statement_identity = Some(
            NativeIdentity::new(format!("original-native-writer-statement-{index}"))
                .qualified("native_object", "sys.dm_exec_requests.sql_handle")
                .qualified("statement_start_offset", "0")
                .qualified("statement_end_offset", "-1"),
        );
        TelemetryRecord::from(row)
    });
    let rows = std::iter::once(TelemetryRecord::from(page.request.clone())).chain(candidates).collect::<Vec<_>>();
    let SourceObservation::Activity(frame) = &mut source.observation else { unreachable!() };
    frame.epoch = Some("original-writer-native-process".into());
    frame.database_writable = Some(true);
    frame.native_rows = rows.len() as u32;
    frame.observations = rows.len() as u32;
    frame.coverage = StateSourceCoverage::collected(source.source.clone(), rows.len() as u32);
    (source, rows)
}

fn qualify_target(target: &mut MappedPageTarget) {
    let object = target.object.as_mut().unwrap();
    object.native = object
        .native
        .clone()
        .qualified("native_object", "sys.all_objects")
        .qualified("created_without_offset", "2019-01-01T10:00:00.0000000")
        .qualified("modified_without_offset", "2019-01-02T10:00:00.0000000")
        .qualified("index_type", "1")
        .qualified("index_id", "1");
    assert!(target.writer_target_key().is_ok());
}

fn candidate(
    case: PageWriterCase,
    index: usize,
    original: &ActivityObservation,
    selected: &PageWaitWriterRequest,
    target: &MappedPageTarget,
) -> PageWaitWriterCandidate {
    let missing = match case {
        PageWriterCase::MissingRequest => Some((PageWaitRequestState::Missing, PayloadUnavailable::Missing)),
        PageWriterCase::ChangedRequest => Some((PageWaitRequestState::Changed, PayloadUnavailable::Missing)),
        PageWriterCase::ChangedTarget | PageWriterCase::NoLongerObserved | PageWriterCase::PermissionUnavailable => {
            Some((PageWaitRequestState::Unverified, PayloadUnavailable::Restricted))
        }
        _ => None,
    };
    if let Some((request_state, reason)) = missing {
        return PageWaitWriterCandidate {
            parent: selected.parent.clone(),
            request_state,
            request: None,
            plan_identity: None,
            plan: DefinitionPayload::Unavailable { reason },
            relation: PageWriterRelation::Unavailable,
            binding: None,
        };
    }
    let relation = match case {
        PageWriterCase::PayloadUnavailable => PageWriterRelation::Unavailable,
        PageWriterCase::ReadOnly => PageWriterRelation::ReadOnlyPlan,
        PageWriterCase::OtherTarget if index == 0 => PageWriterRelation::OtherTarget,
        _ if index == 0 => PageWriterRelation::TargetObjectWrite,
        _ => PageWriterRelation::ReadOnlyPlan,
    };
    let sql = original_sql(case, index);
    let native_plan = native_plan(sql, relation);
    let plan = if relation == PageWriterRelation::Unavailable {
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    } else {
        let payload = DefinitionPayload::plan(&native_plan, Engine::Mssql);
        let DefinitionPayload::Available { text, .. } = &payload else { panic!("valid selected native XML") };
        assert!(text.len() > 1024, "the native body requires explicit recovery hydration");
        payload
    };
    let mut candidate = PageWaitWriterCandidate {
        parent: selected.parent.clone(),
        request_state: PageWaitRequestState::SameStatement,
        request: Some(original.clone()),
        plan_identity: (relation != PageWriterRelation::Unavailable).then(|| {
            NativeIdentity::new(format!("AABBCC0{index}"))
                .qualified("native_object", "sys.dm_exec_requests.plan_handle")
        }),
        plan,
        relation,
        binding: None,
    };
    if relation == PageWriterRelation::TargetObjectWrite {
        candidate.binding = Some(
            MssqlPageWritePlanProof::new(
                target,
                &candidate,
                digest(native_plan.as_bytes()),
                digest(sql.as_bytes()),
                vec![MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }],
            )
            .unwrap(),
        );
    }
    candidate
}

fn native_plan(sql: &str, relation: PageWriterRelation) -> String {
    let writing = matches!(relation, PageWriterRelation::TargetObjectWrite | PageWriterRelation::OtherTarget);
    let table = if relation == PageWriterRelation::OtherTarget { "unrelated" } else { "orders" };
    let (physical, logical, statement_type) =
        if writing { ("Clustered Index Insert", "Insert", "INSERT") } else { ("Index Seek", "Index Seek", "SELECT") };
    let access = if writing {
        format!(
            r#"<Update DMLRequestSort="false"><Object Database="[db]" Schema="[dbo]" Table="[{table}]" Index="[orders_id]" IndexKind="Clustered" />
            <SetPredicate><ScalarOperator ScalarString="[db].[dbo].[{table}].[customer_id]=(9264)"><ScalarExpressionList><ScalarOperator><MultipleAssign><Assign><ColumnReference Database="[db]" Schema="[dbo]" Table="[{table}]" Column="customer_id" /><ScalarOperator><Const ConstValue="(9264)" /></ScalarOperator></Assign></MultipleAssign></ScalarOperator></ScalarExpressionList></ScalarOperator></SetPredicate>
            <RelOp NodeId="2" PhysicalOp="Constant Scan" LogicalOp="Constant Scan" EstimateRows="1" EstimateCPU="0.001" EstimatedTotalSubtreeCost="0.001"><OutputList /><ConstantScan><Values><Row><ScalarOperator ScalarString="(9264)"><Const ConstValue="(9264)" /></ScalarOperator></Row></Values></ConstantScan></RelOp></Update>"#
        )
    } else {
        r#"<IndexScan Ordered="false" Storage="RowStore"><DefinedValues><DefinedValue><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="customer_id" /></DefinedValue></DefinedValues>
        <Object Database="[db]" Schema="[dbo]" Table="[orders]" Index="[orders_id]" IndexKind="Clustered" />
        <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="customer_id" /></RangeColumns><RangeExpressions><ScalarOperator ScalarString="(9264)"><Const ConstValue="(9264)" /></ScalarOperator></RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan>"#.into()
    };
    format!(
        r#"<ShowPlanXML Version="1.539" xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements>
        <StmtSimple StatementId="1" StatementText="{sql}" StatementType="{statement_type}" StatementSubTreeCost="0.1"><QueryPlan CachedPlanSize="16" CompileTime="1" CompileCPU="1" CompileMemory="128">
        <RelOp NodeId="1" PhysicalOp="{physical}" LogicalOp="{logical}" EstimateRows="1" EstimateIO="0.01" EstimateCPU="0.001" EstimatedTotalSubtreeCost="0.1" Parallel="false">
        <OutputList><ColumnReference Database="[db]" Schema="[dbo]" Table="[{table}]" Column="customer_id" /></OutputList>
        {access}</RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    )
}

fn original_sql(case: PageWriterCase, index: usize) -> &'static str {
    if index != 0 || case == PageWriterCase::ReadOnly {
        READ_SQL
    } else if case == PageWriterCase::OtherTarget {
        OTHER_SQL
    } else {
        WRITER_SQL
    }
}

fn move_page_point(page: &mut PageWaitObservation) {
    page.observed_from = "2020-01-01T12:00:02.100000000Z".into();
    page.observed_until = "2020-01-01T12:00:02.800000000Z".into();
    page.before_check.as_mut().unwrap().observed_at = "2020-01-01T12:00:02.200000000Z".into();
    page.after_check.as_mut().unwrap().observed_at = "2020-01-01T12:00:02.700000000Z".into();
    if let Some(PageIndexOperations::Observed { observed_at, .. }) = &mut page.index_operations {
        *observed_at = "2020-01-01T12:00:02.500000000Z".into();
    }
}

fn operational(page: PageWaitObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(page)),
    })
}

pub(super) fn retained(source: &CollectionDescription, rows: &[TelemetryRecord], sequence: u64) -> Recovery {
    Recovery::capture(
        "fixture:page-writers",
        &digest(b"page-writer-native-schema"),
        source.clone(),
        rows.to_vec(),
        sequence,
    )
}

pub(super) fn reference(recovery: &Recovery, record: &TelemetryRecord, ordinal: usize) -> DiagnosticEvidenceRef {
    let source = &recovery.manifest.source;
    let row = ExportedRecord::new(source, &recovery.manifest.capture, record.clone()).unwrap();
    DiagnosticEvidenceRef::from_export(source, &recovery.manifest.capture_manifest_sha256, &row, ordinal as u32)
        .unwrap()
}
