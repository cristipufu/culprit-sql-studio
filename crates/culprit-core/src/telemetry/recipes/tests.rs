use super::*;

fn descriptor() -> RecipeDescriptor {
    RecipeDescriptor {
        id: "source.baseline".into(),
        version: 1,
        purpose: RecipePurpose::Baseline,
        selector: RecipeSelector::Collection(TelemetryCollection::Activity),
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Unqualified,
    }
}

#[test]
fn registration_describes_actual_selection_scope_and_unqualified_cost_independently() {
    let descriptor = descriptor();
    let mut request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget::default(),
    };
    assert!(descriptor.is_valid());
    assert!(descriptor.accepts(&request));
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets };
    assert!(!descriptor.accepts(&request));
    assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
    request.selection = CollectionSelection::Activity;
    request.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: crate::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native"),
            native: NativeIdentity::new("instance"),
        },
    };
    assert!(!descriptor.accepts(&request));
}

#[test]
fn invalid_or_ambiguous_recipe_declarations_cannot_authorize_policy_work() {
    for field in ["id", "version", "subjects", "duplicate-subject", "budget"] {
        let mut descriptor = descriptor();
        match field {
            "id" => descriptor.id.clear(),
            "version" => descriptor.version = 0,
            "subjects" => descriptor.subjects.clear(),
            "duplicate-subject" => descriptor.subjects.push(RecipeSubject::Database),
            _ => descriptor.budget.max_queries = 0,
        }
        assert!(!descriptor.is_valid(), "{field}");
    }
}

#[test]
fn intersection_preserves_both_native_envelope_and_stricter_callers_in_every_dimension() {
    let native = CollectionBudget::default();
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 2,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 2048,
        max_pages: 1,
        max_queries: 2,
        statement_timeout_ms: 10,
        total_timeout_ms: 20,
    };
    assert!(strict.validate().is_ok());
    assert_eq!(native.intersection(&strict), strict);
    assert_eq!(strict.intersection(&native), strict);
    let high = CollectionBudget {
        page_rows: 10_000,
        max_snapshot_rows: 100_000,
        page_bytes: 8 * 1024 * 1024,
        total_bytes: 1024 * 1024 * 1024,
        max_sql_bytes: 4 * 1024 * 1024,
        max_plan_bytes: 4 * 1024 * 1024,
        max_pages: 10_000,
        max_queries: 100_000,
        statement_timeout_ms: 60_000,
        total_timeout_ms: 900_000,
    };
    assert!(high.validate().is_ok());
    assert_eq!(CollectionBudget::upper_limits(), high);
    assert_eq!(native.intersection(&high), native);
    let mut invalid = strict;
    invalid.max_queries = 0;
    assert!(native.intersection(&invalid).validate().is_err(), "intersection never repairs a forbidden caller limit");
}

#[test]
fn every_protocol_budget_ceiling_is_enforced_independently_of_caller_defaults() {
    let upper = CollectionBudget::upper_limits();
    assert!(upper.validate().is_ok());
    assert_eq!(upper.intersection(&CollectionBudget::default()), CollectionBudget::default());
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
        let mut outside = upper.clone();
        match field {
            "page_rows" => outside.page_rows += 1,
            "max_snapshot_rows" => outside.max_snapshot_rows += 1,
            "page_bytes" => outside.page_bytes += 1,
            "total_bytes" => outside.total_bytes += 1,
            "max_sql_bytes" => outside.max_sql_bytes += 1,
            "max_plan_bytes" => outside.max_plan_bytes += 1,
            "max_pages" => outside.max_pages += 1,
            "max_queries" => outside.max_queries += 1,
            "statement_timeout_ms" => outside.statement_timeout_ms += 1,
            "total_timeout_ms" => outside.total_timeout_ms += 1,
            _ => unreachable!(),
        }
        assert!(outside.validate().is_err(), "{field}");
    }
}

#[test]
fn catalogs_accept_undeclared_support_and_distinct_native_subjects_without_inventing_availability() {
    assert!(recipe_catalog_is_valid(&[]));
    let database = descriptor();
    let mut instance = descriptor();
    instance.id = "source.instance-baseline".into();
    instance.subjects = vec![RecipeSubject::Shared(SharedResourceKind::Instance)];
    assert!(recipe_catalog_is_valid(&[database, instance]));
}

#[test]
fn catalog_ids_and_selector_subject_ownership_must_be_unambiguous() {
    let first = descriptor();
    let mut duplicate_id = descriptor();
    duplicate_id.selector = RecipeSelector::FocusedPageWait;
    assert!(!recipe_catalog_is_valid(&[first.clone(), duplicate_id]));
    let mut overlap = descriptor();
    overlap.id = "source.another-baseline".into();
    overlap.subjects.push(RecipeSubject::Shared(SharedResourceKind::Instance));
    assert!(!recipe_catalog_is_valid(&[first.clone(), overlap]));
    let mut invalid = first.clone();
    invalid.budget.total_timeout_ms = 0;
    assert!(!recipe_catalog_is_valid(&[invalid]));
    let oversized = (0..=MAX_TELEMETRY_RECIPES)
        .map(|index| {
            let mut recipe = first.clone();
            recipe.id = format!("source.recipe-{index}");
            recipe
        })
        .collect::<Vec<_>>();
    assert!(!recipe_catalog_is_valid(&oversized));
}

#[test]
fn a_native_provider_catalog_cannot_claim_the_collectors_derived_diagnostic_source() {
    let mut derived = descriptor();
    derived.selector = RecipeSelector::Collection(TelemetryCollection::Diagnostics);
    assert!(derived.is_valid(), "the descriptor is well formed, but its ownership is not native");
    assert!(!recipe_catalog_is_valid(&[derived]));
}
