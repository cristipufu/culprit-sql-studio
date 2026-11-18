//! Controlled source facts exercise real capture, sealed Outbox restore and committed archive preparation.
//! No SQL is executed and no native support/cost qualification is claimed.
use super::*;
use crate::{
    capture::{CollectionCancellation, SourceFactory, collect},
    delivery::{file_store::FileRecoveryStore, recovery::stage_and_commit},
    outbox::Outbox,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub fn resource_io_evidence(accounting: DataIoAccounting) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (manifest, row) = fixture();
    let mut native = row.record;
    let TelemetryRecord::ResourceSample(point) = &mut native else { unreachable!() };
    point.data_io_accounting = accounting;
    point.metrics[0].key = "data-io".into();
    point.metrics[0].value = Some(99.0);
    (manifest.source, vec![native])
}

/// Controlled transport facts, not a claim that the MSSQL provider admits secondary resource export.
/// Its existing primary-only native policy is tested independently in the provider.
pub fn resource_role_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    use MssqlResourceReplicaRole::*;
    use ResourceLimitBasis::{Absolute, DatabaseLimit, InstanceLimit, PoolLimit};
    use ResourceStatistic::{Maximum, Mean, Point};
    use ResourceUnit::{Cores, DatabaseTransactionUnits, Percent};
    use culprit_core::MeasurementScope::{Database, Server};
    [None, Some(Primary), Some(HighAvailabilitySecondary), Some(GeoReplicationForwarder), Some(NamedReplica)]
        .into_iter()
        .map(|role| {
            let (source, mut rows) = resource_io_evidence(DataIoAccounting::DataFiles);
            let TelemetryRecord::ResourceSample(point) = &mut rows[0] else { unreachable!() };
            point.replica_id = None;
            point.replica_role = role.map(NativeResourceReplicaRole::MssqlDbResourceStats);
            point.metrics = [
                ("cpu", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(0.0)),
                ("data-io", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(20.0)),
                ("log-write", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(90.0)),
                ("memory", Database, Percent, Mean, Some(15_000), DatabaseLimit, None),
                ("workers", Database, Percent, Maximum, Some(15_000), DatabaseLimit, Some(55.0)),
                ("sessions", Database, Percent, Maximum, Some(15_000), DatabaseLimit, Some(30.0)),
                ("xtp-storage", Database, Percent, Point, None, PoolLimit, Some(0.0)),
                ("cpu", Server, Percent, Mean, Some(60_000), InstanceLimit, Some(70.0)),
                ("memory", Server, Percent, Mean, None, InstanceLimit, Some(40.0)),
                ("dtu-limit", Database, DatabaseTransactionUnits, Point, None, Absolute, None),
                ("cpu-limit", Database, Cores, Point, None, Absolute, Some(10.0)),
            ]
            .into_iter()
            .map(|(key, scope, unit, statistic, period_ms, limit_basis, value)| ResourceMetricSample {
                key: key.into(),
                scope,
                unit,
                statistic,
                period_ms,
                limit_basis,
                value,
            })
            .collect();
            (source, rows)
        })
        .collect()
}

pub fn rbio_wait_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (manifest, _) = fixture();
    let names = [
        "RBIO_RG_STORAGE",
        "RBIO_RG_REPLICA",
        "RBIO_RG_LOCALDESTAGE",
        "RBIO_RG_DESTAGE",
        "RBIO_RG_GEOREPLICA",
        "RBIO_RG_STORAGE_CHECKPOINT",
        "RBIO_RG_MIGRATION_TARGET",
        "FUTURE_NATIVE_WAIT",
    ];
    let rows: Vec<_> = names
        .into_iter()
        .map(|name| {
            TelemetryRecord::WaitCounter(WaitCounterObservation {
                mechanism: culprit_core::Engine::Mssql.spec().native_wait_mechanism(name),
                source: culprit_core::source_id::WAITS.into(),
                wait_type: name.into(),
                scope: culprit_core::MeasurementScope::Database,
                reset: culprit_core::ResetMarker::Unknown,
                waiting_tasks_count: "9007199254740993".into(),
                wait_time_ms: "9007199254740993".into(),
                signal_wait_time_ms: "0".into(),
                max_wait_time_ms: "7".into(),
            })
        })
        .collect();
    let source = CollectionDescription {
        source: "wait-counters".into(),
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::WaitCounters,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: rows.len() as u32,
            sources: vec![StateSourceCoverage::collected(culprit_core::source_id::WAITS.into(), rows.len() as u32)],
        }),
        ..manifest.source
    };
    (source, rows)
}

struct Source {
    description: CollectionDescription,
    rows: Vec<TelemetryRecord>,
    closed: Arc<AtomicBool>,
}
#[async_trait]
impl SourceFactory for Source {
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(Self {
            description: self.description.clone(),
            rows: self.rows.clone(),
            closed: self.closed.clone(),
        }))
    }
}
#[async_trait]
impl TelemetrySource for Source {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        Ok(self.description.clone())
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert!(cursor.is_none());
        Ok(CollectionRead::Page {
            records: self.rows.iter().filter(|row| row.stream() == stream).cloned().collect(),
            next_cursor: None,
        })
    }
    fn query_count(&self) -> u32 {
        1
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.store(true, Ordering::SeqCst);
        SourceShutdown::Quiescent
    }
}

pub async fn collected_resource_recovery(
    destination: &str,
    schema: &str,
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
) -> Recovery {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let selection = match &source.observation {
        SourceObservation::RetainedSamples(history) => {
            CollectionSelection::ResourceHistory { from: history.from.clone(), until: history.until.clone() }
        }
        SourceObservation::Snapshot(snapshot) => CollectionSelection::Snapshot { dataset: snapshot.dataset },
        _ => panic!("resource history or point counter fixture"),
    };
    let request =
        CollectionRequest { subject: CollectionSubject::Database, selection, budget: CollectionBudget::default() };
    let closed = Arc::new(AtomicBool::new(false));
    let factory = Source { description: source.clone(), rows, closed: closed.clone() };
    let mut lease = outbox.reserve("resource-fixture", &request).await.unwrap();
    let directory = lease.directory().to_owned();
    let identity = lease.identity().clone();
    let manifest = collect(
        &culprit_read::admission::SourceGate::default(),
        &factory,
        request,
        &directory,
        identity.clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(manifest.references.integrity_complete);
    lease.finish().await.unwrap();
    drop(outbox);
    let mut restored = Outbox::open(&path).unwrap();
    restored.recover().await.unwrap();
    let sealed = restored.verify(&identity.observation_id).await.unwrap();
    assert_eq!(sealed.capture, identity);
    assert_eq!(sealed.description.as_ref(), Some(&source));
    let contract = Contract::new("resource-fixture".into(), "shard".into(), destination.into(), schema.into()).unwrap();
    let prepared = restored.delivery().prepare(&identity.observation_id, contract.clone(), vec![]).await.unwrap();
    let store = FileRecoveryStore::create(&root.path().join("archive"), contract).unwrap();
    let owner = store.acquire().await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let mut objects = BTreeMap::new();
    for batch in &committed.manifest().batches {
        objects.insert(batch.object.clone(), std::fs::read(prepared.batch_path(batch)).unwrap());
    }
    Recovery {
        manifest: committed.manifest().clone(),
        objects,
        fault: Mutex::new(Fault::None),
        object_reads: AtomicUsize::new(0),
    }
}
