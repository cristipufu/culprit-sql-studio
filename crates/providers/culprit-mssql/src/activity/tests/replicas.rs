use super::*;

const OTHER_REPLICA: &str = "91487244-730d-4b83-8e49-d482241c7289";

fn replies(row: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    vec![Ok(vec![authority(&row)]), Ok(vec![row.clone()]), Ok(vec![authority(&row)])]
}

async fn captured(row: Rec) -> CollectionDescription {
    let mut owner = reader(replies(row), IdentityDetail::AzureDatabase);
    let (description, _) = crate::telemetry::activity::capture(&mut owner).await.unwrap();
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    description
}

#[tokio::test]
async fn native_members_partition_one_logical_database_while_observed_roles_do_not_become_member_keys() {
    let primary = captured(metadata(42)).await;
    let secondary = captured(replace(&metadata(42), &[("updateability", Val::Text("READ_ONLY".into()))])).await;
    assert_eq!(primary.target, secondary.target);
    assert_eq!(primary.partition, secondary.partition, "role is observed state, not a member ID or epoch");
    assert_eq!(primary.logical_source_id().unwrap(), secondary.logical_source_id().unwrap());
    assert_eq!(secondary.observation.activity().unwrap().database_writable, Some(false));
    let other = captured(replace(&metadata(42), &[("replica_id", Val::Text(OTHER_REPLICA.into()))])).await;
    assert_eq!(primary.target, other.target);
    assert!(!primary.partition.same_member(&other.partition));
    assert_ne!(primary.logical_source_id().unwrap(), other.logical_source_id().unwrap());
    assert_ne!(primary.captured_range().unwrap().scope_id, other.captured_range().unwrap().scope_id);
}

#[tokio::test]
async fn native_process_restart_changes_the_scope_epoch_without_inventing_a_new_compute_member() {
    let mut first = reader(replies(metadata(42)), IdentityDetail::AzureDatabase);
    let (before, _) = crate::telemetry::activity::capture(&mut first).await.unwrap();
    let mut restarted = replies(metadata(42));
    for index in [0, 2] {
        let rows = restarted[index].as_mut().unwrap();
        rows[0] = replace(&rows[0], &[("local_started_ticks", Val::Int(200))]);
    }
    let mut restarted = reader(restarted, IdentityDetail::AzureDatabase);
    let (after, _) = crate::telemetry::activity::capture(&mut restarted).await.unwrap();
    assert!(before.partition.same_member(&after.partition));
    assert_eq!(before.logical_source_id().unwrap(), after.logical_source_id().unwrap());
    assert_ne!(before.captured_range().unwrap().scope_id, after.captured_range().unwrap().scope_id);
    assert_ne!(before.observation.activity().unwrap().epoch, after.observation.activity().unwrap().epoch);
}

#[tokio::test]
async fn missing_member_native_epoch_or_role_preserves_positive_rows_without_current_head_or_focus_authority() {
    for missing in ["member", "process", "role"] {
        let row = match missing {
            "member" => replace(&metadata(42), &[("replica_id", Val::Null)]),
            "role" => replace(&metadata(42), &[("updateability", Val::Null)]),
            _ => metadata(42),
        };
        let mut native = replies(row);
        if missing == "process" {
            for index in [0, 2] {
                let rows = native[index].as_mut().unwrap();
                rows[0] = replace(&rows[0], &crate::activity::database_context::tests::epoch_fields(false));
            }
        }
        let mut owner = reader(native, IdentityDetail::AzureDatabase);
        let (description, rows) = crate::telemetry::activity::capture(&mut owner).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!description.is_partition_qualified(), "{missing}");
        assert!(description.partition_is_consistent());
        let native = description.observation.activity().unwrap();
        assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
        assert!(!native.coverage.complete_scope);
        assert_eq!(native.coverage.stopped, Some(CollectionStop::SourceUnavailable));
        assert!(native.epoch.is_none());
        assert_eq!(owner.query_count(), 3);
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn member_role_and_native_process_changes_during_one_reader_stop_publication() {
    for (field, value) in [
        ("replica_id", Val::Text(OTHER_REPLICA.into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("local_process_id", Val::Int(12)),
        ("local_started_ticks", Val::Int(200)),
        ("local_started_without_offset", Val::Text("2020-01-01T05:00:00".into())),
    ] {
        let mut native = replies(metadata(42));
        let rows = native[2].as_mut().unwrap();
        rows[0] = replace(&rows[0], &[(field, value)]);
        let mut owner = reader(native, IdentityDetail::AzureDatabase);
        let error = crate::telemetry::activity::capture(&mut owner).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(owner.query_count(), 3);
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}

struct Reconnecting {
    inner: FakeRead,
    after: usize,
}
#[async_trait::async_trait]
impl NativeRead for Reconnecting {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(if self.inner.calls.len() >= self.after { 2 } else { 1 })
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}

#[tokio::test]
async fn reconnecting_to_the_same_native_member_and_process_cannot_reuse_an_earlier_reader_authority() {
    let base = reader(replies(metadata(42)), IdentityDetail::AzureDatabase);
    let mut owner =
        ActivityReader::new(Reconnecting { inner: base.io, after: 2 }, base.options, "fixture.invalid".into(), 1433);
    let error = crate::telemetry::activity::capture(&mut owner).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(owner.query_count(), 3);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn reconnecting_during_boxed_authority_acquisition_keeps_baseline_rows_without_borrowing_the_prior_instance() {
    let platform = crate::instance::test_support::metadata();
    let other_platform = replace(
        &platform,
        &[
            ("server_name", Val::Text("another-native-instance".into())),
            ("machine_name", Val::Text("another-native-machine".into())),
        ],
    );
    let local = replace(&metadata(42), &[("edition", Val::Int(3))]);
    let native = vec![
        Ok(vec![platform]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![crate::activity::database_context::tests::row(3)]),
        Ok(vec![local]),
        Ok(vec![other_platform]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![crate::activity::database_context::tests::row(3)]),
    ];
    let base = reader(native, IdentityDetail::Endpoint);
    let mut owner =
        ActivityReader::new(Reconnecting { inner: base.io, after: 2 }, base.options, "fixture.invalid".into(), 1433);
    let (description, rows) = crate::telemetry::activity::capture(&mut owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(description.partition, SourcePartition::InstanceLocal { member: None, epoch: None }));
    assert_eq!(description.observation.activity().unwrap().coverage.state, culprit_core::SourceState::Partial);
    assert_eq!(owner.query_count(), 7);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn terminal_authority_and_replica_baseline_cancellation_preserve_source_fencing_and_forbid_replacement_work() {
    for stage in [0, 1, 2] {
        let mut native = replies(metadata(42));
        native[stage] = Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        let mut owner = reader(native, IdentityDetail::AzureDatabase);
        owner.io.shutdown = SourceShutdown::Unconfirmed;
        let error = crate::telemetry::activity::capture(&mut owner).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        assert_eq!(owner.query_count(), stage as u32 + 1);
        assert_eq!(owner.close().await, SourceShutdown::Unconfirmed);
        assert_eq!(owner.io.closed, 1);
    }
}
