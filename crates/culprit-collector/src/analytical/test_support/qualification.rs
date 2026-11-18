//! Controlled executable issuer for lifecycle tests. These synthetic facts qualify no built-in/native profile.
use super::*;
use crate::{
    capture::SourceFactory,
    outbox::{NativeQualificationDecision, NativeQualificationEvidence, NativeQualificationSource},
};
use culprit_core::{ComponentVersion, RecipeQualificationGap};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone, Copy)]
pub enum QualificationFixtureCase {
    Approve,
    Refuse,
    WrongDigest,
    WrongOrdinal,
    WrongBudget,
    Cancelled,
    Unconfirmed,
}

#[derive(Clone)]
pub struct QualificationFixture {
    pub recipe: RecipeDescriptor,
    pub source: CollectionDescription,
    pub component: ComponentVersion,
    pub scope: NativeSourceReadScope,
    pub case: QualificationFixtureCase,
    record: TelemetryRecord,
    metrics: ReadWorkMetrics,
    state: Arc<FixtureState>,
}
#[derive(Default)]
struct FixtureState {
    opens: AtomicUsize,
    assessments: AtomicUsize,
    closed: AtomicBool,
}
impl QualificationFixture {
    pub fn new(case: QualificationFixtureCase) -> Self {
        let original = investigation_fixture();
        let mut source = original.source;
        let CollectionTarget::Database(database) = &mut source.target else { panic!() };
        database.incarnation = Some("controlled-qualification-database-incarnation".into());
        let scope = NativeSourceReadScope {
            target: database.clone(),
            partition: replica_activity_evidence_cases().remove(0).0.partition,
            database_writable: true,
        };
        assert!(scope.is_valid());
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        header.dataset = SnapshotDataset::QueryCompilation;
        let receipt = original.intent.qualification;
        Self {
            recipe: RecipeDescriptor {
                id: "fixture.native-compilation".into(),
                version: 1,
                purpose: RecipePurpose::Investigation,
                selector: SourceReadQuestion::CompilationHistory.selector(),
                subjects: vec![RecipeSubject::Database],
                budget: CollectionBudget::default(),
                cost: RecipeCostQualification::Unqualified,
            },
            source,
            component: receipt.component,
            scope,
            case,
            record: original.record.record,
            metrics: receipt.read_metrics,
            state: Arc::new(FixtureState::default()),
        }
    }
    pub fn request(&self) -> CollectionRequest {
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::QueryCompilation },
            budget: CollectionBudget { max_queries: 8, ..Default::default() },
        }
    }
    pub fn opens(&self) -> usize {
        self.state.opens.load(Ordering::SeqCst)
    }
    pub fn assessments(&self) -> usize {
        self.state.assessments.load(Ordering::SeqCst)
    }
    pub fn closed(&self) -> bool {
        self.state.closed.load(Ordering::SeqCst)
    }
    pub fn telemetry_source(&self) -> Box<dyn TelemetrySource> {
        self.state.opens.fetch_add(1, Ordering::SeqCst);
        self.state.closed.store(false, Ordering::SeqCst);
        Box::new(FixtureRead { fixture: self.clone(), rows: Some(vec![self.record.clone()]), queries: 0 })
    }
    /// Exact controlled native artifact meaning; neither elapsed time nor a returned-row ceiling grants approval.
    pub fn verifies(&self, receipt: &RecipeCostReceipt, source: &CollectionDescription, row: &ExportedRecord) -> bool {
        receipt.scope == RecipeCostScope::NativeRead(self.scope.clone())
            && source == &self.source
            && row.record == self.record
            && receipt.component == self.component
            && receipt.read_metrics.templates == self.metrics.templates
    }
}
impl NativeQualificationSource for QualificationFixture {
    fn source_factory<'a>(&'a self, _: &'a CaptureIdentity) -> Box<dyn SourceFactory + 'a> {
        Box::new(self.clone())
    }
    fn assess(&self, evidence: NativeQualificationEvidence<'_>) -> Result<NativeQualificationDecision> {
        self.state.assessments.fetch_add(1, Ordering::SeqCst);
        assert!(self.closed(), "source close precedes native assessment and local installation");
        if matches!(self.case, QualificationFixtureCase::Refuse) {
            return Ok(NativeQualificationDecision::Refuse(vec![RecipeQualificationGap::NativePolicyRejected]));
        }
        let mut receipt = RecipeCostReceipt {
            recipe_id: self.recipe.id.clone(),
            recipe_version: self.recipe.version,
            selector: self.recipe.selector,
            scope: RecipeCostScope::NativeRead(self.scope.clone()),
            component: self.component.clone(),
            tested_budget: evidence.manifest.request.budget.clone(),
            evidence: evidence.captured.reference(0)?,
            read_metrics: evidence.manifest.read_metrics.clone().unwrap(),
        };
        match self.case {
            QualificationFixtureCase::WrongDigest => receipt.evidence.payload_sha256 = digest(b"another-body"),
            QualificationFixtureCase::WrongOrdinal => receipt.evidence.row_ordinal += 1,
            QualificationFixtureCase::WrongBudget => receipt.tested_budget.max_queries += 1,
            _ => (),
        }
        Ok(NativeQualificationDecision::Approve(Box::new(receipt)))
    }
}
#[async_trait::async_trait]
impl SourceFactory for QualificationFixture {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        assert!(self.source.matches_request(request));
        Ok(self.telemetry_source())
    }
}
struct FixtureRead {
    fixture: QualificationFixture,
    rows: Option<Vec<TelemetryRecord>>,
    queries: u32,
}
#[async_trait::async_trait]
impl TelemetrySource for FixtureRead {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.queries += 1;
        Ok(self.fixture.source.clone())
    }
    fn query_count(&self) -> u32 {
        self.queries
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        Some(self.fixture.metrics.clone())
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert_eq!(stream, TelemetryStream::OperationalSamples);
        assert!(cursor.is_none());
        self.queries += 1;
        if matches!(self.fixture.case, QualificationFixtureCase::Cancelled) {
            return Err(CollectionStop::Cancelled.error());
        }
        Ok(CollectionRead::Page { records: self.rows.take().unwrap(), next_cursor: None })
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.fixture.state.closed.store(true, Ordering::SeqCst);
        if matches!(self.fixture.case, QualificationFixtureCase::Unconfirmed) {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        }
    }
}
