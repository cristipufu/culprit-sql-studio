use super::*;

struct OriginalFrame {
    source: CollectionDescription,
    rows: Vec<ActivityObservation>,
    references: Vec<DiagnosticEvidenceRef>,
    original: ActivityRequestFocus,
}

impl OriginalFrame {
    fn new(page: &mut PageWaitObservation) -> Self {
        page.request.statement_identity = page.statement_identity.clone();
        page.request.wait.as_mut().unwrap().owners =
            vec![ActivityWaitOwner::Session { id: "57".into(), generation: Some("original-login".into()) }];
        let waiter = page.request.clone();
        let owners = (0..2).map(|index| {
            let mut owner = waiter.clone();
            owner.key.session = NativeActivityIdentity { id: "57".into(), generation: "original-login".into() };
            owner.key.request.as_mut().unwrap().id = index.to_string();
            owner.state = "running".into();
            owner.command = Some(if index == 0 { "INSERT" } else { "SELECT" }.into());
            owner.wait = None;
            owner.statement_identity = Some(NativeIdentity::new(format!("original-owner-statement-{index}")));
            owner.statement = DefinitionPayload::sql("private owner SQL WHERE secret=8821", Engine::Mssql);
            owner.request_counters.as_mut().unwrap().cpu_ms = Some("9007199254740993".into());
            owner
        });
        let rows = std::iter::once(waiter.clone()).chain(owners).collect::<Vec<_>>();
        let source = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-machine"),
                    native: NativeIdentity::new("native-member"),
                }),
                epoch: Some("original-epoch".into()),
            },
            source: "original-activity".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: Some("original-epoch".into()),
                database_writable: Some(true),
                observed_from: "2026-11-13T10:00:00Z".into(),
                observed_until: "2026-11-13T10:00:00.05Z".into(),
                at: "2026-11-13T10:00:00.025Z".into(),
                scope: culprit_core::MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: rows.len() as u32,
                observations: rows.len() as u32,
                coverage: StateSourceCoverage::collected("original-activity".into(), rows.len() as u32),
            }),
        };
        let capture = CaptureIdentity {
            observation_id: "original-activity".into(),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"outbox"),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let references = rows
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::from(row.clone())).unwrap();
                DiagnosticEvidenceRef::from_export(&source, &digest(b"manifest"), &exported, ordinal as u32).unwrap()
            })
            .collect::<Vec<_>>();
        let original = ActivityRequestFocus {
            parent: references[0].clone(),
            target: source.target.clone(),
            partition: source.partition.clone(),
            database_writable: Some(true),
            source: source.source.clone(),
            epoch: "original-epoch".into(),
            original_observed_at: "2026-11-13T10:00:00.025Z".into(),
            statement_identity: waiter.statement_identity.clone().unwrap(),
            original: waiter,
        };
        Self { source, rows, references, original }
    }

    fn borrowed(&self) -> Vec<ActivityFrameRow<'_>> {
        self.rows
            .iter()
            .zip(&self.references)
            .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect()
    }

    fn page_source(&self, page: &PageWaitObservation) -> CollectionDescription {
        CollectionDescription {
            target: self.source.target.clone(),
            partition: self.source.partition.clone(),
            source: page.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitTargets {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: page.observed_from.clone(),
                    observed_until: page.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
                },
                database_writable: None,
                original_activity: None,
            },
        }
    }
}

#[test]
fn authenticated_original_frame_exposes_all_mars_commands_as_historical_session_context_without_sql_or_costs() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let original = OriginalFrame::new(&mut page);
    let frame = ActivityFrameEvidence::new(&original.source, &original.borrowed(), true).unwrap();
    let page_source = original.page_source(&page);
    let report = explain(
        &[&page],
        Some(&source(SourceState::Partial)),
        Some(PageWaitActivityEvidence { source: &page_source, original: &original.original, frame: &frame }),
    )
    .unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.blocking.contexts.len(), 2);
    assert_eq!(entry.blocking.references().cloned().collect::<Vec<_>>(), original.references[1..]);
    assert_eq!(entry.blocking.contexts[0].command.as_deref(), Some("INSERT"));
    assert_eq!(entry.blocking.contexts[1].command.as_deref(), Some("SELECT"));
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.historical")));
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.multiple_requests")));
    let encoded = serde_json::to_string(&report).unwrap();
    for raw in ["private owner SQL", "secret=8821", "9007199254740993", "private-app-label", "SELECT payload"] {
        assert!(!encoded.contains(raw), "{raw}");
    }
    assert_eq!(entry.blocking.basis.as_ref().unwrap().waiter.evidence.as_ref(), Some(&original.original.parent));
    assert_eq!(report.source_state, Some(SourceState::Partial));
}

#[test]
fn direct_page_sample_without_original_activity_does_not_reconstruct_blocker_context_from_waiter_sql() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let _original = OriginalFrame::new(&mut page);
    let report = explain(&[&page], Some(&source(SourceState::Partial)), None).unwrap();
    let entry = &report.entries[0];
    assert!(entry.blocking.contexts.is_empty());
    assert!(entry.blocking.basis.is_none());
    assert_eq!(entry.blocking.issues, [PageWaitBlockingIssue::FrameUnavailable]);
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.frame_unavailable")));
    assert_eq!(entry.request_key, page.request.key);
}
