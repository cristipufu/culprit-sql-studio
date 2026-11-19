use super::*;

fn read() -> StorageRead {
    let mut read = super::super::tests::read();
    read.observations.truncate(1);
    let row = &mut read.observations[0];
    row.source = culprit_core::source_id::LOG_CAPACITY.into();
    row.key = "shared-tempdb-log".into();
    let space = space_mut(&mut read);
    space.population = StoragePopulation::SharedTempdbTransactionLog;
    space.kind = culprit_core::FileKind::Log;
    space.scope = culprit_core::FileScope::Server;
    space.database = None;
    space.native_name = Some("tempdb".into());
    space.used_bytes = Some("1".into());
    space.reuse_wait = Some("ACTIVE_TRANSACTION".into());
    space.parts.clear();
    let owner = SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("original-native-machine"),
        native: NativeIdentity::new("original-native-instance"),
    };
    read.target = CollectionTarget::Shared(owner.clone());
    read.context = ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: read.target.clone(),
            partition: SourcePartition::InstanceLocal { member: Some(owner), epoch: Some(digest(b"native-process")) },
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        }),
    };
    read.sources = vec![StateSourceCoverage::collected(culprit_core::source_id::LOG_CAPACITY.into(), 1)];
    assert!(read.is_valid());
    read
}

fn space_mut(read: &mut StorageRead) -> &mut OperationalSpace {
    let OperationalFact::Space(space) = &mut read.observations[0].fact else { unreachable!() };
    space
}

fn free(entry: &StorageEntry) -> Option<&str> {
    entry.fields.iter().find(|field| field.key == "allocated_free_bytes").unwrap().value.as_deref()
}

#[test]
fn original_shared_log_point_explains_exact_allocated_free_bytes_and_a_last_checkpoint_candidate() {
    let read = read();
    let original = read.clone();
    let report = explain(&read).unwrap();
    let [entry] = report.entries.as_slice() else { panic!("one original shared log point") };
    assert_eq!(entry.scope, culprit_core::FileScope::Server);
    assert_eq!(entry.source, culprit_core::source_id::LOG_CAPACITY);
    assert_eq!(entry.observation_key, "shared-tempdb-log");
    assert_eq!(entry.title, t!("view.storage.temp_log.title"));
    assert_eq!(free(entry), Some("9007199254740992"));
    assert!(entry.explanation.contains(&t!("view.storage.temp_log.limit")));
    assert!(entry.explanation.contains(&t!("view.storage.temp_log.active")));
    assert_eq!(read, original);
    let json = serde_json::to_value(&report).unwrap();
    for unsupported in ["severity", "owner", "retaining_transaction", "sql", "fix_sql", "time_to_full"] {
        assert!(json.get(unsupported).is_none());
        assert!(json["entries"][0].get(unsupported).is_none());
    }
}

#[test]
fn exact_native_reuse_categories_choose_only_their_recorded_follow_up_and_never_a_tempdb_log_backup() {
    for (reason, message) in [
        (Some("ACTIVE_TRANSACTION"), "view.storage.temp_log.active"),
        (Some("CHECKPOINT"), "view.storage.temp_log.checkpoint"),
        (Some("NOTHING"), "view.storage.temp_log.nothing"),
        (Some("LOG_BACKUP"), "view.storage.temp_log.other"),
        (Some("FUTURE_NATIVE_REASON"), "view.storage.temp_log.other"),
        (None, "view.storage.temp_log.other"),
    ] {
        let mut read = read();
        space_mut(&mut read).reuse_wait = reason.map(str::to_owned);
        let report = explain(&read).unwrap();
        let entry = &report.entries[0];
        assert!(entry.explanation.ends_with(&culprit_core::i18n::t(message, &[])));
        assert_eq!(entry.fields.iter().find(|field| field.key == "reuse_wait").unwrap().value.as_deref(), reason);
        assert!(!entry.explanation.contains("BACKUP LOG"));
        assert!(report.limitations.contains(&t!("view.storage.limit.growth")));
    }
}

#[test]
fn native_nulls_zero_and_contradictory_quantities_do_not_invent_log_or_filesystem_headroom() {
    for (total, used, expected) in [
        (Some("0"), Some("0"), Some("0")),
        (Some("5"), Some("0"), Some("5")),
        (Some("5"), None, None),
        (None, Some("0"), None),
        (Some("1"), Some("2"), None),
    ] {
        let mut read = read();
        let space = space_mut(&mut read);
        space.total_bytes = total.map(str::to_owned);
        space.used_bytes = used.map(str::to_owned);
        space.limit_bytes = None;
        space.growth_enabled = None;
        let report = explain(&read).unwrap();
        let entry = &report.entries[0];
        assert_eq!(free(entry), expected);
        for key in ["limit_bytes", "growth_enabled"] {
            assert_eq!(entry.fields.iter().find(|field| field.key == key).unwrap().value, None);
        }
        assert!(entry.explanation.contains(&t!("view.storage.temp_log.limit")));
    }
}

#[test]
fn independent_data_capacity_and_partial_log_visibility_never_supply_the_log_free_bytes_or_owner() {
    let mut read = read();
    read.sources[0].state = SourceState::Partial;
    read.sources[0].complete_scope = false;
    let mut data = read.observations[0].clone();
    data.source = culprit_core::source_id::TEMP_CAPACITY.into();
    data.key = "shared-tempdb".into();
    let OperationalFact::Space(space) = &mut data.fact else { unreachable!() };
    space.population = StoragePopulation::SharedTempdbDataFiles;
    space.kind = culprit_core::FileKind::Temp;
    space.total_bytes = Some("18446744073709551615".into());
    space.used_bytes = Some("18446744073709551615".into());
    read.observations.push(data);
    read.sources.push(StateSourceCoverage::collected(culprit_core::source_id::TEMP_CAPACITY.into(), 1));
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(free(&report.entries[0]), Some("9007199254740992"));
    assert!(!report.entries[1].fields.iter().any(|field| field.key == "allocated_free_bytes"));
    read.observations.remove(0);
    read.sources[0] = StateSourceCoverage::unavailable(
        culprit_core::source_id::LOG_CAPACITY.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let unavailable = explain(&read).unwrap();
    assert_eq!(unavailable.entries.len(), 1);
    assert_eq!(unavailable.source_state, SourceState::Partial);
    assert!(!unavailable.entries[0].explanation.contains(&t!("view.storage.temp_log.active")));
}

#[test]
fn original_shared_scope_is_required_and_saved_explanation_is_identical_in_each_language() {
    let read = read();
    let restored: StorageRead = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            assert_eq!(
                serde_json::to_value(explain(&read).unwrap()).unwrap(),
                serde_json::to_value(explain(&restored).unwrap()).unwrap()
            );
        });
    }
    let mut wrong = read;
    wrong.target = super::super::tests::read().target;
    assert!(explain(&wrong).is_err());
    let database = explain(&super::super::tests::read()).unwrap();
    assert!(!database.entries.iter().any(|entry| entry.explanation.contains(&t!("view.storage.temp_log.active"))));
}
