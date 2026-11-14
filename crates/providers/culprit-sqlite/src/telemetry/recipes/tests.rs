use super::*;
use culprit_core::{ConnectionSpec, Provider};

fn unopened_path(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("culprit-catalog-{}-{label}-{nonce}.sqlite", std::process::id()));
    assert!(!path.exists());
    path
}

fn request(dataset: SnapshotDataset, budget: CollectionBudget) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset },
        budget,
    }
}

#[test]
fn the_file_catalog_drives_every_constructor_without_opening_or_creating_a_file() {
    let path = unopened_path("declared");
    let provider = crate::SqliteProvider::new(&ConnectionSpec {
        database: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    })
    .unwrap();
    let catalog = crate::telemetry_recipes();
    assert!(recipe_catalog_is_valid(&catalog));
    assert_eq!(catalog.len(), 10);
    for recipe in &catalog {
        let RecipeSelector::Collection(TelemetryCollection::Snapshot(dataset)) = recipe.selector else {
            panic!("SQLite must not advertise server sessions or retained query counters");
        };
        assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
        let request = request(dataset, CollectionBudget::default());
        assert!(recipe.accepts(&request));
        assert_eq!(select(&request).unwrap().id, recipe.id);
        assert_eq!(REGISTRATIONS.iter().filter(|entry| entry.selector().matches(&request)).count(), 1);
        let source = provider.telemetry_source(&request).unwrap();
        assert_eq!(source.query_count(), 0);
        assert!(source.read_metrics().is_none_or(|metrics| metrics.active_tasks == 0));
        assert!(!path.exists());
    }
    assert_eq!(catalog.iter().filter(|recipe| recipe.purpose == RecipePurpose::Baseline).count(), 2);
}

#[test]
fn unsupported_native_scope_and_sources_never_borrow_a_file_recipe() {
    let path = unopened_path("unsupported");
    let provider = crate::SqliteProvider::new(&ConnectionSpec {
        database: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    })
    .unwrap();
    let mut shared = request(SnapshotDataset::Settings, CollectionBudget::default());
    shared.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: culprit_core::Engine::Sqlite,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("file-namespace"),
            native: NativeIdentity::new("not-a-server"),
        },
    };
    let mut activity = request(SnapshotDataset::Settings, CollectionBudget::default());
    activity.selection = CollectionSelection::Activity;
    for request in [shared, activity, request(SnapshotDataset::TaskSpace, CollectionBudget::default())] {
        assert!(select(&request).is_none());
        assert!(crate::telemetry_recipes().iter().all(|recipe| !recipe.accepts(&request)));
        assert_eq!(provider.telemetry_source(&request).err().unwrap().code, ErrorCode::Unsupported);
        assert!(!path.exists());
    }
}

#[test]
fn file_recipe_envelopes_preserve_valid_large_callers_and_every_stricter_limit() {
    let large = request(SnapshotDataset::Tables, CollectionBudget::upper_limits());
    assert!(large.validate().is_ok());
    let recipe = select(&large).unwrap();
    assert_eq!(recipe.bounded_request(&large).budget, large.budget);
    let strict = request(
        SnapshotDataset::Tables,
        CollectionBudget {
            page_rows: 1,
            max_snapshot_rows: 1,
            page_bytes: 1024,
            total_bytes: 2048,
            max_sql_bytes: 1024,
            max_plan_bytes: 1024,
            max_pages: 1,
            max_queries: 2,
            statement_timeout_ms: 10,
            total_timeout_ms: 20,
        },
    );
    assert!(strict.validate().is_ok());
    assert_eq!(recipe.bounded_request(&strict).budget, strict.budget);
}
