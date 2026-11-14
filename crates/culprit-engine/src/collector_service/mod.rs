//! Runtime composition owns credentials and adapters; capture, recovery and delivery retain their own lifecycles.
pub mod config;
mod destination;
pub mod evidence;
mod recovery;
mod runtime;
pub use runtime::CollectorRuntime;
#[cfg(test)]
mod startup_tests;
#[cfg(all(test, feature = "store-adx", feature = "recovery-azure"))]
mod tests;
use crate::Workspace;
use config::*;
use culprit_collector::{
    capture::CollectionCancellation,
    delivery::{
        contract::Contract,
        journal::{DeliveryHealthReader, DeliveryJournal},
        object_store::ObjectRecoveryStore,
        recovery::RecoveryStore,
        restore::{restore_diagnostic_inputs_page, restore_diagnostics_page},
        worker::{DeliveryJournals, DeliveryWorker},
    },
    diagnostics::{DiagnosticPolicies, drain_pending as drain_diagnostics},
    fleet::{FleetBackend, FleetJournal, FleetRunner},
    health::{CollectorHealth, HealthServer},
    lifecycle::with_background,
    outbox::Outbox,
    service::{CollectorService, ServiceReporter, run_delivery},
};
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeSet, path::Path, sync::Arc};

/// Delivery-only recovery never opens a native connection or enables native capture allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Collect,
    DeliveryOnly,
}

struct Adapters {
    workspace: Workspace,
    credentials: Arc<dyn culprit_auth::CredentialResolver>,
    recovery: Arc<ObjectRecoveryStore>,
    destination: Box<dyn destination::SinkFactory>,
    contract: Contract,
}
struct ConfiguredAdapters {
    workspace: Workspace,
    credentials: Arc<dyn culprit_auth::CredentialResolver>,
    recovery: Arc<dyn culprit_collector::delivery::object_store::transport::ObjectTransport>,
    destination: Box<dyn destination::SinkFactory>,
    contract: Contract,
}
impl ConfiguredAdapters {
    fn new(config: &CollectorConfig, mode: SourceMode, runtime: &CollectorRuntime) -> Result<Self> {
        let credentials: Arc<dyn culprit_auth::CredentialResolver> = match &runtime.credentials {
            Some(credentials) => credentials.clone(),
            None => Arc::new(crate::credentials::ConfiguredCredentials::new(&config.credentials)?),
        };
        Self::configured(config, mode, credentials, runtime)
    }
    fn configured(
        config: &CollectorConfig,
        mode: SourceMode,
        credentials: Arc<dyn culprit_auth::CredentialResolver>,
        runtime: &CollectorRuntime,
    ) -> Result<Self> {
        config.validate()?;
        let mut workspace = Workspace::open_in(&config.workspace);
        if mode == SourceMode::Collect {
            workspace = bind_sources(workspace, config, credentials.as_ref())?;
        }
        let recovery = runtime.recovery.configure(&config.recovery, credentials.as_ref())?;
        let transport = recovery.transport;
        let destination = destination::configure(&config.target, credentials.clone(), transport.clone())?;
        if destination.staging_scope().is_some_and(|scope| scope == recovery.access_scope) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let contract = Contract::new(
            config.pipeline.clone(),
            config.shard.clone(),
            destination.destination().into(),
            destination.schema_digest().into(),
        )?;
        Ok(Self { workspace, credentials, recovery: transport, destination, contract })
    }
}
impl Adapters {
    async fn open(
        config: &CollectorConfig,
        initialize: bool,
        mode: SourceMode,
        runtime: &CollectorRuntime,
    ) -> Result<Self> {
        let ConfiguredAdapters { workspace, credentials, recovery: transport, destination, contract } =
            ConfiguredAdapters::new(config, mode, runtime)?;
        let recovery = Arc::new(if initialize {
            ObjectRecoveryStore::create(transport, contract.clone()).await?
        } else {
            ObjectRecoveryStore::open(transport, contract.clone()).await?
        });
        Ok(Self { workspace, credentials, recovery, destination, contract })
    }
    fn activate_sources(mut self, config: &CollectorConfig) -> Result<Self> {
        self.workspace = bind_sources(self.workspace, config, self.credentials.as_ref())?;
        Ok(self)
    }
}

fn bind_sources(
    mut workspace: Workspace,
    config: &CollectorConfig,
    credentials: &dyn culprit_auth::CredentialResolver,
) -> Result<Workspace> {
    let routes: BTreeSet<_> = config
        .inventory
        .targets
        .iter()
        .filter(|target| target.shard == config.shard)
        .map(|target| target.connection.as_str())
        .collect();
    for connection in routes {
        if let Some(profile) = config.source_credentials.get(connection) {
            workspace = workspace.with_connection_tokens(connection, credentials.database(profile)?);
        }
        workspace.validate_target(connection)?;
    }
    Ok(workspace)
}

/// Validate source metadata, identity configuration, destinations and schema without acquiring tokens or
/// contacting source databases, warehouses, recovery storage or cloud management. No local runtime state is created.
pub fn validate(config: &CollectorConfig) -> Result<()> {
    CollectorRuntime::default().validate(config)
}

/// Explicit namespace provisioning. Normal startup refuses a missing remote history.
/// This does not provision warehouse tables, Event Grid, containers, identities, permissions or networks.
pub async fn initialize(config: &CollectorConfig) -> Result<()> {
    CollectorRuntime::default().initialize(config).await
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
    CollectorRuntime::default().run(config, mode, cancellation, max_work, reporter).await
}
async fn run_with_runtime(
    runtime: &CollectorRuntime,
    config: &CollectorConfig,
    mode: SourceMode,
    cancellation: &CollectionCancellation,
    max_work: Option<u32>,
    reporter: &dyn ServiceReporter,
) -> Result<FleetSummary> {
    // Startup must inspect durable source debt and restore native evidence before resolving source profiles.
    // Explicit validate() still checks every configured source offline.
    ConfiguredAdapters::new(config, SourceMode::DeliveryOnly, runtime)?;
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
        run_configured(runtime, config, mode, cancellation, max_work, reporter, &health),
        probes.run(cancellation),
    )
    .await
}
async fn run_configured(
    runtime: &CollectorRuntime,
    config: &CollectorConfig,
    mode: SourceMode,
    cancellation: &CollectionCancellation,
    max_work: Option<u32>,
    reporter: &dyn ServiceReporter,
    health: &CollectorHealth,
) -> Result<FleetSummary> {
    let mut adapters = Adapters::open(config, false, SourceMode::DeliveryOnly, runtime).await?;
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
    let mut journal = open_journal(&config.state.join("delivery"), &adapters.contract)?;
    let mut archive = open_journal(&config.state.join("archive"), &adapters.contract)?;
    archive.bind_store(adapters.recovery.id()).await?;
    journal.bind_store(adapters.recovery.id()).await?;
    let fence = adapters.recovery.acquire().await?;
    let high = restore_diagnostic_state(&mut outbox, &*adapters.recovery, cancellation).await?;
    if cancellation.is_cancelled() {
        return Ok(FleetSummary::default());
    }
    outbox.begin_diagnostic_epoch(&fence, high).await?;
    let policies = DiagnosticPolicies::from_inventory(&config.inventory, &config.shard)?;
    drain_diagnostics(&mut outbox, &policies, cancellation, Some(&*adapters.recovery)).await?;
    if cancellation.is_cancelled() {
        return Ok(FleetSummary::default());
    }
    if source_paused && max_work.is_some() {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    if mode == SourceMode::Collect && !source_paused {
        adapters = adapters.activate_sources(config)?;
        outbox.begin_remote_epoch(&fence, high).await?;
    }
    if cancellation.is_cancelled() {
        return Ok(FleetSummary::default());
    }
    let health_sources = HealthSources {
        outbox: outbox.delivery(),
        archive: archive.health_reader(),
        analytical: journal.health_reader(),
    };
    let delivery_policy = adapters.destination.delivery_policy();
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
        delivery_policy,
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

const RESTORE_PAGE_ROWS: u32 = 128;
const MAX_RESTORE_PAGES: usize = MAX_FLEET_TARGETS;

#[derive(Clone, Copy)]
enum RestorePhase {
    WorkingSet,
    PendingNativeInputs,
}

async fn restore_diagnostic_state(
    outbox: &mut Outbox,
    recovery: &dyn RecoveryStore,
    cancellation: &CollectionCancellation,
) -> Result<u64> {
    let mut high = 0;
    for phase in [RestorePhase::WorkingSet, RestorePhase::PendingNativeInputs] {
        high = high.max(restore_phase(outbox, recovery, cancellation, phase).await?);
        if cancellation.is_cancelled() {
            break;
        }
    }
    Ok(high)
}

async fn restore_phase(
    outbox: &mut Outbox,
    recovery: &dyn RecoveryStore,
    cancellation: &CollectionCancellation,
    phase: RestorePhase,
) -> Result<u64> {
    let mut high = 0;
    let mut cursor = None;
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_RESTORE_PAGES {
        if cancellation.is_cancelled() {
            return Ok(high);
        }
        let (next, sequence) = match phase {
            RestorePhase::WorkingSet => {
                let page = restore_diagnostics_page(outbox, recovery, cursor.as_deref(), RESTORE_PAGE_ROWS).await?;
                (page.next_cursor, page.high_sequence)
            }
            RestorePhase::PendingNativeInputs => {
                let page = restore_diagnostic_inputs_page(
                    outbox,
                    recovery,
                    cursor.as_deref(),
                    RESTORE_PAGE_ROWS,
                    CollectionDescription::logical_source_id,
                )
                .await?;
                (page.next_cursor, page.high_sequence)
            }
        };
        high = high.max(sequence);
        let Some(next) = next else {
            return Ok(high);
        };
        if next.len() > MAX_CURSOR_BYTES || !visited.insert(next.clone()) {
            return Err(DeliveryProblem::Integrity.error());
        }
        cursor = Some(next);
    }
    Err(DeliveryProblem::Integrity.error())
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
