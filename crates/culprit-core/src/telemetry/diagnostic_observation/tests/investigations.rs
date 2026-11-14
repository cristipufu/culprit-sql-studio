use super::*;
use crate::ComponentVersion;

struct Scenario {
    episode: DiagnosticEpisode,
    focus: ActivityWaitFocus,
    recipe: RecipeDescriptor,
    component: ComponentVersion,
    cost_source: CollectionDescription,
    cost_row: ExportedRecord,
}

impl Scenario {
    fn intent(&self) -> DiagnosticInvestigationIntent {
        DiagnosticInvestigationIntent::new(
            &self.episode,
            &self.recipe,
            self.focus.clone(),
            &self.component,
            &CollectionBudget::default(),
        )
        .unwrap()
    }
    fn receipt(&mut self) -> &mut RecipeCostReceipt {
        let RecipeCostQualification::Measured(receipt) = &mut self.recipe.cost else { panic!() };
        receipt
    }
}

fn scenario() -> Scenario {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    let episode = observed.checkpoint.episodes[0].clone();
    let focus = observed.focuses[0].1.clone();
    // These are source-owned fixture facts. Core validates their relations/accounting; only the selected source's
    // verifier can authenticate this artifact and decide whether its native work qualifies the recipe.
    let cost_source = CollectionDescription {
        target: baseline.0.target.clone(),
        source: "fixture-native-cost".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected("fixture-native-cost".into(), 1)],
        }),
    };
    let cost_capture = CaptureIdentity {
        observation_id: "native-cost-observation".into(),
        revision: Some(CaptureRevision {
            outbox_id: "owned-cost-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let cost = NativeCounterSet {
        identity: NativeIdentity::new("owned-native-observer"),
        scope: MeasurementScope::Database,
        counters: vec![NativeCounterSample::gauge(
            "logical_reads",
            12,
            CounterUnit::Count,
            MeasurementScope::Database,
            "native-observer",
            "logical_reads",
        )],
        properties: Default::default(),
    };
    let cost_row = ExportedRecord::new(
        &cost_source,
        &cost_capture,
        TelemetryRecord::Operational(OperationalObservation {
            source: cost_source.source.clone(),
            key: cost.key().unwrap(),
            fact: OperationalFact::CounterSet(Box::new(cost)),
        }),
    )
    .unwrap();
    let evidence =
        DiagnosticEvidenceRef::from_export(&cost_source, &digest(b"immutable-native-cost-manifest"), &cost_row, 0)
            .unwrap();
    let component = ComponentVersion::package("fixture-native-reader", "1.0.0", Some("native-reader-build-42"));
    let receipt = RecipeCostReceipt {
        recipe_id: "fixture.page-wait-focus".into(),
        recipe_version: 1,
        scope: episode.scope.clone(),
        component: component.clone(),
        tested_budget: CollectionBudget {
            page_rows: 1,
            max_snapshot_rows: 1,
            max_queries: 3,
            statement_timeout_ms: 1_000,
            total_timeout_ms: 5_000,
            ..Default::default()
        },
        evidence,
        read_metrics: ReadWorkMetrics {
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"bounded-native-metadata-template"),
                statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                completed_tasks: 2,
                failures: 0,
                timeouts: 0,
                cancellations: 0,
                unconfirmed_cancellations: 0,
                rows_decoded: "2".into(),
                charged_value_bytes: "4096".into(),
                client_elapsed_ms: 1.25,
            }],
        },
    };
    let recipe = RecipeDescriptor {
        id: receipt.recipe_id.clone(),
        version: receipt.recipe_version,
        purpose: RecipePurpose::Investigation,
        selector: RecipeSelector::FocusedPageWait,
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Measured(Box::new(receipt)),
    };
    Scenario { episode, focus, recipe, component, cost_source, cost_row }
}

#[test]
fn an_immutable_question_preserves_its_parent_tested_budget_and_independent_native_cost_reference() {
    let scenario = scenario();
    let intent = scenario.intent();
    assert!(intent.is_valid());
    assert!(intent.matches_admission(
        &scenario.episode,
        &scenario.recipe,
        &scenario.component,
        &CollectionBudget::default()
    ));
    assert_eq!(intent.focus, scenario.focus);
    assert_ne!(intent.qualification.evidence.capture, intent.focus.parent.capture);
    assert_eq!(intent.budget, intent.qualification.tested_budget);
    assert_eq!(intent.episode_revision, scenario.episode.revision);
    assert_eq!(
        DiagnosticEvidenceRef::from_export(
            &scenario.cost_source,
            &intent.qualification.evidence.manifest_sha256,
            &scenario.cost_row,
            intent.qualification.evidence.row_ordinal,
        )
        .unwrap(),
        intent.qualification.evidence,
        "artifact authentication and native-cost interpretation still belong to the selected source's verifier",
    );
    let restored: DiagnosticInvestigationIntent =
        serde_json::from_slice(&serde_json::to_vec(&intent).unwrap()).unwrap();
    assert_eq!(restored, intent);
    assert_eq!(restored.key().unwrap(), intent.key().unwrap());
}

#[test]
fn package_version_alone_and_unqualified_routes_cannot_construct_investigation_intents() {
    let mut unqualified = scenario();
    unqualified.recipe.cost = RecipeCostQualification::Unqualified;
    assert!(unqualified.recipe.is_valid());
    assert!(
        DiagnosticInvestigationIntent::new(
            &unqualified.episode,
            &unqualified.recipe,
            unqualified.focus.clone(),
            &unqualified.component,
            &CollectionBudget::default(),
        )
        .is_err()
    );
    let mut scenario = scenario();
    scenario.component.revision = None;
    assert!(!qualification_component_is_valid(&scenario.component));
    let component = scenario.component.clone();
    scenario.receipt().component = component;
    assert!(!scenario.recipe.is_valid());
}

#[test]
fn exact_receipt_digest_prevents_float_equality_from_substituting_different_accounting_bytes() {
    let mut scenario = scenario();
    scenario.receipt().read_metrics.templates[0].client_elapsed_ms = 0.0;
    let intent = scenario.intent();
    let mut changed = scenario.recipe.clone();
    let RecipeCostQualification::Measured(receipt) = &mut changed.cost else { panic!() };
    receipt.read_metrics.templates[0].client_elapsed_ms = -0.0;
    assert_eq!(receipt.as_ref(), &intent.qualification, "numeric equality is not receipt identity");
    assert_ne!(receipt.key().unwrap(), intent.qualification.key().unwrap());
    assert!(!intent.matches_admission(&scenario.episode, &changed, &scenario.component, &CollectionBudget::default(),));
}

#[test]
fn every_caller_ceiling_must_accommodate_the_exact_tested_budget_without_resizing_the_work() {
    let scenario = scenario();
    let intent = scenario.intent();
    assert!(intent.matches_admission(
        &scenario.episode,
        &scenario.recipe,
        &scenario.component,
        &CollectionBudget::upper_limits()
    ));
    for field in [
        "page_rows",
        "max_snapshot_rows",
        "page_bytes",
        "total_bytes",
        "max_sql_bytes",
        "max_plan_bytes",
        "max_pages",
        "max_queries",
        "statement_timeout_ms",
        "total_timeout_ms",
    ] {
        let mut ceiling = intent.budget.clone();
        match field {
            // These dimensions are already at the protocol minimum: a forbidden zero must not be repaired.
            "page_rows" => ceiling.page_rows = 0,
            "max_snapshot_rows" => ceiling.max_snapshot_rows = 0,
            "page_bytes" => ceiling.page_bytes /= 2,
            "total_bytes" => ceiling.total_bytes /= 2,
            "max_sql_bytes" => ceiling.max_sql_bytes /= 2,
            "max_plan_bytes" => ceiling.max_plan_bytes /= 2,
            "max_pages" => ceiling.max_pages -= 1,
            "max_queries" => ceiling.max_queries -= 1,
            "statement_timeout_ms" => ceiling.statement_timeout_ms -= 1,
            _ => ceiling.total_timeout_ms -= 1,
        }
        assert!(
            !intent.matches_admission(&scenario.episode, &scenario.recipe, &scenario.component, &ceiling),
            "{field}"
        );
        assert_eq!(intent.budget, intent.qualification.tested_budget, "no smaller budget is silently substituted");
    }
}

#[test]
fn scope_epoch_component_recipe_and_receipt_changes_cannot_borrow_an_existing_intent() {
    let scenario = scenario();
    let intent = scenario.intent();
    for changed in
        ["recipe-id", "version", "envelope", "epoch", "population", "scope", "native-cost-proof", "accounting"]
    {
        let mut recipe = scenario.recipe.clone();
        match changed {
            "recipe-id" => recipe.id = "another-source.page-wait-focus".into(),
            "version" => recipe.version += 1,
            "envelope" => recipe.budget.max_queries = intent.budget.max_queries - 1,
            _ => {
                let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
                match changed {
                    "epoch" => receipt.scope.native_epoch = Some("another-native-epoch".into()),
                    "population" => receipt.scope.population = ActivityPopulation::DefaultDatabaseConnections,
                    "scope" => receipt.scope.measurement_scope = MeasurementScope::Server,
                    "native-cost-proof" => receipt.evidence.payload_sha256 = digest(b"different-native-proof"),
                    _ => receipt.read_metrics.templates[0].client_elapsed_ms = 2.0,
                }
            }
        }
        assert!(
            !intent.matches_admission(&scenario.episode, &recipe, &scenario.component, &CollectionBudget::default()),
            "{changed}"
        );
    }
    for changed in ["component", "version", "revision"] {
        let mut component = scenario.component.clone();
        match changed {
            "component" => component.component = "another-native-reader".into(),
            "version" => component.version = Some("2.0.0".into()),
            _ => component.revision = Some("different-native-reader-build".into()),
        }
        assert!(
            !intent.matches_admission(&scenario.episode, &scenario.recipe, &component, &CollectionBudget::default()),
            "{changed}"
        );
    }
}

#[test]
fn active_untracked_failed_or_over_budget_measurements_never_form_a_valid_cost_receipt() {
    for invalid in
        ["empty", "active", "untracked", "unfinished", "failure", "queries", "bytes", "overflow", "nan", "revision"]
    {
        let mut scenario = scenario();
        let receipt = scenario.receipt();
        match invalid {
            "empty" => receipt.read_metrics.templates.clear(),
            "active" => receipt.read_metrics.active_tasks = 1,
            "untracked" => receipt.read_metrics.untracked_tasks = 1,
            "unfinished" => receipt.read_metrics.templates[0].completed_tasks = 0,
            "failure" => receipt.read_metrics.templates[0].failures = 1,
            "queries" => receipt.read_metrics.templates[0].completed_tasks = receipt.tested_budget.max_queries + 1,
            "bytes" => {
                receipt.read_metrics.templates[0].charged_value_bytes =
                    (u64::from(receipt.tested_budget.total_bytes) + 1).to_string()
            }
            "overflow" => {
                receipt.read_metrics.templates[0].charged_value_bytes = u64::MAX.to_string();
                let mut second = receipt.read_metrics.templates[0].clone();
                second.template_id = digest(b"second-native-template");
                receipt.read_metrics.templates.push(second);
            }
            "nan" => receipt.read_metrics.templates[0].client_elapsed_ms = f64::NAN,
            _ => receipt.component.revision = None,
        }
        assert!(!receipt.is_valid(), "{invalid}");
        assert!(receipt.key().is_err(), "{invalid}");
        assert!(!scenario.recipe.is_valid(), "{invalid}");
    }
}

#[test]
fn full_intent_identity_binds_receipt_revision_and_budget_while_stage_identity_deduplicates_them() {
    let scenario = scenario();
    let first = scenario.intent();
    let original = first.key().unwrap();
    let stage = first.episode_stage_key().unwrap();
    for changed in ["revision", "parent", "recipe", "receipt", "budget"] {
        let mut other = first.clone();
        match changed {
            "revision" => other.episode_revision = "2".into(),
            "parent" => other.focus.parent.row_ordinal += 1,
            "recipe" => {
                other.recipe_version += 1;
                other.qualification.recipe_version = other.recipe_version;
            }
            "receipt" => other.qualification.evidence.row_ordinal += 1,
            _ => {
                other.budget.max_queries += 1;
                other.qualification.tested_budget = other.budget.clone();
            }
        }
        assert!(other.is_valid(), "{changed} still needs external context/authentication");
        assert_ne!(other.key().unwrap(), original, "{changed}");
        assert_eq!(other.episode_stage_key().unwrap(), stage, "{changed}");
    }
}

#[test]
fn newer_same_episode_and_qualified_recovery_preserve_owed_work_but_rollback_and_terminal_analysis_do_not() {
    let scenario = scenario();
    let intent = scenario.intent();
    let original = observe(&frame(1, vec![actor()], true), None);
    let newer = observe(&frame(2, vec![actor()], true), Some(&original.checkpoint));
    let recovered = observe(&frame(3, vec![], true), Some(&newer.checkpoint));
    for episode in [&newer.checkpoint.episodes[0], &recovered.checkpoint.episodes[0]] {
        assert!(intent.matches_admission(episode, &scenario.recipe, &scenario.component, &CollectionBudget::default()));
        assert_eq!(intent.episode_revision, "1", "the queued origin is immutable");
    }
    assert_eq!(recovered.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Resolved);
    let mut later_bound = intent.clone();
    later_bound.episode_revision = "2".into();
    assert!(!later_bound.matches_admission(
        &scenario.episode,
        &scenario.recipe,
        &scenario.component,
        &CollectionBudget::default()
    ));
    let mut terminal = recovered.checkpoint.episodes[0].clone();
    terminal.analysis = DiagnosticAnalysisStatus::Unresolved;
    terminal.issue = Some(DiagnosticAnalysisIssue::NativeContextGone);
    assert!(terminal.is_valid());
    assert!(!intent.matches_admission(&terminal, &scenario.recipe, &scenario.component, &CollectionBudget::default()));
}

#[test]
fn raw_intent_changes_and_unknown_components_fail_storage_validation_without_repairing_their_identity() {
    let scenario = scenario();
    for invalid in ["id", "revision", "recipe", "budget", "epoch", "component", "unknown-fields"] {
        let mut encoded = serde_json::to_value(scenario.intent()).unwrap();
        match invalid {
            "id" => encoded["episode_id"] = serde_json::json!("not-a-digest"),
            "revision" => encoded["episode_revision"] = serde_json::json!("01"),
            "recipe" => encoded["recipe_version"] = serde_json::json!(2),
            "budget" => encoded["budget"]["max_queries"] = serde_json::json!(2),
            "epoch" => encoded["qualification"]["scope"]["native_epoch"] = serde_json::json!(null),
            "component" => encoded["qualification"]["component"]["version"] = serde_json::json!(null),
            _ => encoded["unrelated_policy"] = serde_json::json!(true),
        }
        match serde_json::from_value::<DiagnosticInvestigationIntent>(encoded) {
            Ok(intent) => {
                assert!(!intent.is_valid(), "{invalid}");
                assert!(intent.key().is_err(), "{invalid}");
            }
            Err(_) => assert_eq!(invalid, "unknown-fields"),
        }
    }
}

fn completed_result(scenario: &Scenario) -> ActivityWaitFocusResult {
    ActivityWaitFocusResult {
        focus: scenario.focus.clone(),
        lineage: Some(DiagnosticInvestigationLineage::from_intent(&scenario.intent()).unwrap()),
        observed_from: "2020-01-01T00:02:00Z".into(),
        observed_until: "2020-01-01T00:02:01Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    }
}

fn result_source(result: ActivityWaitFocusResult) -> CollectionDescription {
    let source = "native-page-targets".to_owned();
    CollectionDescription {
        target: result.focus.target.clone(),
        source: source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 0,
                sources: vec![StateSourceCoverage::observed(source, 0, false)],
            },
            result: Box::new(result),
        },
    }
}

#[test]
fn completed_lineage_reconstructs_the_exact_retired_intent_without_duplicating_its_original_focus() {
    let scenario = scenario();
    let intent = scenario.intent();
    let source = result_source(completed_result(&scenario));
    let encoded = serde_json::to_vec(&source).unwrap();
    let restored: CollectionDescription = serde_json::from_slice(&encoded).unwrap();
    let SourceObservation::FocusedPageWait { result, .. } = &restored.observation else { panic!() };
    let lineage = result.lineage.as_ref().unwrap();
    assert!(result.is_valid());
    assert_eq!(lineage.reconstruct(&result.focus).unwrap(), intent);
    assert_eq!(lineage.intent_key, intent.key().unwrap());
    assert_eq!(lineage.budget, intent.budget);
    assert_eq!(lineage.qualification, intent.qualification);
    assert!(serde_json::to_value(lineage).unwrap().get("focus").is_none());
    assert_eq!(restored, source);
}

#[test]
fn completed_lineage_rejects_changed_intent_episode_recipe_budget_approval_and_original_parent() {
    let scenario = scenario();
    for changed in
        ["intent-key", "episode", "revision", "recipe-version", "budget", "receipt", "component", "parent", "original"]
    {
        let mut result = completed_result(&scenario);
        let lineage = result.lineage.as_mut().unwrap();
        match changed {
            "intent-key" => lineage.intent_key = digest(b"another-admitted-intent"),
            "episode" => lineage.episode_id = digest(b"another-episode"),
            "revision" => lineage.episode_revision = "2".into(),
            "recipe-version" => {
                lineage.recipe_version += 1;
                lineage.qualification.recipe_version = lineage.recipe_version;
            }
            "budget" => {
                lineage.budget.max_queries += 1;
                lineage.qualification.tested_budget = lineage.budget.clone();
            }
            "receipt" => lineage.qualification.evidence.row_ordinal += 1,
            "component" => lineage.qualification.component.revision = Some("another-native-build".into()),
            "parent" => result.focus.parent.row_ordinal += 1,
            _ => result.focus.original.elapsed_ms = Some("2".into()),
        }
        assert!(!result.is_valid(), "{changed}");
        assert!(result.lineage.as_ref().unwrap().reconstruct(&result.focus).is_err(), "{changed}");
    }
}

#[test]
fn automatic_lineage_requires_the_same_admitted_request_budget_in_every_dimension() {
    let scenario = scenario();
    let intent = scenario.intent();
    let source = result_source(completed_result(&scenario));
    let original = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(intent.focus.clone()) },
        budget: intent.budget.clone(),
    };
    assert!(source.matches_request(&original));
    for changed in [
        "page_rows",
        "max_snapshot_rows",
        "page_bytes",
        "total_bytes",
        "max_sql_bytes",
        "max_plan_bytes",
        "max_pages",
        "max_queries",
        "statement_timeout_ms",
        "total_timeout_ms",
    ] {
        let mut request = original.clone();
        match changed {
            "page_rows" => request.budget.page_rows += 1,
            "max_snapshot_rows" => request.budget.max_snapshot_rows += 1,
            "page_bytes" => request.budget.page_bytes += 1,
            "total_bytes" => request.budget.total_bytes += 1,
            "max_sql_bytes" => request.budget.max_sql_bytes += 1,
            "max_plan_bytes" => request.budget.max_plan_bytes += 1,
            "max_pages" => request.budget.max_pages += 1,
            "max_queries" => request.budget.max_queries += 1,
            "statement_timeout_ms" => request.budget.statement_timeout_ms += 1,
            _ => request.budget.total_timeout_ms += 1,
        }
        request.validate().unwrap();
        assert!(!source.matches_request(&request), "{changed}: a compatible ceiling cannot replace admitted work");
    }
}

#[test]
fn manual_focused_results_retain_no_automatic_lineage_or_inferred_admission_budget() {
    let scenario = scenario();
    let mut result = completed_result(&scenario);
    result.lineage = None;
    assert!(result.is_valid());
    assert!(serde_json::to_value(&result).unwrap().get("lineage").is_none());
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(result.focus.clone()) },
        budget: CollectionBudget::default(),
    };
    assert!(result_source(result).matches_request(&request));
}
