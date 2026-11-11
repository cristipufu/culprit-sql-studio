//! Outbox transactions retain file ownership on a blocking worker while async source cancellation remains runnable.
use super::{Files, Reservation, problem};
use crate::{blocking_state::BlockingState, collection_activity::ActivityBaseline, delivery};
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
    /// Install verified native checkpoints before source activation. This operation never opens a database.
    pub async fn restore_activity(
        &mut self,
        checkpoint: delivery::activity_recovery::RecoveredActivityCheckpoint,
    ) -> Result<()> {
        self.files.run(|| move |files| files.restore_activity(checkpoint)).await
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
    pub(crate) async fn baseline(&self, source: &CollectionDescription) -> Result<Option<ActivityBaseline>> {
        let restored = self
            .outbox
            .files
            .run(|| {
                let source = source.clone();
                move |files| files.restored_baseline(&source)
            })
            .await?;
        let source_id = source.logical_source_id()?;
        let local = match self.reservation.baseline.as_ref() {
            Some(baseline) if baseline.source.logical_source_id()? == source_id => Some(baseline),
            _ => None,
        };
        match (local, restored) {
            (Some(local), Some(remote)) => {
                if super::baselines::compare(&local.capture, &remote.capture)?.is_lt() {
                    Ok(Some(remote))
                } else {
                    Ok(Some(local.clone()))
                }
            }
            (Some(local), None) => Ok(Some(local.clone())),
            (None, remote) => Ok(remote),
        }
    }
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
