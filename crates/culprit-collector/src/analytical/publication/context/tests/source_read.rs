use super::*;

#[test]
fn source_read_public_context_keeps_only_snapshot_and_opaque_question_execution_keys() {
    let mut source = test_support::compiler_admission_investigation_fixture().baseline_source;
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    database.incarnation = Some("private-source-read-logical-database".into());
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
    member.namespace.id = "private-source-read-namespace".into();
    member.native.id = "private-source-read-native-member".into();
    let execution = NativeReadContext::from_activity(&source).unwrap();
    let scope = NativeSourceReadScope::from_context(&execution).unwrap();
    let snapshot = StateSnapshotDescription {
        dataset: SnapshotDataset::QueryCompilation,
        records: 0,
        observed_from: execution.observed_from.clone(),
        observed_until: execution.observed_until.clone(),
        sources: vec![StateSourceCoverage::observed(culprit_core::source_id::QUERY_COMPILATION.into(), 0, false)],
    };
    let focus_key = digest(b"private-original-and-gateway-proof");
    let observation = SourceObservation::SourceRead {
        snapshot: snapshot.clone(),
        focus_key: focus_key.clone(),
        execution: Box::new(execution),
    };
    let public = super::super::observation(&observation).unwrap();
    assert_eq!(public["kind"], "source-read-context");
    assert_eq!(public["data"]["snapshot"], serde_json::to_value(snapshot).unwrap());
    assert_eq!(public["data"]["focus_key"], focus_key);
    assert_eq!(public["data"]["execution_key"], scope.key().unwrap());
    assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
    for field in ["execution", "target", "partition", "basis", "prerequisite", "database_writable"] {
        assert!(public["data"].get(field).is_none(), "{field}");
    }
    let encoded = serde_json::to_string(&public).unwrap();
    for canary in
        ["private-source-read-logical-database", "private-source-read-namespace", "private-source-read-native-member"]
    {
        assert!(!encoded.contains(canary), "{canary}");
    }
}
