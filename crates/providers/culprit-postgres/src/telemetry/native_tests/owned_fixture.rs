//! Shared owned-loopback admission and actual collector receipts; fixture-specific writers live elsewhere.
use super::{Factory, spec};
use crate::{
    connect::Connector,
    read::{NativeRead, QueryRead, ReadSession},
};
use culprit_collector::{
    capture::{CollectionCancellation, collect, collect_once},
    outbox::Outbox,
};
use culprit_core::{ConnectionSpec, Provider, telemetry::*};
use culprit_read::admission::SourceGate;

pub(super) fn owner() -> String {
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("explicit owned fixture nonce");
    assert!(owner.len() == 24 && owner.bytes().all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase()));
    owner
}

pub(super) async fn specification(database: &str, budget: CollectionBudget) -> ConnectionSpec {
    let specification = spec();
    assert_eq!(specification.database.as_deref(), Some(database));
    assert!(specification.port.is_some_and(|port| port >= 1024 && port != 5432));
    let owner = owner();
    let mut reader = ReadSession::new(Connector::new(&specification).unwrap(), budget);
    let result = reader
        .query("SELECT current_database() AS database,pg_catalog.current_setting('cluster_name') AS marker", 1)
        .await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let rows = result.expect("bounded owned fixture admission");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text("database").as_deref(), Some(database));
    // Existing supervisor-owned PostgreSQL and Timescale fixtures share this native startup marker.
    assert_eq!(rows[0].text("marker"), Some(format!("culprit_owned_{owner}")));
    assert_settled(&reader.metrics().unwrap(), reader.queries());
    specification
}

pub(super) struct Captured {
    pub manifest: CollectionManifest,
    pub manifest_sha256: String,
    pub records: Vec<ExportedRecord>,
    _owner: Option<(Outbox, tempfile::TempDir)>,
}

pub(super) async fn capture(
    specification: &ConnectionSpec,
    request: CollectionRequest,
    cancellation: &CollectionCancellation,
) -> Captured {
    let factory = Factory(crate::PostgresProvider::new(specification).unwrap());
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("capture");
    let result = collect_once(&SourceGate::default(), &factory, request, &output, cancellation).await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = result.expect("native capture");
    completed(&output, manifest)
}

/// A diagnostic parent must be acquired under an actual durable revision, not relabeled after collection.
pub(super) async fn capture_parent(
    specification: &ConnectionSpec,
    request: CollectionRequest,
    cancellation: &CollectionCancellation,
) -> Captured {
    let directory = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()).unwrap();
    let mut captured = capture_reserved(&mut outbox, specification, request, cancellation).await;
    captured._owner = Some((outbox, directory));
    captured
}

async fn capture_reserved(
    outbox: &mut Outbox,
    specification: &ConnectionSpec,
    request: CollectionRequest,
    cancellation: &CollectionCancellation,
) -> Captured {
    let factory = Factory(crate::PostgresProvider::new(specification).unwrap());
    let mut lease = outbox.reserve("owned-native-parent", &request).await.unwrap();
    let identity = lease.identity().clone();
    let output = lease.directory().to_owned();
    let result =
        collect(&SourceGate::default(), &factory, request, &output, identity.clone(), cancellation, Some(&mut lease))
            .await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = result.expect("native diagnostic parent capture");
    assert_eq!(manifest.capture, identity);
    assert!(manifest.capture.revision.is_some(), "the actual acquisition owns its durable revision");
    lease.finish().await.unwrap();
    // Retain the sealed representation that later readback authenticates, including native numeric metadata.
    let manifest = outbox.verify(&identity.observation_id).await.unwrap();
    assert_eq!(manifest.capture, identity);
    completed(&output, manifest)
}

fn completed(output: &std::path::Path, manifest: CollectionManifest) -> Captured {
    assert_eq!(
        manifest.status,
        CollectionStatus::Complete,
        "description admitted: {}; native statements: {}",
        manifest.description.is_some(),
        manifest.source_queries
    );
    let source = manifest.description.as_ref().expect("original native source");
    let mut records = Vec::new();
    for file in &manifest.files {
        for line in std::fs::read_to_string(output.join(&file.file)).unwrap().lines() {
            let exported: ExportedRecord = serde_json::from_str(line).unwrap();
            assert_eq!(exported.record.stream(), file.stream);
            assert_eq!(exported, ExportedRecord::new(source, &manifest.capture, exported.record.clone()).unwrap());
            records.push(exported);
        }
    }
    assert_settled(manifest.read_metrics.as_ref().unwrap(), manifest.source_queries);
    let manifest_sha256 = digest(&std::fs::read(output.join("manifest.json")).unwrap());
    Captured { manifest, manifest_sha256, records, _owner: None }
}

impl Captured {
    pub(super) fn census(&self) -> culprit_core::SessionCensusReport {
        let original = self
            .records
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let TelemetryRecord::Activity(observation) = &row.record else { panic!("one native Activity source") };
                (observation.as_ref(), self.reference(index))
            })
            .collect::<Vec<_>>();
        let rows = original
            .iter()
            .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect::<Vec<_>>();
        let frame = ActivityFrameEvidence::new(self.source(), &rows, true).unwrap();
        culprit_checks::session_census::explain_frame(&frame).unwrap()
    }

    pub(super) async fn verify_original(&mut self) {
        let (outbox, _) = self._owner.as_mut().expect("the original durable Outbox owner remains held");
        let retained = outbox.verify(&self.manifest.capture.observation_id).await.unwrap();
        assert_eq!(serde_json::to_value(retained).unwrap(), serde_json::to_value(&self.manifest).unwrap());
        for (index, row) in self.records.iter().enumerate() {
            self.reference(index).validate_canonical_record(self.source(), &row.record).unwrap();
        }
    }

    pub(super) async fn followup(
        &mut self,
        specification: &ConnectionSpec,
        request: CollectionRequest,
        cancellation: &CollectionCancellation,
    ) -> Captured {
        let (outbox, _) = self._owner.as_mut().expect("the actual original Outbox owns focused authentication");
        capture_reserved(outbox, specification, request, cancellation).await
    }

    pub(super) fn source(&self) -> &CollectionDescription {
        self.manifest.description.as_ref().unwrap()
    }

    pub(super) fn reference(&self, index: usize) -> DiagnosticEvidenceRef {
        let exported = &self.records[index];
        let ordinal =
            self.records[..index].iter().filter(|row| row.record.stream() == exported.record.stream()).count() as u32;
        DiagnosticEvidenceRef::from_export(self.source(), &self.manifest_sha256, exported, ordinal).unwrap()
    }
}

pub(super) fn metrics_are_settled(metrics: &ReadWorkMetrics, queries: u32) -> bool {
    metrics.is_valid()
        && !metrics.has_unconfirmed_work()
        && metrics.active_tasks == 0
        && metrics.untracked_tasks == 0
        && metrics.templates.iter().map(|template| template.completed_tasks).sum::<u32>() == queries
}
pub(super) fn assert_settled(metrics: &ReadWorkMetrics, queries: u32) {
    assert!(
        metrics_are_settled(metrics, queries),
        "the original native metrics must prove every dispatched task settled"
    );
}
