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
    assert_eq!(native.intersection(&high), native);
    let mut invalid = strict;
    invalid.max_queries = 0;
    assert!(native.intersection(&invalid).validate().is_err(), "intersection never repairs a forbidden caller limit");
}
