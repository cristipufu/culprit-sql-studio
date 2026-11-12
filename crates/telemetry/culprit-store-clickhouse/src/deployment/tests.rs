use super::*;
#[test]
fn physical_storage_and_canonical_views_share_one_complete_contract() {
    let assets = DeploymentAssets::new().unwrap();
    let script = assets.script();
    assert_eq!(script.matches("CREATE TABLE IF NOT EXISTS").count(), 16);
    assert!(script.contains("Nullable(String)"));
    assert!(!script.contains("HasDataset"));
    assert!(script.contains("DateTime64(9, 'UTC')"));
    assert!(script.contains("Decimal(38, 0)"));
    assert!(script.contains("ORDER BY (RangeId, BatchId, RowOrdinal)"));
    assert!(script.contains("PROJECTION ByResourceTime"));
    for name in [
        "CulpritPublishedV1",
        "CulpritActivityHeadsV1",
        "CulpritSnapshotHeadsV1",
        "CulpritQueryRuntimeV1Rows",
        "CulpritActivityV1Current",
    ] {
        assert!(script.contains(name), "{name}");
    }
    assert!(script.contains(&assets.schema_digest));
    assert!(script.contains("R.ManifestSha256!=M.PublishedManifest"));
    assert!(script.contains("HAVING count()!=1"));
    assert!(script.contains("JSONExtractBool(Coverage,'complete_scope')"));
    assert!(!script.contains("$TABLE"));
    assert!(!script.contains("$ARGS"));
    assert!(!script.contains("DROP TABLE"));
    assert!(
        DeploymentAssets::with_layout(TableLayout::Replicated)
            .unwrap()
            .script()
            .contains("ENGINE = ReplicatedMergeTree")
    );
}
