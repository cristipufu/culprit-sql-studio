//! Read-only acceptance on the existing nonce-owned supervisor. Group setup and artifact cleanup stay there.
//! A real active plugin/group is mandatory for the positive; unsupported environments never skip-pass it.
use super::*;
use crate::{
    connect::Scope,
    native::{self, group_replication as group},
    read::ReadSession,
};
use culprit_collector::{
    capture::{self, CollectionCancellation},
    outbox::Outbox,
};
use std::collections::BTreeSet;

async fn owned() -> (crate::MysqlProvider, group::Context) {
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("owned smoke fixture nonce");
    assert!(super::replication::valid_owner(&owner));
    let provider = super::fixture();
    // One parameterized native connection guard plus a four-query group context; existing setup is paid too.
    let mut reader = ReadSession::new(
        provider.connector.clone(),
        CollectionBudget { max_queries: 13, statement_timeout_ms: 1000, total_timeout_ms: 5000, ..Default::default() },
        Scope::Server,
    );
    let acquired = async {
        let native = crate::native::metadata::connection_context(&mut reader, &provider.connector).await?;
        let (context, _) = group::context(&mut reader).await?;
        Result::Ok((native, context))
    }
    .await;
    let queries = reader.query_count();
    let shutdown = reader.close().await;
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(queries <= 13);
    let (native, context) = super::replication::required(acquired, "original nonce/member guard");
    assert!(
        native.server_name.starts_with("culprit-smoke-") && native.server_name.ends_with(&format!("-{owner}")),
        "exact owned native hostname required"
    );
    assert_eq!(native.database_name, "culprit_smoke");
    assert_eq!(native.connection_id, context.instance.connection_id);
    (provider, context)
}
async fn capture(provider: crate::MysqlProvider, context: &group::Context) -> super::capture::Capture {
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: context.instance.resource.clone() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::GroupReplication },
        budget: CollectionBudget { page_rows: 1, ..crate::group_replication::budget() },
    };
    let temporary = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&temporary.path().join("outbox"), OutboxLimits::default()).unwrap();
    let mut lease = outbox.reserve("owned-group-replication", &request).await.unwrap();
    let output = lease.directory().to_path_buf();
    let factory = super::capture::Factory(provider);
    let result = capture::collect(
        &culprit_read::admission::SourceGate::default(),
        &factory,
        request.clone(),
        &output,
        lease.identity().clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await;
    let finished = lease.finish().await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = super::replication::required(result, "actual Group Replication source/Outbox capture");
    super::replication::required(finished, "sealed Group Replication outbox completion");
    super::capture::read_capture(manifest, &request.budget, &output)
}
#[tokio::test]
#[ignore = "requires nonce-owned MySQL with an active Group Replication group and exact expected member UUIDs; supervisor owns setup"]
async fn owned_group_replication_factory_outbox_preserves_real_member_roles_and_native_transaction_statistics() {
    let (provider, context) = owned().await;
    let expected_group =
        std::env::var("CULPRIT_MYSQL_GROUP_REPLICATION_GROUP_ID").expect("owned group UUID").to_ascii_lowercase();
    assert_eq!(context.metadata.group_id.as_deref(), Some(expected_group.as_str()));
    assert!(
        context.metadata.available(MYSQL_GROUP_MEMBERS_SOURCE)
            && context.metadata.available(MYSQL_GROUP_STATISTICS_SOURCE),
        "positive native component required, unsupported is not a pass"
    );
    let expected_members = std::env::var("CULPRIT_MYSQL_GROUP_REPLICATION_MEMBER_IDS")
        .expect("exact owned group member UUIDs")
        .split(',')
        .map(|id| native::instance::resource(crate::connect::Flavor::Mysql, id).unwrap().native.id)
        .collect::<BTreeSet<_>>();
    assert!(!expected_members.is_empty() && expected_members.contains(&context.instance.resource.native.id));
    let capture = capture(provider, &context).await;
    assert_eq!(capture.manifest.status, CollectionStatus::Complete);
    let source = capture.manifest.description.as_ref().unwrap();
    assert_eq!(source.target, CollectionTarget::Shared(context.instance.resource.clone()));
    let retained_context = group_replication_context(source).unwrap();
    assert!(retained_context.has_qualified_local_owner());
    assert_eq!(retained_context.database_writable, None);
    let snapshot = source.observation.snapshot().unwrap();
    assert!(snapshot.sources.iter().all(|source| source.state == culprit_core::SourceState::Collected));
    let mut members = BTreeSet::new();
    let mut statistics = BTreeSet::new();
    for record in &capture.records {
        let TelemetryRecord::Operational(row) = &record.record else { panic!("native operational point") };
        let OperationalFact::MysqlGroupReplication(point) = &row.fact else { panic!("native group point") };
        assert!(point.fits_source(source));
        assert_eq!(point.group_id(), expected_group);
        match point.as_ref() {
            MysqlGroupReplicationPoint::Member(member) => {
                assert_eq!(member.state, "ONLINE");
                assert!(matches!(member.role.as_deref(), Some("PRIMARY" | "SECONDARY")));
                members.insert(member.member_id.clone());
            }
            MysqlGroupReplicationPoint::Statistics(stats) => {
                assert!(stats.view_id.is_some());
                assert!(stats.checked.is_some());
                assert!(stats.conflicts.is_some());
                assert!(stats.certification_queue.is_some());
                assert!(stats.applier_queue.is_some());
                statistics.insert(stats.member_id.clone());
            }
        }
    }
    assert_eq!(members, expected_members);
    assert_eq!(statistics, expected_members);
    assert_eq!(snapshot.records, capture.records.len() as u32);
    assert!(capture.manifest.source_queries <= 23);
    assert_eq!(
        crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mysql.group-replication").unwrap().cost,
        RecipeCostQualification::Unqualified
    );
    eprintln!(
        "owned Group Replication acceptance: members={} statistics={} source_queries={}; native source cost remains unqualified",
        members.len(),
        statistics.len(),
        capture.manifest.source_queries
    );
}
#[tokio::test]
#[ignore = "requires the existing nonce-owned plain MySQL fixture without an installed/configured Group Replication group"]
async fn owned_plain_mysql_preserves_qualified_unavailable_group_sources_instead_of_healthy_empty() {
    let (provider, context) = owned().await;
    assert!(
        !context.metadata.available(MYSQL_GROUP_MEMBERS_SOURCE),
        "requires a genuine absent or unconfigured native feature"
    );
    let capture = capture(provider, &context).await;
    assert_eq!(capture.manifest.status, CollectionStatus::Complete);
    let source = capture.manifest.description.as_ref().unwrap();
    assert!(group_replication_context(source).is_ok());
    assert!(capture.records.is_empty());
    let snapshot = source.observation.snapshot().unwrap();
    assert!(
        snapshot
            .sources
            .iter()
            .all(|source| source.state == culprit_core::SourceState::Unavailable && !source.complete_scope)
    );
    assert!(capture.manifest.source_queries <= 23);
}
