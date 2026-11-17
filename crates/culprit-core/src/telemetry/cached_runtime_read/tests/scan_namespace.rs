use super::*;

fn known_database() -> (CachedRuntimeRead, crate::Snapshot) {
    let mut read = read();
    let mut database = read.description.target.database().unwrap().clone();
    database.incarnation = Some("original-native-database-guid".into());
    read.description.target = database.clone().into();
    let mut snapshot = snapshot(read.clone());
    snapshot.server.engine = database.engine;
    snapshot.server.database = database.database.clone();
    snapshot.identity = Some(database);
    (read, snapshot)
}

#[test]
fn exact_native_database_namespace_survives_an_endpoint_alias_and_database_rename_without_source_repair() {
    let (read, mut snapshot) = known_database();
    let before = serde_json::to_vec(&read.description).unwrap();
    let current = snapshot.identity.as_mut().unwrap();
    current.database = "native-renamed-database".into();
    current.location = crate::DatabaseLocation::Server { host: "new-alias.invalid".into(), port: 1444, instance: None };
    snapshot.server.database = current.database.clone();
    assert!(read.matches_native_database(&snapshot));
    assert_eq!(before, serde_json::to_vec(&read.description).unwrap());
    assert!(
        !read.description.target.snapshot_namespace_comparison(&snapshot).allows_comparison(),
        "the narrow native namespace rule does not change general endpoint comparison"
    );
}

#[test]
fn matching_names_and_endpoints_never_fill_an_unknown_or_changed_native_database_incarnation() {
    for change in ["source-unknown", "scan-unknown", "both-unknown", "source-blank", "scan-blank", "changed", "missing"]
    {
        let (mut read, mut snapshot) = known_database();
        let CollectionTarget::Database(original) = &mut read.description.target else { panic!() };
        match change {
            "source-unknown" => original.incarnation = None,
            "scan-unknown" => snapshot.identity.as_mut().unwrap().incarnation = None,
            "both-unknown" => {
                original.incarnation = None;
                snapshot.identity.as_mut().unwrap().incarnation = None;
            }
            "source-blank" => original.incarnation = Some("   ".into()),
            "scan-blank" => snapshot.identity.as_mut().unwrap().incarnation = Some("   ".into()),
            "changed" => snapshot.identity.as_mut().unwrap().incarnation = Some("replacement-native-guid".into()),
            _ => snapshot.identity = None,
        }
        assert!(!read.matches_native_database(&snapshot), "{change}");
    }
}

#[test]
fn a_native_identifier_cannot_correct_an_identity_that_names_another_scan_engine_or_database() {
    for change in ["server-engine", "identity-engine", "server-database", "identity-database"] {
        let (read, mut snapshot) = known_database();
        match change {
            "server-engine" => snapshot.server.engine = crate::Engine::Postgres,
            "identity-engine" => snapshot.identity.as_mut().unwrap().engine = crate::Engine::Postgres,
            "server-database" => snapshot.server.database = "another-database".into(),
            _ => snapshot.identity.as_mut().unwrap().database = "another-database".into(),
        }
        assert!(!read.matches_native_database(&snapshot), "{change}");
    }
}
