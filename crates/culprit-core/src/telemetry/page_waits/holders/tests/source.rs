use super::*;

fn description(focus: &PageTaskHolderFocus, context: &NativeReadContext) -> CollectionDescription {
    let NativeRecordIdentity::Operational { source, .. } = &focus.page.native_key else { panic!("native page") };
    CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageTaskHolders {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageTaskHolders,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(source.clone(), 1, false)],
            },
            focus: Box::new(focus.clone()),
            database_writable: context.database_writable,
        },
    }
}

fn request(focus: &PageTaskHolderFocus) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::PageTaskHolders { focus: Box::new(focus.clone()) },
        budget: CollectionBudget::default(),
    }
}

fn record(source: &CollectionDescription, row: PageTaskHolderObservation) -> Result<TelemetryRecord> {
    Ok(TelemetryRecord::Operational(OperationalObservation {
        source: source.observation.snapshot().unwrap().sources[0].source.clone(),
        key: row.key()?,
        fact: OperationalFact::PageTaskHolders(Box::new(row)),
    }))
}

fn export(source: &CollectionDescription, row: PageTaskHolderObservation) -> Result<ExportedRecord> {
    ExportedRecord::new(
        source,
        &CaptureIdentity { observation_id: "selected-task-holder".into(), revision: None },
        record(source, row)?,
    )
}

#[test]
fn exact_holder_source_selection_and_export_preserve_original_receipts_and_all_statement_payloads() {
    let (focus, context, row) = fixture();
    let source = description(&focus, &context);
    let request = request(&focus);
    assert!(source.matches_request(&request));
    assert!(source.observation.covers_request());
    assert_eq!(NativeReadContext::from_snapshot(&source).unwrap(), context);
    let question = DiagnosticFocus::from_selection(&request.selection).unwrap();
    assert_eq!(question.selector(), RecipeSelector::FocusedPageTaskHolders);
    assert_eq!(question.references().collect::<Vec<_>>(), [&focus.participant.parent, &focus.page]);
    assert!(question.page_wait().is_none(), "a parallel task is not a fabricated coordinator wait");
    assert_eq!(question.database_selection(), request.selection);
    assert_eq!(source.observation.diagnostic_focus(), Some(question));
    let exported = export(&source, row).unwrap();
    let payloads = exported.record.definition_payloads().map(|field| field.pointer.into_owned()).collect::<Vec<_>>();
    assert_eq!(
        payloads,
        [
            "/record/data/fact/data/page/request/statement",
            "/record/data/fact/data/before/holder/request/statement",
            "/record/data/fact/data/after/holder/request/statement"
        ]
    );
}

#[test]
fn plain_snapshot_shared_scope_wrong_role_owner_clock_or_source_cannot_admit_holder_rows() {
    let (focus, context, row) = fixture();
    let source = description(&focus, &context);
    let mut plain = request(&focus);
    plain.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::PageTaskHolders };
    assert!(plain.validate().is_err());
    assert!(!source.matches_request(&plain));
    assert!(!RecipeSelector::Collection(request(&focus).collection()).matches(&request(&focus)));
    for field in ["target", "member", "epoch", "role", "complete", "count", "clock", "native-source", "generic"] {
        let mut changed = source.clone();
        match field {
            "target" => {
                changed.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "other", "fixture.invalid", 1433, None).into()
            }
            "member" => {
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition {
                    member.native.id = "other".into();
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition {
                    *epoch = Some("other".into());
                }
            }
            "generic" => {
                changed.observation = SourceObservation::Snapshot(changed.observation.snapshot().unwrap().clone())
            }
            _ => {
                if let SourceObservation::PageTaskHolders { snapshot, database_writable, .. } = &mut changed.observation
                {
                    match field {
                        "role" => *database_writable = None,
                        "complete" => {
                            snapshot.sources[0] = StateSourceCoverage::collected(snapshot.sources[0].source.clone(), 1)
                        }
                        "count" => {
                            snapshot.records = 0;
                            snapshot.sources[0].records = 0;
                        }
                        "native-source" => snapshot.sources[0].source = "unrelated-native-source".into(),
                        _ => snapshot.observed_from = "2026-11-13T09:00:00Z".into(),
                    }
                }
            }
        }
        assert!(!changed.matches_request(&request(&focus)), "{field}");
        assert!(export(&changed, row.clone()).is_err(), "{field}");
    }
}

#[test]
fn logical_recipe_names_do_not_replace_the_declared_native_page_and_holder_sources() {
    let (focus, context, row) = fixture();
    let mut holder_source = description(&focus, &context);
    holder_source.source = "logical-holder-recipe".into();
    assert!(holder_source.partition_is_consistent());
    assert!(holder_source.matches_request(&request(&focus)));
    let exported = export(&holder_source, row.clone()).unwrap();
    let TelemetryRecord::Operational(native) = &exported.record else { panic!("native holder row") };
    assert_eq!(native.source, holder_source.observation.snapshot().unwrap().sources[0].source);
    assert_ne!(native.source, holder_source.source);

    let mut undeclared = native.clone();
    undeclared.source = holder_source.source.clone();
    let OperationalFact::PageTaskHolders(point) = &mut undeclared.fact else { panic!("native holder row") };
    point.page.as_mut().unwrap().source = undeclared.source.clone();
    assert!(undeclared.is_valid(), "the row is independently valid but absent from native coverage");
    assert!(
        ExportedRecord::new(&holder_source, &exported.capture, TelemetryRecord::Operational(undeclared)).is_err(),
        "the logical namespace cannot grant native row membership"
    );

    let page = row.page.unwrap();
    let mut page_source = holder_source.clone();
    page_source.source = "logical-page-recipe".into();
    let mut snapshot = page_source.observation.snapshot().unwrap().clone();
    snapshot.dataset = SnapshotDataset::PageWaitTargets;
    page_source.observation = SourceObservation::PageWaitTargets {
        snapshot,
        database_writable: context.database_writable,
        original_activity: None,
    };
    assert!(page_source.partition_is_consistent());
    assert!(page.fits_source_scope(&page_source));
    let native = OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(page),
    };
    assert!(ExportedRecord::new(&page_source, &exported.capture, TelemetryRecord::Operational(native.clone())).is_ok());
    let mut undeclared = native;
    undeclared.source = page_source.source.clone();
    let OperationalFact::PageWait(page) = &mut undeclared.fact else { panic!("native page row") };
    page.source = undeclared.source.clone();
    undeclared.key = page.key().unwrap();
    assert!(undeclared.is_valid(), "a well-formed row still needs native coverage membership");
    assert!(
        ExportedRecord::new(&page_source, &exported.capture, TelemetryRecord::Operational(undeclared)).is_err(),
        "an ordinary page row also requires its exact declared native source"
    );
}

fn with_target_state(mut row: PageTaskHolderObservation, state: PageWriterTargetState) -> PageTaskHolderObservation {
    row.target_state = state;
    match state {
        PageWriterTargetState::NoLongerObserved => {
            row.after.request = PageWaitRequestState::Missing;
            row.after.wait = PageWaitRelation::Unqualified;
            row.after_task = None;
            row.after.holder = MssqlPageTaskHolder {
                relation: PageTaskHolderRelation::Unavailable,
                matching_owner_rows: 0,
                task: None,
                request: None,
            };
        }
        PageWriterTargetState::ChangedTarget => {
            row.final_target
                .as_mut()
                .unwrap()
                .target
                .object
                .as_mut()
                .unwrap()
                .native
                .qualifiers
                .insert("modified_without_offset".into(), "2021-01-01T00:00:00.0000000".into());
        }
        _ => {}
    }
    row
}

#[test]
fn every_holder_target_state_rejects_a_foreign_request_database_but_keeps_unknown_labels() {
    let (focus, context, original) = fixture();
    let source = description(&focus, &context);
    for state in [
        PageWriterTargetState::CurrentMapped,
        PageWriterTargetState::NoLongerObserved,
        PageWriterTargetState::ChangedTarget,
        PageWriterTargetState::Unavailable,
    ] {
        let mut row = with_target_state(original.clone(), state);
        assert!(focus.accepts_observation(&context, &row), "{state:?}");
        row.page.as_mut().unwrap().request.database = None;
        assert!(focus.accepts_observation(&context, &row), "a missing native label stays unknown: {state:?}");
        assert!(export(&source, row.clone()).is_ok(), "{state:?}");
        row.page.as_mut().unwrap().request.database = Some("foreign-database".into());
        assert!(row.is_valid(), "the point itself is structurally valid: {state:?}");
        assert!(!focus.accepts_observation(&context, &row), "{state:?}");
        assert!(export(&source, row).is_err(), "{state:?}");
    }
}

#[test]
fn all_holder_target_states_refuse_genuinely_bound_shared_first_and_final_mappings() {
    use super::super::super::tests::shared_target;
    let (focus, context, original) = fixture();
    let source = description(&focus, &context);
    for state in [
        PageWriterTargetState::CurrentMapped,
        PageWriterTargetState::NoLongerObserved,
        PageWriterTargetState::ChangedTarget,
        PageWriterTargetState::Unavailable,
    ] {
        let control = with_target_state(original.clone(), state);
        assert!(focus.accepts_observation(&context, &control), "{state:?}");
        for bound in ["first", "final"] {
            let mut row = control.clone();
            if bound == "first" {
                let page = row.page.as_mut().unwrap();
                page.target = Some(Box::new(shared_target(
                    page.target.as_ref().unwrap(),
                    &page.observed_from,
                    &page.observed_until,
                )));
                assert!(page.is_valid(), "a genuinely valid shared page: {state:?}");
            } else {
                let check = row.final_target.as_mut().unwrap();
                *check.target = shared_target(&check.target, &check.observed_at, &check.observed_at);
                assert!(check.target.is_valid(), "the independent final shared mapping is valid: {state:?}");
            }
            assert!(!row.is_valid(), "the database-only profile rejects {bound} shared mapping: {state:?}");
            assert!(!focus.accepts_observation(&context, &row), "{bound}: {state:?}");
            assert!(export(&source, row).is_err(), "{bound}: {state:?}");
        }
    }
}

#[test]
fn a_changed_pointer_wrong_fact_or_invalid_original_payload_fails_before_sharing_can_repair_it() {
    let (focus, context, row) = fixture();
    let source = description(&focus, &context);
    let mut changed = row.clone();
    changed.after_task.as_mut().unwrap().blocking_session_id = Some(99);
    assert!(export(&source, changed).is_err());
    let mut invalid = row.clone();
    invalid.before.holder.request.as_mut().unwrap().statement = DefinitionPayload::Available {
        text: "SELECT 'private-invalid-original'".into(),
        sha256: digest(b"different"),
        redaction_version: REDACTION_VERSION,
    };
    assert!(export(&source, invalid).is_err());
    let mut wrong = source.clone();
    wrong.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: context.observed_from,
        observed_until: context.observed_until,
        records: 1,
        sources: vec![StateSourceCoverage::observed(wrong.source.clone(), 1, false)],
    });
    assert!(export(&wrong, row).is_err(), "an ordinary operational capsule cannot borrow holder focus authority");
}
