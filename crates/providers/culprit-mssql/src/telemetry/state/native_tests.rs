//! Real shared-state capture uses the owned loopback fixture, never a production endpoint.
use super::*;
use crate::{
    MssqlProvider,
    test_support::{OwnedSourceFixture, config},
};
use culprit_core::{Environment, MeasurementScope, Provider, SourceState};
use std::collections::BTreeSet;

fn provider(port: u16, database: &str, password: &str) -> MssqlProvider {
    MssqlProvider {
        endpoint_host: "127.0.0.1".into(),
        endpoint_port: port,
        environment: Environment::Development,
        pool: Arc::new(Pool::new(config(port, database, password, "culprit"), None)),
    }
}

async fn instance(provider: &MssqlProvider) -> SharedResourceIdentity {
    let resources = provider
        .telemetry_resources(&ResourceDiscoveryRequest {
            kind: SharedResourceKind::Instance,
            budget: CollectionBudget::default(),
        })
        .await
        .unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0].collections,
        [
            TelemetryCollection::Snapshot(SnapshotDataset::Jobs),
            TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets),
            TelemetryCollection::Snapshot(SnapshotDataset::Operational),
        ]
    );
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    resources[0].identity.clone()
}

#[tokio::test]
#[ignore = "requires the owned loopback SQL Server provider fixture"]
async fn shared_instance_capture_and_scan_use_the_same_native_memory_and_scheduler_facts() {
    let fixture = OwnedSourceFixture::open("culprit").await.unwrap();
    let master = provider(fixture.port, "master", fixture.password());
    let member = provider(fixture.port, &fixture.database, fixture.password());
    let identity = instance(&master).await;
    assert_eq!(identity, instance(&member).await, "the member database is a route, not the shared owner");

    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: identity.clone() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget { page_rows: 7, ..Default::default() },
    };
    let mut source = member.telemetry_source(&request).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(description.target, CollectionTarget::Shared(identity));
    let snapshot = description.observation.snapshot().unwrap();
    let expected_sources = point::shared_families(SnapshotDataset::Operational)
        .unwrap()
        .into_iter()
        .map(|family| family.source().name)
        .collect::<BTreeSet<_>>();
    assert_eq!(snapshot.sources.len(), expected_sources.len(), "each registered family appears once");
    assert_eq!(snapshot.sources.iter().map(|source| source.source.as_str()).collect::<BTreeSet<_>>(), expected_sources);
    for name in [crate::sources::MEMORY_CLERKS.name, crate::sources::SCHEDULERS.name] {
        let coverage = snapshot.sources.iter().find(|source| source.source == name).unwrap();
        assert_eq!(coverage.state, SourceState::Collected, "{coverage:?}");
        assert!(coverage.complete_scope && coverage.records > 0);
    }

    let capture = CaptureIdentity { observation_id: "owned-shared-native".into(), revision: None };
    let mut counter_sources = BTreeSet::new();
    let mut instance_points = 0;
    let mut cursor = None;
    let mut records = 0;
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("owned native capture must not silently truncate a shared resource");
        };
        records += page.len();
        for record in page {
            let row = ExportedRecord::new(&description, &capture, record).unwrap();
            if let TelemetryRecord::Operational(OperationalObservation { source, fact, .. }) = row.record {
                match fact {
                    OperationalFact::CounterSet(set) => {
                        assert_eq!(set.scope, MeasurementScope::Server);
                        assert!(set.is_valid());
                        counter_sources.insert(source);
                    }
                    OperationalFact::MssqlInstanceResources(point) => {
                        assert_eq!(source, INSTANCE_RESOURCES_SOURCE);
                        assert!(point.is_valid());
                        assert!(point.matches_context(&NativeReadContext::from_snapshot(&description).unwrap()));
                        instance_points += 1;
                    }
                    _ => {}
                }
            }
        }
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(records, snapshot.records as usize);
    assert_eq!(instance_points, 1, "the seventh family retains its raw instance point without admission");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(member.source_shutdown(), SourceShutdown::Quiescent);

    let probe = member.pool.observe(|reader| Box::pin(crate::probe::run(reader))).await.unwrap();
    let not_requested = crate::resource_counters::collect(member.pool.clone(), &probe, false).await;
    assert!(not_requested.counters.is_none() && not_requested.instance_resources.is_none());
    let shared = crate::resource_counters::collect(member.pool.clone(), &probe, true).await;
    let scan_counters = shared.counters.unwrap();
    let coverage = shared.coverage;
    assert!(scan_counters.iter().all(|set| set.is_valid() && set.scope == MeasurementScope::Server));
    for name in [crate::sources::MEMORY_CLERKS.name, crate::sources::SCHEDULERS.name] {
        assert!(counter_sources.contains(name), "native collector source {name}");
        assert_eq!(
            coverage.outcomes().into_iter().find(|source| source.source == name).unwrap().state,
            SourceState::Collected,
            "native scan source {name}",
        );
    }
    assert_eq!(member.source_shutdown(), SourceShutdown::Quiescent);
}
