use super::*;
use crate::{DatabaseIdentity, Engine, MeasurementScope};
mod log_rates;

struct Original {
    source: CollectionDescription,
    row: ExportedRecord,
    focus: SourceReadFocus,
    execution: NativeSourceReadScope,
    episode: DiagnosticEpisode,
    prerequisite_source: CollectionDescription,
    prerequisite_rows: Vec<ExportedRecord>,
}

fn original() -> Original {
    original_in_epoch(None)
}

fn original_in_epoch(epoch: Option<&str>) -> Original {
    let mut context = super::super::native_read_context::tests::read_context();
    let mut database = DatabaseIdentity::server(Engine::Mssql, "sample", "owned.invalid", 1433, None);
    database.incarnation = Some("native-database-incarnation".into());
    context.target = database.into();
    if let Some(value) = epoch {
        let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
        *epoch = Some(value.into());
    }
    let execution = NativeSourceReadScope::from_context(&context).unwrap();
    let mut actor = super::super::activity::tests::request("42");
    actor.statement_identity = Some(NativeIdentity::new("original-native-statement"));
    actor.wait = Some(ActivityWait {
        class: Some(ActivityWaitClass::CompilationAdmission),
        native_type: Some("RESOURCE_SEMAPHORE_QUERY_COMPILE".into()),
        identified_resources: vec![],
        elapsed_ms: None,
        owners: vec![],
        resources: None,
    });
    let source = CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: "original-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: context.database_writable,
            epoch: context.partition.native_epoch().map(str::to_owned),
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            at: context.observed_until.clone(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed("original-activity".into(), 1, true),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-compiler-wait".into(),
        revision: Some(CaptureRevision {
            outbox_id: "owned-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let row = ExportedRecord::new(&source, &capture, TelemetryRecord::Activity(Box::new(actor.clone()))).unwrap();
    let basis = DiagnosticEvidenceRef::from_export(&source, &digest(b"original-sealed-manifest"), &row, 0).unwrap();
    let mut episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&source).unwrap(),
        DiagnosticSignal::CompilationAdmission { key: actor.key, statement_identity: actor.statement_identity },
        context.observed_until.clone(),
        basis.clone(),
    )
    .unwrap();
    // Independently retain the genuine focused waiter and native gateway answer. Neither becomes the
    // query-global history body or substitutes for the original Activity receipt.
    let TelemetryRecord::Activity(original_actor) = &row.record else { panic!() };
    let gateway = super::super::compiler_admission::tests::gateway("2", 3);
    let OperationalFact::CounterSet(gateway_set) = &gateway.fact else { panic!() };
    let waiter = CompilerWaiterObservation {
        observed_at: "2020-01-01T12:00:02Z".into(),
        request: original_actor.as_ref().clone(),
    };
    let waiter = OperationalObservation {
        source: gateway.source.clone(),
        key: waiter.key().unwrap(),
        fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
    };
    let result_source = CollectionDescription {
        observation: SourceObservation::FocusedCompilationAdmission {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CompilerAdmission,
                records: 2,
                observed_from: "2020-01-01T12:00:02Z".into(),
                observed_until: "2020-01-01T12:00:02Z".into(),
                sources: vec![StateSourceCoverage::observed(gateway.source.clone(), 2, false)],
            },
            result: Box::new(ActivityCompilationAdmissionFocusResult {
                focus: ActivityRequestFocus {
                    parent: basis.clone(),
                    target: source.target.clone(),
                    partition: source.partition.clone(),
                    database_writable: context.database_writable,
                    source: source.source.clone(),
                    epoch: context.partition.native_epoch().unwrap().into(),
                    original_observed_at: context.observed_until.clone(),
                    original: original_actor.as_ref().clone(),
                    statement_identity: original_actor.statement_identity.clone().unwrap(),
                },
                observed_from: "2020-01-01T12:00:02Z".into(),
                observed_until: "2020-01-01T12:00:02Z".into(),
                source: FocusSourceState::Matched,
                membership: CompilationAdmissionMembership::Observed,
                gateways: vec![gateway_set.identity.clone()],
            }),
        },
        source: gateway.source.clone(),
        ..source.clone()
    };
    let result_capture = CaptureIdentity { observation_id: "original-gateway-answer".into(), ..capture };
    let result_rows = [waiter, gateway]
        .into_iter()
        .map(|native| {
            ExportedRecord::new(&result_source, &result_capture, TelemetryRecord::Operational(native)).unwrap()
        })
        .collect::<Vec<_>>();
    let references = result_rows
        .iter()
        .enumerate()
        .map(|(ordinal, native)| {
            DiagnosticEvidenceRef::from_export(&result_source, &digest(b"gateway-manifest"), native, ordinal as u32)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let prerequisite = Box::new(CompilationAdmissionReport {
        observation: references[0].clone(),
        gateways: vec![references[1].clone()],
    });
    let report = DiagnosticReport::CompilationAdmission { report: prerequisite.clone() };
    assert!(report.matches_compiler_admission_input(
        &result_source,
        references.iter().zip(&result_rows).map(|(reference, row)| {
            let TelemetryRecord::Operational(native) = &row.record else { panic!() };
            (reference, native)
        })
    ));
    episode.report = Some(report);
    let focus = SourceReadFocus {
        basis: Box::new(basis),
        target: source.target.clone(),
        question: SourceReadQuestion::CompilationHistory,
        prerequisite: SourceReadPrerequisite::CompilationAdmission { report: prerequisite },
    };
    episode.analysis = DiagnosticAnalysisStatus::Partial;
    assert!(episode.is_valid());
    Original {
        source,
        row,
        focus,
        execution,
        episode,
        prerequisite_source: result_source,
        prerequisite_rows: result_rows,
    }
}

#[test]
fn original_native_owner_requires_real_database_incarnation_member_process_and_updateability() {
    let original = original();
    let context = NativeReadContext::from_activity(&original.source).unwrap();
    assert!(original.execution.matches_context(&context));
    for missing in ["database-incarnation", "member", "epoch", "updateability", "target"] {
        let mut changed = context.clone();
        match missing {
            "database-incarnation" => {
                let CollectionTarget::Database(database) = &mut changed.target else { panic!() };
                database.incarnation = None;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut changed.partition else { panic!() };
                *member = None;
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = None;
            }
            "updateability" => changed.database_writable = None,
            _ => changed.partition = SourcePartition::Target,
        }
        assert!(NativeSourceReadScope::from_context(&changed).is_err(), "{missing}");
    }
}

#[test]
fn scope_matches_native_database_aliases_without_borrowing_new_member_epoch_or_updateability() {
    let original = original();
    let mut context = NativeReadContext::from_activity(&original.source).unwrap();
    let CollectionTarget::Database(database) = &mut context.target else { panic!() };
    let crate::DatabaseLocation::Server { host, .. } = &mut database.location else { panic!() };
    *host = "another-owned-route.invalid".into();
    assert!(original.execution.matches_context(&context));
    context.database_writable = Some(false);
    assert!(!original.execution.matches_context(&context));
    context.database_writable = Some(true);
    let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
    *epoch = Some("replacement-process".into());
    assert!(!original.execution.matches_context(&context));
}

#[test]
fn source_question_has_no_synthetic_activity_and_always_requires_parent_authentication() {
    let original = original();
    let focus = DiagnosticFocus::SourceRead { focus: Box::new(original.focus.clone()) };
    assert!(focus.is_valid());
    assert!(focus.activity_participant().is_none());
    assert_eq!(focus.parent(), original.focus.basis.as_ref());
    assert_eq!(focus.references().count(), 3);
    let selection =
        CollectionSelection::SourceRead { focus: Box::new(original.focus), execution: Box::new(original.execution) };
    let request =
        CollectionRequest { subject: CollectionSubject::Database, selection, budget: CollectionBudget::default() };
    request.validate().unwrap();
    assert!(request.focus().is_none());
    assert!(request.selection.requires_authenticated_parent());
    assert_eq!(request.collection(), TelemetryCollection::Snapshot(SnapshotDataset::QueryCompilation));
}

#[test]
fn first_canonical_receipt_is_required_not_same_actor_in_another_capture_or_a_repaired_body() {
    let original = original();
    assert!(original.focus.matches_basis(&original.source, &original.row, &original.execution));
    let mut another = original.row.clone();
    another.capture.observation_id = "later-same-actor".into();
    assert!(!original.focus.matches_basis(&original.source, &another, &original.execution));
    let mut changed = original.row.clone();
    let TelemetryRecord::Activity(actor) = &mut changed.record else { panic!() };
    actor.key.request.as_mut().unwrap().generation = "recycled-request".into();
    assert!(!original.focus.matches_basis(&original.source, &changed, &original.execution));
    assert!(original.episode.plan_source_read_focus(&original.source, &changed).is_err());
}

#[test]
fn a_canonical_original_wait_cannot_grant_source_read_authority_to_a_known_prelogin_request() {
    for lifecycle in [SessionLifecycle::PoolResetPrelogin, SessionLifecycle::Connecting, SessionLifecycle::Unknown] {
        let mut original = original();
        let TelemetryRecord::Activity(actor) = &mut original.row.record else { panic!() };
        actor.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        let row = ExportedRecord::new(&original.source, &original.row.capture, original.row.record.clone()).unwrap();
        let basis =
            DiagnosticEvidenceRef::from_export(&original.source, &original.focus.basis.manifest_sha256, &row, 0)
                .unwrap();
        basis.validate_canonical_record(&original.source, &row.record).unwrap();
        *original.focus.basis = basis;
        assert!(original.focus.is_valid());
        assert_eq!(
            original.focus.matches_basis(&original.source, &row, &original.execution),
            lifecycle == SessionLifecycle::Unknown
        );
    }
}

#[test]
fn a_completed_gateway_answer_and_quiet_symptom_keep_separate_finite_source_debt() {
    let original = original();
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    assert_eq!(enrolled.report, original.episode.report);
    assert_eq!(enrolled.analysis, original.episode.analysis);
    let collecting = enrolled.transition_source_read(SourceReadStageState::Collecting).unwrap();
    let mut quiet = collecting.clone();
    quiet.lifecycle = DiagnosticLifecycle::Resolved;
    quiet.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: original.focus.basis.capture_ref() });
    assert!(quiet.is_valid());
    assert!(quiet.retained_in_checkpoint());
    let result = result_capture(&original);
    let captured = quiet.transition_source_read(SourceReadStageState::Captured { capture: result.clone() }).unwrap();
    assert!(!captured.retained_in_checkpoint());
    assert_eq!(captured.source_read_capture(), Some(&result));
    assert!(captured.transition_source_read(SourceReadStageState::Collecting).is_err());
    assert_eq!(captured.report, original.episode.report);
}

#[test]
fn revision_keeps_basis_after_latest_changes_and_refuses_erasing_stage_or_replacing_gateway_report() {
    let original = original();
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    let revision = DiagnosticRevision::between(Some(&original.episode), enrolled.clone(), "2020-01-01T12:00:02Z")
        .unwrap()
        .unwrap();
    assert!(revision.inputs.contains(&original.focus.basis));
    let mut removed = enrolled.clone();
    removed.source_read = None;
    assert!(DiagnosticRevision::between(Some(&enrolled), removed, "2020-01-01T12:00:03Z").is_err());
    let mut replaced = enrolled.clone();
    replaced.analysis = DiagnosticAnalysisStatus::Analyzed;
    assert!(DiagnosticRevision::between(Some(&enrolled), replaced, "2020-01-01T12:00:03Z").is_err());
}

fn description(original: &Original, until: &str) -> CollectionDescription {
    let context = NativeReadContext {
        target: original.source.target.clone(),
        partition: original.execution.partition.clone(),
        database_writable: Some(true),
        observed_from: "2020-01-01T12:00:02Z".into(),
        observed_until: until.into(),
    };
    CollectionDescription {
        target: original.source.target.clone(),
        partition: SourcePartition::Target,
        source: "query-compilation".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::SourceRead {
            focus_key: original.focus.key().unwrap(),
            execution: Box::new(context),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::QueryCompilation,
                records: 0,
                observed_from: "2020-01-01T12:00:02Z".into(),
                observed_until: until.into(),
                sources: vec![StateSourceCoverage::observed("query-compilation".into(), 0, false)],
            },
        },
    }
}

#[test]
fn typed_result_keeps_logical_counter_scope_separate_from_qualified_execution_updateability() {
    let original = original();
    let source = description(&original, "2020-01-01T12:00:03Z");
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        budget: CollectionBudget::default(),
        selection: CollectionSelection::SourceRead {
            focus: Box::new(original.focus.clone()),
            execution: Box::new(original.execution.clone()),
        },
    };
    assert!(source.matches_request(&request));
    assert!(source.observation.covers_request());
    let dataset_context = NativeReadContext::from_snapshot(&source).unwrap();
    assert_eq!(dataset_context.partition, SourcePartition::Target);
    assert_eq!(dataset_context.database_writable, None);
    assert!(!dataset_context.has_qualified_local_owner());
    assert!(original.execution.matches_context(source.observation.execution_context().unwrap()));
    let mut wrong = source.clone();
    wrong.partition = original.execution.partition.clone();
    assert!(!wrong.matches_request(&request), "execution scope cannot replace query-global data namespace");
    let mut wrong = source.clone();
    let SourceObservation::SourceRead { execution, .. } = &mut wrong.observation else { panic!() };
    execution.database_writable = Some(false);
    assert!(!wrong.matches_request(&request));
    let mut ordinary = source;
    ordinary.observation = SourceObservation::Snapshot(ordinary.observation.snapshot().unwrap().clone());
    assert!(!ordinary.matches_request(&request), "an ordinary snapshot cannot impersonate an admitted result");
}

#[test]
fn final_native_execution_must_match_the_provisional_owner_and_preserve_ordered_native_bounds() {
    let original = original();
    let completed = description(&original, "2020-01-01T12:00:03Z");
    let SourceObservation::SourceRead { snapshot, focus_key, execution } = &completed.observation else { panic!() };
    let mut provisional = completed.clone();
    provisional.observation = SourceObservation::PagedSourceRead {
        snapshot: PagedSnapshotDescription {
            dataset: snapshot.dataset,
            observed_from: snapshot.observed_from.clone(),
            sources: vec!["query-compilation".into()],
        },
        focus_key: focus_key.clone(),
        execution: Box::new(NativeReadContext {
            observed_until: execution.observed_from.clone(),
            ..*execution.clone()
        }),
    };
    assert!(provisional.completed(SourceCompletion::Verified).is_err());
    assert_eq!(provisional.completed(SourceCompletion::Changed).unwrap(), None);
    assert_eq!(
        provisional
            .completed(SourceCompletion::SourceRead { snapshot: snapshot.clone(), execution: execution.clone() })
            .unwrap(),
        Some(completed.clone())
    );
    for defect in ["epoch", "updateability", "clock"] {
        let mut current = execution.clone();
        match defect {
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut current.partition else { panic!() };
                *epoch = Some("new-process".into());
            }
            "updateability" => current.database_writable = None,
            _ => current.observed_until = "2020-01-01T12:00:01Z".into(),
        }
        assert!(
            provisional
                .completed(SourceCompletion::SourceRead { snapshot: snapshot.clone(), execution: current })
                .is_err(),
            "{defect}"
        );
    }
}

fn result_capture(original: &Original) -> DiagnosticCaptureRef {
    DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "completed-compilation-history".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "3".into(),
            }),
        },
        manifest_sha256: digest(b"actual-completed-source-read-manifest"),
        source_scope_id: description(original, "2020-01-01T12:00:03Z").captured_range().unwrap().scope_id,
    }
}

fn recipe(original: &Original) -> (RecipeDescriptor, crate::ComponentVersion) {
    let component = crate::ComponentVersion::package("fixture-source-read", "1", Some("fixture-revision"));
    let receipt = RecipeCostReceipt {
        recipe_id: "fixture.compilation-history".into(),
        recipe_version: 1,
        selector: SourceReadQuestion::CompilationHistory.selector(),
        scope: RecipeCostScope::NativeRead(original.execution.clone()),
        component: component.clone(),
        tested_budget: CollectionBudget::default(),
        // This is structurally separate retained evidence. A selected provider's authenticated native-cost
        // verifier is still mandatory; these pure fixture relations never qualify a production source.
        evidence: original.episode.report.as_ref().unwrap().evidence().clone(),
        read_metrics: ReadWorkMetrics {
            native_session_work: None,
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"owned-fixture-template"),
                statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                completed_tasks: 1,
                failures: 0,
                timeouts: 0,
                cancellations: 0,
                unconfirmed_cancellations: 0,
                stop_causes: vec![],
                rows_decoded: "1".into(),
                charged_value_bytes: "100".into(),
                client_elapsed_ms: 1.0,
            }],
        },
    };
    (
        RecipeDescriptor {
            id: receipt.recipe_id.clone(),
            version: receipt.recipe_version,
            purpose: RecipePurpose::Investigation,
            selector: receipt.selector,
            subjects: vec![RecipeSubject::Database],
            budget: CollectionBudget::default(),
            cost: RecipeCostQualification::Measured(Box::new(receipt)),
        },
        component,
    )
}

#[test]
fn finite_source_admission_is_question_specific_and_requires_exact_native_cost_scope() {
    let original = original();
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    let (recipe, component) = recipe(&original);
    let intent = DiagnosticInvestigationIntent::new_source_read(
        &enrolled,
        &recipe,
        original.focus.clone(),
        original.execution.clone(),
        &component,
        &CollectionBudget::default(),
    )
    .unwrap();
    assert!(intent.is_valid());
    assert!(intent.matches_parent_record(&original.source, &original.row));
    assert!(intent.matches_admission(&enrolled, &recipe, &component, &CollectionBudget::default()));
    assert!(intent.matches_result_source(&description(&original, "2020-01-01T12:00:03Z")));
    let terminal = enrolled
        .transition_source_read(SourceReadStageState::Stopped { issue: DiagnosticAnalysisIssue::SourceChanged })
        .unwrap();
    assert!(!intent.matches_admission(&terminal, &recipe, &component, &CollectionBudget::default()));
    for selector in [
        RecipeSelector::FocusedCompilationAdmission,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::CachedRuntime)),
    ] {
        let mut other = recipe.clone();
        other.selector = selector;
        let RecipeCostQualification::Measured(receipt) = &mut other.cost else { panic!() };
        receipt.selector = selector;
        assert!(!receipt.is_valid(), "no generic snapshot or Activity approval follows from this NativeRead scope");
    }
    let mut changed = intent;
    let DiagnosticExecutionBinding::NativeRead { execution } = &mut changed.execution else { panic!() };
    execution.database_writable = false;
    assert!(!changed.is_valid());
}

#[test]
fn source_completion_retains_one_actual_result_capture_and_never_completes_the_waiter_question() {
    let original = original();
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    let (recipe, component) = recipe(&original);
    let intent = DiagnosticInvestigationIntent::new_source_read(
        &enrolled,
        &recipe,
        original.focus.clone(),
        original.execution.clone(),
        &component,
        &CollectionBudget::default(),
    )
    .unwrap();
    let collecting = enrolled.transition_source_read(SourceReadStageState::Collecting).unwrap();
    let result = result_capture(&original);
    let completed =
        collecting.complete_source_read(&intent, &description(&original, "2020-01-01T12:00:03Z"), &result).unwrap();
    assert_eq!(completed.source_read_capture(), Some(&result));
    assert_eq!(completed.analysis, original.episode.analysis);
    assert_eq!(completed.report, original.episode.report);
    assert!(collecting.complete_source_read(&intent, &original.source, &original.focus.basis.capture_ref()).is_err());
    let mut wrong = result.clone();
    wrong.source_scope_id = digest(b"foreign-result-scope");
    assert!(collecting.complete_source_read(&intent, &description(&original, "2020-01-01T12:00:03Z"), &wrong).is_err());
}

#[test]
fn a_fresh_activity_route_can_prove_the_same_execution_owner_but_not_a_logical_snapshot_or_changed_updateability() {
    let original = original();
    let expected = RecipeCostScope::NativeRead(original.execution.clone());
    assert!(expected.matches_source(&original.source));
    for defect in ["updateability", "unknown-updateability", "unknown-owner", "process", "database"] {
        let mut current = original.source.clone();
        let SourceObservation::Activity(frame) = &mut current.observation else { panic!() };
        match defect {
            "updateability" => frame.database_writable = Some(false),
            "unknown-updateability" => frame.database_writable = None,
            "unknown-owner" => current.partition = SourcePartition::Target,
            "process" => {
                frame.epoch = Some("new-native-process".into());
                let SourcePartition::InstanceLocal { epoch, .. } = &mut current.partition else { panic!() };
                *epoch = frame.epoch.clone();
            }
            _ => {
                let CollectionTarget::Database(target) = &mut current.target else { panic!() };
                target.incarnation = Some("another-native-database".into());
            }
        }
        assert!(!expected.matches_source(&current), "{defect}");
    }
    let mut logical = description(&original, "2020-01-01T12:00:03Z");
    logical.observation = SourceObservation::Snapshot(logical.observation.snapshot().unwrap().clone());
    assert!(!expected.matches_source(&logical));
}

#[test]
fn enrolling_separate_source_debt_cannot_reopen_a_partially_answered_gateway_question() {
    let original = original();
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    let SourceObservation::Activity(frame) = &original.source.observation else { panic!() };
    let TelemetryRecord::Activity(actor) = &original.row.record else { panic!() };
    let gateway = DiagnosticFocus::CompilationAdmission {
        focus: Box::new(ActivityRequestFocus {
            parent: original.focus.basis.as_ref().clone(),
            target: original.source.target.clone(),
            partition: original.source.partition.clone(),
            database_writable: frame.database_writable,
            source: original.source.source.clone(),
            epoch: frame.epoch.clone().unwrap(),
            original_observed_at: frame.at.clone(),
            original: actor.as_ref().clone(),
            statement_identity: actor.statement_identity.clone().unwrap(),
        }),
    };
    assert!(enrolled.matches_focus(&gateway));
    assert_eq!(enrolled.analysis, DiagnosticAnalysisStatus::Partial);
    assert!(!enrolled.needs_question(&gateway));
    assert_eq!(
        enrolled.plan_focus(&original.source, actor, &original.focus.basis).unwrap(),
        DiagnosticFocusPlan::NotRequired
    );
}

fn prerequisite_rows(original: &Original) -> Vec<(&DiagnosticEvidenceRef, &OperationalObservation)> {
    original
        .focus
        .prerequisite
        .references()
        .zip(&original.prerequisite_rows)
        .map(|(reference, row)| {
            let TelemetryRecord::Operational(native) = &row.record else { panic!() };
            (reference, native)
        })
        .collect()
}

#[test]
fn exact_closed_gateway_prerequisite_cannot_be_missing_foreign_or_rebound_to_a_same_id_request() {
    let original = original();
    assert!(original.focus.matches_prerequisite_input(
        &original.source,
        &original.row,
        &original.prerequisite_source,
        prerequisite_rows(&original)
    ));
    assert!(!original.focus.matches_prerequisite_input(
        &original.source,
        &original.row,
        &original.prerequisite_source,
        []
    ));
    let foreign = original_in_epoch(Some("another-native-process"));
    let mut rebound = original.focus.clone();
    rebound.prerequisite = foreign.focus.prerequisite.clone();
    assert!(rebound.is_valid());
    assert!(
        rebound
            .prerequisite
            .compilation_admission()
            .unwrap()
            .matches_input(&foreign.prerequisite_source, prerequisite_rows(&foreign))
    );
    assert!(!rebound.matches_prerequisite_input(
        &original.source,
        &original.row,
        &foreign.prerequisite_source,
        prerequisite_rows(&foreign)
    ));
    let mut changed = original.focus.clone();
    let SourceReadPrerequisite::CompilationAdmission { report } = &mut changed.prerequisite else { panic!() };
    report.observation.payload_sha256 = digest(b"different-valid-digest");
    assert!(changed.is_valid());
    assert!(!changed.matches_prerequisite_input(
        &original.source,
        &original.row,
        &original.prerequisite_source,
        prerequisite_rows(&original)
    ));
    let enrolled = original.episode.enroll_source_read(original.focus.clone()).unwrap();
    let mut replaced = enrolled.clone();
    replaced.source_read.as_mut().unwrap().focus.prerequisite = changed.prerequisite;
    assert!(!replaced.is_valid(), "the flat prerequisite is byte-bound to the original gateway report");
}

#[test]
fn restored_admitted_debt_does_not_grant_a_replacement_source_read() {
    let original = original();
    let collecting = original
        .episode
        .enroll_source_read(original.focus.clone())
        .unwrap()
        .transition_source_read(SourceReadStageState::Collecting)
        .unwrap();
    assert!(collecting.has_owed_question());
    assert!(!collecting.source_read.as_ref().unwrap().needs_admission());
    assert!(matches!(
        collecting.plan_source_read_focus(&original.source, &original.row).unwrap(),
        DiagnosticFocusPlan::NotRequired
    ));
}
