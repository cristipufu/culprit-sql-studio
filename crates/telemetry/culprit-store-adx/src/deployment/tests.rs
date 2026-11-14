use super::*;
#[test]
fn deployment_contains_one_route_and_the_exact_runtime_mapping_for_every_analytical_table() {
    let catalog = SchemaCatalog::new().unwrap();
    let assets = DeploymentAssets::new().unwrap();
    assert_eq!(assets.schema_digest, catalog.digest());
    assert_eq!(assets.routes.len(), catalog.schemas().count() + 1);
    for schema in catalog.schemas().map(|(_, schema)| schema).chain(std::iter::once(catalog.publication())) {
        assert!(assets.commands.contains(&schema.create_table()));
        assert!(assets.commands.contains(&schema.create_mapping().unwrap()));
        let route = assets.routes.iter().find(|route| route.table == schema.table()).unwrap();
        assert_eq!(route.mapping, schema.mapping_name());
        assert_eq!(route.blob_prefix, format!("ready/{}/", schema.table()));
        assert_eq!(route.blob_suffix, ".ndjson");
        assert_eq!(route.format, "json");
    }
    assert!(assets.commands.iter().all(|command| {
        ![
            "$TABLE",
            "$PROJECTION",
            "$COLUMNS",
            "$SCHEMA",
            "$RESTORE",
            "$FUNCTION",
            "$COMPLETE",
            "$SELECTION",
            "$GROUP_KEYS",
        ]
        .iter()
        .any(|placeholder| command.contains(placeholder))
    }));
    assert!(!assets.script().contains("skipvalidation"));
    assert!(assets.script().starts_with(".execute database script with (ContinueOnErrors=false)"));
}
#[test]
fn empty_set_heads_and_revision_rules_are_deliberate_per_family_and_never_use_arrival_order() {
    let assets = DeploymentAssets::new().unwrap();
    let script = assets.script();
    assert!(script.contains("CulpritQueryRuntimeV1Rows"));
    assert!(script.contains("CulpritDeadlocksV1Rows"));
    assert!(script.contains("CulpritActivityV1Current"));
    assert!(script.contains("CulpritObjectsV1Current"));
    assert!(script.contains("tobool(SourceDescription.observation.data.coverage.complete_scope)"));
    assert!(
        script.contains("tostring(SourceDescription.observation.data.coverage.state) in (\"collected\", \"empty\")")
    );
    assert!(!script.contains("SourceDescription.observation.data.complete_scope"));
    assert!(script.contains("max(CaptureSequence)"));
    assert!(script.contains("PresentRows == ExpectedRows"));
    assert!(script.contains("Variants != 1"));
    assert!(!script.contains("ingestion_time"));
    assert!(!script.contains("arg_max"));
    // These are contract structure checks. Functional Kusto execution is a separate emulator integration gate.
}
