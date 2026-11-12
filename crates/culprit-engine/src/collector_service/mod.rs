//! Runtime composition owns credentials and adapters; capture, recovery and delivery retain their own lifecycles.
pub mod config;
mod destination;
mod recovery;
#[cfg(all(test, feature = "store-adx"))]
mod tests;
use crate::Workspace;
use config::*;
use culprit_collector::{
    capture::CollectionCancellation,
    delivery::{
        activity_recovery::ActivityRestoreBudget,
        blob_store::BlobRecoveryStore,
        contract::Contract,
        driver::DeliveryPolicy,
        journal::{DeliveryHealthReader, DeliveryJournal},
        recovery::RecoveryStore,
        restore::restore_activity_page,
        worker::{DeliveryJournals, DeliveryWorker},
    },
    fleet::{FleetBackend, FleetJournal, FleetRunner},
    health::{CollectorHealth, HealthServer},
    lifecycle::with_background,
    outbox::Outbox,
    service::{CollectorService, ServiceReporter, run_delivery},
};
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeSet, path::Path, sync::Arc};

/// Delivery-only recovery never opens a native connection or advances its ownership epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Collect,
    DeliveryOnly,
}

struct Adapters {
    workspace: Workspace,
    recovery: Arc<BlobRecoveryStore>,
    destination: Box<dyn destination::SinkFactory>,
    contract: Contract,
}
struct ConfiguredAdapters {
    workspace: Workspace,
    recovery: Arc<dyn culprit_collector::delivery::blob_store::transport::BlobTransport>,
    destination: Box<dyn destination::SinkFactory>,
    contract: Contract,
}
impl ConfiguredAdapters {
    fn new(config: &CollectorConfig, mode: SourceMode) -> Result<Self> {
        let credentials = Arc::new(crate::credentials::ConfiguredCredentials::new(&config.credentials)?);
        Self::with_credentials(config, mode, credentials)
    }
    fn with_credentials(
        config: &CollectorConfig,
        mode: SourceMode,
        credentials: Arc<dyn culprit_auth::CredentialResolver>,
    ) -> Result<Self> {
        config.validate()?;
        let mut workspace = Workspace::open_in(&config.workspace);
        if mode == SourceMode::Collect
            && let Some(profile) = &config.source_credential
        {
            workspace = workspace.with_database_tokens(credentials.database(profile)?);
        }
        if mode == SourceMode::Collect {
            for target in config.inventory.targets.iter().filter(|target| target.shard == config.shard) {
                workspace.validate_target(&target.connection)?;
            }
        }
        let transport = config.recovery.transport(credentials.as_ref())?;
        let destination = destination::configure(&config.target, credentials, transport.clone())?;
        let contract = Contract::new(
            config.pipeline.clone(),
            config.shard.clone(),
            destination.destination().into(),
            destination.schema_digest().into(),
        )?;
        Ok(Self { workspace, recovery: transport, destination, contract })
    }
}
impl Adapters {
    async fn open(config: &CollectorConfig, initialize: bool, mode: SourceMode) -> Result<Self> {
        let ConfiguredAdapters { workspace, recovery: transport, destination, contract } =
            ConfiguredAdapters::new(config, mode)?;
        let recovery = Arc::new(if initialize {
            BlobRecoveryStore::create(transport, contract.clone()).await?
        } else {
            BlobRecoveryStore::open(transport, contract.clone()).await?
        });
        Ok(Self { workspace, recovery, destination, contract })
    }
}

/// Validate source metadata, identity configuration, destinations and schema without acquiring tokens or
/// contacting source databases, warehouses, Blob or Azure management. No local runtime state is created.
pub fn validate(config: &CollectorConfig) -> Result<()> {
    ConfiguredAdapters::new(config, SourceMode::Collect).map(|_| ())
}

/// Explicit namespace provisioning. Normal startup refuses a missing remote history.
/// This does not provision warehouse tables, Event Grid, containers, identities, permissions or networks.
pub async fn initialize(config: &CollectorConfig) -> Result<()> {
    Adapters::open(config, true, SourceMode::Collect).await.map(|_| ())
}

/// The host must independently establish that a previous source owner is quiescent before this call.
/// A storage epoch fences writes, never an orphan SQL request. Native debts pause collection without
/// stopping archive recovery, analytical delivery or probes. Delivery-only mode needs no source assertion.
pub async fn run(
    config: &CollectorConfig,
    mode: SourceMode,
    cancellation: &CollectionCancellation,
    max_work: Option<u32>,
    reporter: &dyn ServiceReporter,
) -> Result<FleetSummary> {
    ConfiguredAdapters::new(config, mode)?;
    if mode == SourceMode::DeliveryOnly && max_work.is_some() {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    let health = CollectorHealth::new(&config.inventory, &config.shard)?;
    if mode == SourceMode::DeliveryOnly {
        health.source_disabled()?;
    }
    let probes = HealthServer::bind(config.health_listen, health.clone()).await?;
    with_background(
        cancellation,
        run_configured(config, mode, cancellation, max_work, reporter, &health),
        probes.run(cancellation),
    )
    .await
}
async fn run_configured(
    config: &CollectorConfig,
    mode: SourceMode,
    cancellation: &CollectionCancellation,
    max_work: Option<u32>,
    reporter: &dyn ServiceReporter,
    health: &CollectorHealth,
) -> Result<FleetSummary> {
    let adapters = Adapters::open(config, false, mode).await?;
    if cancellation.is_cancelled() {
        return Ok(FleetSummary::default());
    }
    private_state(&config.state)?;
    let outbox_path = config.state.join("outbox");
    let mut outbox = if outbox_path.try_exists().map_err(|_| storage())? {
        Outbox::open(&outbox_path)?
    } else {
        Outbox::create_remote(&outbox_path, config.limits.clone(), adapters.recovery.id())?
    };
    let recovered = outbox.recover().await?;
    if outbox.status(1, None).await?.limits != config.limits {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    // Inspect the independent native admission journal before acquiring a new remote write epoch.
    let source_paused = mode == SourceMode::Collect
        && (recovered.source_debts > 0
            || adapters.workspace.collection_admission_status().await?.source_quiescence_required);
    if source_paused && max_work.is_some() {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let mut journal = open_journal(&config.state.join("delivery"), &adapters.contract)?;
    let mut archive = open_journal(&config.state.join("archive"), &adapters.contract)?;
    archive.bind_store(adapters.recovery.id()).await?;
    journal.bind_store(adapters.recovery.id()).await?;
    let fence = adapters.recovery.acquire().await?;
    if mode == SourceMode::Collect && !source_paused {
        let high = restore(&mut outbox, &*adapters.recovery, cancellation).await?;
        if !cancellation.is_cancelled() {
            outbox.begin_remote_epoch(&fence, high).await?;
        }
    }
    if cancellation.is_cancelled() {
        return Ok(FleetSummary::default());
    }
    let health_sources = HealthSources {
        outbox: outbox.delivery(),
        archive: archive.health_reader(),
        analytical: journal.health_reader(),
    };
    let sink = adapters.destination.open(
        adapters.contract.clone(),
        adapters.recovery.clone(),
        &config.state.join("scratch"),
    )?;
    let mut delivery = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut journal },
        &*adapters.recovery,
        &sink,
        fence,
        adapters.contract,
        DeliveryPolicy::default(),
    )?;
    let reporter = ObservedReporter { health, delegate: reporter };
    if source_paused {
        reporter.source_paused(&CollectionStop::CancellationUnconfirmed.error())?;
    }
    health.initialized()?;
    let work = async {
        if mode == SourceMode::DeliveryOnly || source_paused {
            run_delivery(&mut delivery, cancellation, &reporter).await?;
            if source_paused {
                return Err(CollectionStop::CancellationUnconfirmed.error());
            }
            return Ok(FleetSummary::default());
        }
        let fleet = FleetJournal::open(&config.state.join("fleet"), &config.inventory, &config.shard).await?;
        let source = FleetRunner::new(&adapters.workspace, &config.inventory, &config.shard, &fleet, &mut outbox)?
            .with_recovery(&*adapters.recovery)?;
        CollectorService::new(source, delivery).run(cancellation, max_work, &reporter).await
    };
    with_background(cancellation, work, refresh_health(health_sources, health, cancellation)).await
}

fn open_journal(path: &Path, contract: &Contract) -> Result<DeliveryJournal> {
    if path.try_exists().map_err(|_| storage())? {
        DeliveryJournal::open(path, contract.clone())
    } else {
        DeliveryJournal::create(path, contract.clone())
    }
}

struct ObservedReporter<'a> {
    health: &'a CollectorHealth,
    delegate: &'a dyn ServiceReporter,
}
impl ServiceReporter for ObservedReporter<'_> {
    fn source_paused(&self, error: &culprit_core::Error) -> Result<()> {
        self.health.source_paused()?;
        self.delegate.source_paused(error)
    }
    fn collection(&self, event: &FleetCollectionEvent) -> Result<()> {
        self.health.collection(event)?;
        self.delegate.collection(event)
    }
    fn delivery(&self, previous: Option<&DeliveryEntry>, result: Result<DeliveryEntry>) -> Result<()> {
        self.health.delivery(&result)?;
        self.delegate.delivery(previous, result)
    }
}
struct HealthSources {
    outbox: culprit_collector::outbox::OutboxDelivery,
    archive: DeliveryHealthReader,
    analytical: DeliveryHealthReader,
}
async fn refresh_health(
    sources: HealthSources,
    health: &CollectorHealth,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    while !cancellation.is_cancelled() {
        health.outbox(&sources.outbox.status(1, None).await?)?;
        health.archive_backlog(sources.archive.backlog().await?)?;
        health.backlog(sources.analytical.backlog().await?)?;
        tokio::select! {biased;_=cancellation.cancelled()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(5))=>{}}
    }
    Ok(())
}

async fn restore(
    outbox: &mut Outbox,
    recovery: &dyn RecoveryStore,
    cancellation: &CollectionCancellation,
) -> Result<u64> {
    let mut high = 0;
    let mut cursor = None;
    let mut visited = BTreeSet::new();
    loop {
        if cancellation.is_cancelled() {
            return Ok(high);
        }
        let page =
            restore_activity_page(outbox, recovery, cursor.as_deref(), 128, ActivityRestoreBudget::default()).await?;
        high = high.max(page.high_sequence);
        let Some(next) = page.next_cursor else {
            break;
        };
        if visited.len() >= MAX_FLEET_TARGETS || !visited.insert(next.clone()) {
            return Err(DeliveryProblem::Integrity.error());
        }
        cursor = Some(next);
    }
    Ok(high)
}
fn private_state(path: &Path) -> Result<()> {
    if path.try_exists().map_err(|_| storage())?
        && !std::fs::symlink_metadata(path).map_err(|_| storage())?.file_type().is_dir()
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    crate::private_fs::create_dir(path).map_err(|_| storage())
}
fn storage() -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}
