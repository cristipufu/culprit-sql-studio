//! Outbox transactions retain file ownership on a blocking worker while async source cancellation remains runnable.
use super::{Files, Reservation, problem};
use crate::{blocking_state::BlockingState, delivery};
use culprit_core::{Result, telemetry::*};
use std::path::Path;

pub struct Outbox {
    pub(super) files: BlockingState<Files>,
    recovery_store_id: Option<String>,
}
/// Delivery can inspect and prepare sealed captures while the source runner owns the capture interface.
/// Every handle retains the same filesystem owner and serialized journal access.
#[derive(Clone)]
pub struct OutboxDelivery {
    files: BlockingState<Files>,
    recovery_store_id: Option<String>,
}
pub struct CaptureLease<'a> {
    pub(super) outbox: &'a mut Outbox,
    reservation: Reservation,
}
impl Outbox {
    pub fn create(path: &Path, limits: OutboxLimits) -> Result<Self> {
        Ok(Self::from_files(Files::create(path, limits)?))
    }
    pub fn create_remote(path: &Path, limits: OutboxLimits, store_id: &str) -> Result<Self> {
        Ok(Self::from_files(Files::create_remote(path, limits, store_id)?))
    }
    /// Initialize allocation from a freshly acquired remote epoch and the recovered sequence high-water mark.
    /// The caller must establish source quiescence separately before resuming after volume loss. This method
    /// never clears an existing source-work debt and refuses local-only namespaces.
    pub async fn begin_remote_epoch(&mut self, fence: &delivery::recovery::Fence, high_sequence: u64) -> Result<()> {
        self.files
            .run(|| {
                let fence = fence.clone();
                move |files| files.begin_remote_epoch(&fence, high_sequence)
            })
            .await
    }
    /// Bind allocation for local-only diagnostics while native source activation remains independently fenced.
    pub async fn begin_diagnostic_epoch(
        &mut self,
        fence: &delivery::recovery::Fence,
        high_sequence: u64,
    ) -> Result<()> {
        self.files
            .run(|| {
                let fence = fence.clone();
                move |files| files.begin_diagnostic_epoch(&fence, high_sequence)
            })
            .await
    }
    pub async fn restore_diagnostic_input(
        &mut self,
        connection: &str,
        recovered: delivery::diagnostic_recovery::RecoveredDiagnosticInput,
        recovery: &dyn delivery::recovery::RecoveryStore,
    ) -> Result<()> {
        let (range, rows) = recovered.into_parts();
        if self.recovery_store_id() != Some(recovery.id()) || range.store_id() != recovery.id() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let parent = delivery::diagnostic_recovery::capture_reference(range.manifest())?;
        let generation =
            self.files.run(|| |files| super::investigations::retirement::generation(&files.journal.conn)).await?;
        if !recovery.diagnostic_input_pending(&parent, DIAGNOSTIC_CONTROLLER_VERSION).await? {
            return Ok(());
        }
        let input = super::investigations::RestoredInput {
            store_id: range.store_id().into(),
            parent,
            source: range.manifest().source.clone(),
            connection: connection.into(),
            rows,
        };
        self.files.run(|| move |files| files.restore_diagnostic_input_at_generation(input, generation)).await
    }
    /// Cache authenticated original evidence without adding or acknowledging diagnostic work.
    pub async fn cache_diagnostic_evidence(
        &mut self,
        connection: &str,
        recovered: delivery::diagnostic_recovery::RecoveredDiagnosticInput,
    ) -> Result<()> {
        let (range, rows) = recovered.into_parts();
        let parent = delivery::diagnostic_recovery::capture_reference(range.manifest())?;
        let input = super::investigations::RestoredInput {
            store_id: range.store_id().into(),
            parent,
            source: range.manifest().source.clone(),
            connection: connection.into(),
            rows,
        };
        self.files.run(|| move |files| files.cache_diagnostic_evidence(input)).await
    }
    pub async fn restore_diagnostics(
        &mut self,
        recovered: delivery::diagnostic_recovery::RecoveredDiagnosticCheckpoint,
    ) -> Result<()> {
        self.files.run(|| move |files| files.restore_diagnostic_checkpoint(recovered)).await
    }
    pub async fn diagnostic_checkpoints(
        &self,
        controller_version: u32,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<super::DiagnosticCheckpointPage> {
        self.files
            .run(|| {
                let cursor = cursor.map(str::to_owned);
                move |files| files.diagnostic_checkpoints(controller_version, cursor.as_deref(), limit)
            })
            .await
    }
    pub async fn diagnostic_evidence(
        &self,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<(CollectionDescription, ExportedRecord)> {
        self.files
            .run(|| {
                let reference = reference.clone();
                move |files| files.diagnostic_reference(&reference)
            })
            .await
    }
    /// Authenticate local queue knowledge without interpreting absent metadata or cached evidence as consumption.
    pub async fn diagnostic_input_state(
        &self,
        reference: &DiagnosticCaptureRef,
        controller_version: u32,
    ) -> Result<super::DiagnosticInputState> {
        self.files
            .run(|| {
                let reference = reference.clone();
                move |files| files.diagnostic_input_state(&reference, controller_version)
            })
            .await
    }
    /// Authenticate and retain the exact original before storing a queued investigation. This grants no native rights.
    pub async fn pin_investigation_parent(&mut self, key: &str, focus: &ActivityWaitFocus) -> Result<()> {
        self.files
            .run(|| {
                let key = key.to_owned();
                let focus = focus.clone();
                move |files| files.pin_investigation_parent(&key, &focus)
            })
            .await
    }
    /// The caller reconciles this complete bounded inventory with its durable investigation journal after restart.
    pub async fn investigation_parent_keys(&self) -> Result<Vec<String>> {
        self.files.run(|| |files| files.investigation_parent_keys()).await
    }
    /// Release only the settled journal stage's pin. Native focus/result pins and source-work debt are independent.
    pub async fn release_investigation_parent(&mut self, key: &str) -> Result<()> {
        self.files
            .run(|| {
                let key = key.to_owned();
                move |files| files.release_investigation_parent(&key)
            })
            .await
    }
    /// Consume only sealed native evidence; this path never creates a native source session.
    pub async fn next_diagnostic_input(&self, controller_version: u32) -> Result<Option<super::DiagnosticInput>> {
        self.files.run(|| move |files| files.next_diagnostic_input(controller_version)).await
    }
    pub async fn diagnostic_input(
        &self,
        observation_id: &str,
        controller_version: u32,
    ) -> Result<super::DiagnosticInput> {
        self.files
            .run(|| {
                let id = observation_id.to_owned();
                move |files| files.diagnostic_input(&id, controller_version)
            })
            .await
    }
    pub async fn commit_diagnostics(
        &mut self,
        input: super::DiagnosticInput,
        update: super::DiagnosticUpdate,
    ) -> Result<CaptureIdentity> {
        self.files.run(|| move |files| files.commit_diagnostics(input, update)).await
    }
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self::from_files(Files::open(path)?))
    }
    fn from_files(files: Files) -> Self {
        let recovery_store_id = match &files.authority {
            super::RevisionAuthority::Local => None,
            super::RevisionAuthority::Recovery { store_id } => Some(store_id.clone()),
        };
        Self { files: BlockingState::new(files, || problem(OutboxProblem::Storage)), recovery_store_id }
    }
    pub fn recovery_store_id(&self) -> Option<&str> {
        self.recovery_store_id.as_deref()
    }
    /// Read the exact allocated manifest without granting native rights or acknowledging diagnostic work.
    /// A missing initial file is the only successful result without a manifest digest.
    pub async fn attempt_receipt(
        &self,
        identity: &CaptureIdentity,
        investigation: Option<&DiagnosticInvestigationLineage>,
    ) -> Result<DiagnosticAttemptReceipt> {
        self.files
            .run(|| {
                let identity = identity.clone();
                let investigation = investigation.cloned();
                move |files| files.attempt_receipt(&identity, investigation.as_ref())
            })
            .await
    }
    pub fn delivery(&self) -> OutboxDelivery {
        OutboxDelivery { files: self.files.clone(), recovery_store_id: self.recovery_store_id.clone() }
    }
    pub async fn inspect(path: &Path, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        let path = path.to_owned();
        let cursor = cursor.map(str::to_owned);
        tokio::task::spawn_blocking(move || Files::inspect(&path, limit, cursor.as_deref()))
            .await
            .map_err(|_| problem(OutboxProblem::Storage))?
    }
    pub async fn replay(path: &Path, observation_id: &str, destination: &Path) -> Result<OutboxReplay> {
        let path = path.to_owned();
        let observation_id = observation_id.to_owned();
        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || Files::replay(&path, &observation_id, &destination))
            .await
            .map_err(|_| problem(OutboxProblem::Storage))?
    }
    pub async fn status(&self, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        self.delivery().status(limit, cursor).await
    }
    pub async fn recover(&mut self) -> Result<OutboxRecovery> {
        self.files.run(|| |files| files.recover()).await
    }
    pub async fn acknowledge_source_quiescence(&mut self, observation_id: &str) -> Result<()> {
        self.files
            .run(|| {
                let id = observation_id.to_owned();
                move |files| files.acknowledge_source_quiescence(&id)
            })
            .await
    }
    pub async fn verify(&mut self, observation_id: &str) -> Result<CollectionManifest> {
        self.files
            .run(|| {
                let id = observation_id.to_owned();
                move |files| files.verify(&id)
            })
            .await
    }
    pub async fn captured_range(&self, key: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        self.files
            .run(|| {
                let key = key.clone();
                move |files| files.captured_range(&key)
            })
            .await
    }
    pub async fn reserve(&mut self, connection: &str, request: &CollectionRequest) -> Result<CaptureLease<'_>> {
        let reservation = self
            .files
            .run(|| {
                let connection = connection.to_owned();
                let request = request.clone();
                move |files| files.reserve(&connection, &request)
            })
            .await?;
        Ok(CaptureLease { outbox: self, reservation })
    }
}
impl OutboxDelivery {
    pub(crate) async fn next_archive(&self) -> Result<Option<String>> {
        self.files.run(|| |files| files.next_archive()).await
    }
    pub(crate) async fn archive_range(&self, id: &str) -> Result<Option<String>> {
        self.files
            .run(|| {
                let id = id.to_owned();
                move |files| files.archive_range(&id)
            })
            .await
    }
    pub(crate) async fn mark_archived(&self, range: &delivery::recovery::CommittedRange) -> Result<()> {
        self.files
            .run(|| {
                let range = range.clone();
                move |files| files.mark_archived(&range)
            })
            .await
    }
    pub(crate) async fn quarantine_archive(&self, id: &str) -> Result<()> {
        self.files
            .run(|| {
                let id = id.to_owned();
                move |files| files.quarantine_archive(&id)
            })
            .await
    }
    pub fn recovery_store_id(&self) -> Option<&str> {
        self.recovery_store_id.as_deref()
    }
    pub(crate) async fn eviction_candidate(&self, id: &str) -> Result<Option<bool>> {
        self.files
            .run(|| {
                let id = id.to_owned();
                move |files| files.eviction_candidate(&id)
            })
            .await
    }
    /// Returns false while the capture is the latest activity checkpoint required by the next source poll.
    pub async fn evict(&self, committed: &delivery::recovery::CommittedRange) -> Result<bool> {
        self.files
            .run(|| {
                let committed = committed.clone();
                move |files| files.evict(&committed)
            })
            .await
    }
    pub async fn status(&self, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        self.files
            .run(|| {
                let cursor = cursor.map(str::to_owned);
                move |files| files.status(limit, cursor.as_deref())
            })
            .await
    }
    pub async fn prepare(
        &self,
        observation_id: &str,
        contract: delivery::contract::Contract,
        advances: Vec<delivery::evidence::StreamAdvance>,
    ) -> Result<delivery::evidence::PreparedRange> {
        self.files
            .run(|| {
                let id = observation_id.to_owned();
                move |files| files.prepare_delivery(&id, contract, advances)
            })
            .await
    }
}
impl CaptureLease<'_> {
    pub fn identity(&self) -> &CaptureIdentity {
        &self.reservation.entry.identity
    }
    pub fn directory(&self) -> &Path {
        &self.reservation.directory
    }
    pub(crate) async fn source_started(&mut self) -> Result<()> {
        self.source_work(SourceWorkState::Active).await
    }
    pub(crate) async fn source_closed(&mut self, shutdown: SourceShutdown) -> Result<()> {
        self.source_work(match shutdown {
            SourceShutdown::Quiescent => SourceWorkState::Quiescent,
            SourceShutdown::Unconfirmed => SourceWorkState::Unconfirmed,
        })
        .await
    }
    async fn source_work(&self, work: SourceWorkState) -> Result<()> {
        self.outbox
            .files
            .run(|| {
                let entry = self.reservation.entry.clone();
                move |files| files.journal.source_work(&entry, work)
            })
            .await
    }
    pub async fn finish(self) -> Result<()> {
        self.outbox.files.run(|| move |files| files.finish(self.reservation)).await
    }
}
