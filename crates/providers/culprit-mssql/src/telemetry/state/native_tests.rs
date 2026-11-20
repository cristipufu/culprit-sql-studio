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
            TelemetryCollection::DeadlockHistory,
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
    // This independent fixture oracle is outside source accounting. Enabling the feature requires a restart,
    // so its native value remains fixed throughout the original process-qualified source acquisition.
    let expected_metadata_enabled = member
        .pool
        .observe(|reader| {
            Box::pin(async move {
                let row = crate::read::one(
                    reader,
                    "SELECT /* culprit */ CONVERT(int,SERVERPROPERTY('IsTempDbMetadataMemoryOptimized')) AS metadata_enabled",
                )
                .await?;
                crate::read::optional_boolean(&row, "metadata_enabled")
            })
        })
        .await
        .unwrap();
    assert!(
        expected_metadata_enabled.is_some(),
        "the owned supported SQL instance must expose its native feature state"
    );

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
    for name in [crate::sources::MEMORY_CLERKS.name, crate::sources::SCHEDULERS.name, crate::sources::LOG_CAPACITY.name]
    {
        let coverage = snapshot.sources.iter().find(|source| source.source == name).unwrap();
        assert_eq!(coverage.state, SourceState::Collected, "{coverage:?}");
        assert!(coverage.complete_scope && coverage.records > 0);
    }
    let metadata =
        snapshot.sources.iter().find(|source| source.source == crate::sources::TEMPDB_METADATA_MEMORY.name).unwrap();
    let metadata_limit = (MAX_MSSQL_TEMPDB_METADATA_CONSUMERS + 1) as u32;
    assert!(matches!(metadata.state, SourceState::Collected | SourceState::Partial), "{metadata:?}");
    assert!((1..=metadata_limit).contains(&metadata.records));
    assert_eq!(metadata.complete_scope, metadata.state == SourceState::Collected);
    assert_eq!((metadata.error_code, metadata.stopped), (None, None));
    if metadata.state == SourceState::Partial {
        assert_eq!(metadata.records, metadata_limit, "one feature plus the bounded consumer prefix, no overflow row");
    }

    let capture = CaptureIdentity { observation_id: "owned-shared-native".into(), revision: None };
    let mut counter_sources = BTreeSet::new();
    let mut instance_points = 0;
    let mut tempdb_log_points = 0;
    let mut tempdb_data_points = 0;
    let mut tempdb_metadata_features = 0;
    let mut tempdb_metadata_consumers = 0;
    let context = NativeReadContext::from_snapshot(&description).unwrap();
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
                        if source == crate::sources::TEMPDB_METADATA_MEMORY.name {
                            if set.identity.qualifiers.get("kind").map(String::as_str)
                                == Some(MSSQL_TEMPDB_METADATA_FEATURE_KIND)
                            {
                                let feature = TempdbMetadataFeature::from_original(&set, &context).unwrap();
                                assert_eq!(feature.enabled(), expected_metadata_enabled);
                                tempdb_metadata_features += 1;
                            } else {
                                TempdbMetadataConsumer::from_original(&set, &context).unwrap();
                                tempdb_metadata_consumers += 1;
                            }
                        }
                        counter_sources.insert(source);
                    }
                    OperationalFact::MssqlInstanceResources(point) => {
                        assert_eq!(source, INSTANCE_RESOURCES_SOURCE);
                        assert!(point.is_valid());
                        assert!(point.matches_context(&NativeReadContext::from_snapshot(&description).unwrap()));
                        instance_points += 1;
                    }
                    OperationalFact::Space(space) => {
                        assert_eq!(space.scope, culprit_core::FileScope::Server);
                        assert_eq!(space.database, None);
                        match space.population {
                            StoragePopulation::SharedTempdbTransactionLog => {
                                assert_eq!(source, culprit_core::source_id::LOG_CAPACITY);
                                let total = space.total_bytes.as_ref().unwrap().parse::<u64>().unwrap();
                                let used = space.used_bytes.as_ref().unwrap().parse::<u64>().unwrap();
                                assert!(total > 0 && used <= total);
                                assert!(space.parts.is_empty());
                                tempdb_log_points += 1;
                            }
                            StoragePopulation::SharedTempdbDataFiles => {
                                assert_eq!(source, culprit_core::source_id::TEMP_CAPACITY);
                                tempdb_data_points += 1;
                            }
                            _ => panic!("an app-database allocation cannot enter the shared tempdb capture"),
                        }
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
    assert_eq!(tempdb_log_points, 1, "native tempdb log visibility must be proved, never skipped");
    assert_eq!(tempdb_data_points, 1, "data and log retain independent source populations");
    assert_eq!(tempdb_metadata_features, 1, "native feature state is measured once, including when disabled");
    assert_eq!(tempdb_metadata_features + tempdb_metadata_consumers, metadata.records);
    if expected_metadata_enabled == Some(true) {
        assert!(tempdb_metadata_consumers > 0, "the enabled native control must exercise consumer decoding");
    } else {
        assert_eq!(tempdb_metadata_consumers, 0, "disabled metadata cannot manufacture consumer measurements");
    }
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

#[tokio::test]
#[ignore = "requires the existing nonce-owned boxed SQL Server fixture; source cost remains unqualified"]
async fn owned_boxed_settings_use_the_actual_factory_and_same_native_decoder_through_sealed_outbox() {
    use crate::test_support::{owned_captures::OwnedCaptures, owned_case::finish_case};
    use culprit_collector::capture::CollectionCancellation;
    use std::time::Duration;
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        tokio::time::Instant::now() + Duration::from_secs(10),
        &cancellation,
        OwnedSourceFixture::open("culprit-local-settings"),
    )
    .await;
    let fixture = fixture.expect("existing nonce-owned SQL endpoint admission");
    assert!(!expired);
    let factory = Factory(provider(fixture.port, &fixture.database, fixture.password()));
    let mut originals = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port).unwrap();
    let (expired,outcome)=finish_case(tokio::time::Instant::now()+Duration::from_secs(20),&cancellation,async {
        let captured=originals.capture_with_factory(CollectionSelection::Snapshot{dataset:SnapshotDataset::Settings},&cancellation,&factory).await?;
        let source=captured.source()?;let context=NativeReadContext::from_snapshot(source)?;
        assert!(context.has_qualified_local_owner());assert!(source.target.database().unwrap().incarnation.is_none());
        assert_eq!(source.target.database().unwrap().database,fixture.database);
        let sources=&source.observation.snapshot().unwrap().sources;
        assert_eq!(sources.len(),4);
        for kind in crate::settings::Kind::ALL {
            let coverage=sources.iter().find(|source|source.source==kind.source().name).unwrap();
            assert_eq!(coverage.state,SourceState::Collected,"{coverage:?}");assert!(coverage.complete_scope&&coverage.records>0);
        }
        let mut server=0;let mut database=0;
        for (index,row) in captured.records.iter().enumerate(){
            captured.reference(index)?.validate_canonical_record(source,&row.record)?;
            let TelemetryRecord::Setting(value)=&row.record else{panic!("native Setting observations only")};
            assert!(value.is_valid());assert!(!value.name.contains("culprit_settings_frame"));
            if value.namespace=="sys.configurations" {assert_eq!(value.scope,MeasurementScope::Server);assert!(value.value_in_use.is_some());server+=1;}
            else {assert_eq!(value.scope,MeasurementScope::Database);database+=1;}
        }
        assert_eq!(server,4);assert!(database>0);assert!(captured.manifest.source_queries<=settings_budget(&CollectionBudget::upper_limits()).max_queries);
        originals.verify_retained(&captured).await?;
        assert_eq!(factory.0.source_shutdown(),SourceShutdown::Quiescent);
        eprintln!("owned boxed Settings: original_source_queries={}, server_settings={}, database_settings={}; cost_qualified=false",captured.manifest.source_queries,server,database);
        Ok::<(),culprit_core::Error>(())
    }).await;
    outcome.expect("the actual native local Settings profile must qualify every expected family");
    assert!(!expired);
}

struct Factory(MssqlProvider);
#[async_trait::async_trait]
impl culprit_collector::capture::SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.0.telemetry_source(request)
    }
}

#[tokio::test]
#[ignore = "requires the existing nonce-owned SQL Server16+ fixture; selected counter source cost remains unqualified"]
async fn owned_selected_database_performance_counters_use_the_real_factory_and_sealed_original_outbox() {
    use crate::test_support::{owned_captures::OwnedCaptures, owned_case::finish_case};
    use culprit_collector::capture::CollectionCancellation;
    use std::time::Duration;
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        tokio::time::Instant::now() + Duration::from_secs(10),
        &cancellation,
        OwnedSourceFixture::open("culprit-database-counters"),
    )
    .await;
    let fixture = fixture.expect("existing positively nonce-owned SQL fixture admission");
    assert!(!expired);
    let factory = Factory(provider(fixture.port, &fixture.database, fixture.password()));
    let mut originals = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port).unwrap();
    let (expired, outcome) = finish_case(
        tokio::time::Instant::now() + Duration::from_secs(20),
        &cancellation,
        async {
            let captured = originals
                .capture_with_factory(
                    CollectionSelection::Snapshot { dataset: SnapshotDataset::DatabasePerformanceCounters },
                    &cancellation,
                    &factory,
                )
                .await?;
            crate::test_support::save_cost_receipt(
                &crate::test_support::receipt_directory(),
                "database-performance-counters-original",
                &serde_json::json!({
                    "manifest": captured.manifest,
                    "original_records": captured.records,
                    "capture_export_ms": captured.capture_export_ms,
                    "native_cost_approved": false,
                }),
            )?;
            let source = captured.source()?;
            let context = database_performance_context(source)?;
            assert_eq!(context.target.database().unwrap().database, fixture.database);
            assert!(context.target.database().unwrap().incarnation.is_none());
            assert_eq!(context.database_writable, Some(true));
            let SourceObservation::DatabasePerformanceCounters { native_database, snapshot, .. } =
                &source.observation
            else {
                panic!("native selected population header")
            };
            assert!(validate_database_performance_identity(native_database));
            assert_eq!(snapshot.records, 1);
            assert_eq!(snapshot.sources.len(), 1);
            assert_eq!(snapshot.sources[0].state, SourceState::Collected);
            assert!(snapshot.sources[0].complete_scope);
            assert_eq!(captured.records.len(), 1);
            let record = &captured.records[0].record;
            captured.reference(0)?.validate_canonical_record(source, record)?;
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
                record
            else {
                panic!("original native counters")
            };
            assert!(database_performance_sample_fits_source(set, source));
            assert_eq!(&set.identity, native_database.as_ref());
            assert_eq!(set.counters.len(), 3);
            assert_eq!(set.scope, MeasurementScope::Database);
            assert!(set.counters.iter().all(|counter| {
                counter.native_instance.as_deref() == Some(fixture.database.as_str())
                    && counter.reset == culprit_core::ResetMarker::Unknown
                    && counter.value.integer().is_some()
            }));
            assert!(set.ratios.is_empty());
            assert!(
                captured.manifest.source_queries
                    <= database_counter_budget(&CollectionBudget::upper_limits()).max_queries
            );
            originals.verify_retained(&captured).await?;
            assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
            eprintln!(
                "owned database counters: original_source_queries={}, native_counter_rows=3, original_retained=true, cost_qualified=false",
                captured.manifest.source_queries,
            );
            Ok::<(), culprit_core::Error>(())
        },
    )
    .await;
    outcome.expect("all three genuine selected database native counters must be captured");
    assert!(!expired);
}
