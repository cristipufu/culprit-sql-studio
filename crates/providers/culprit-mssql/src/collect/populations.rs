//! Shared native acquisition precedes independent, exact-population snapshot projection.
use super::*;
use culprit_core::telemetry::{CollectionStop, NativeIdentity, SourceShutdown};
use culprit_core::{PopulationSnapshots, QueryStoreScope, RuntimePopulationCatalog, WorkloadKind};

mod retention;

pub(super) struct Workloads {
    requested: WorkloadPart,
    populations: Vec<(NativeIdentity, WorkloadPart)>,
    catalog: Option<RuntimePopulationCatalog>,
    stop_followup: bool,
    pending_compilation: Option<signals::PendingCompilationRead>,
    compilation: signals::CompilationRead,
    configured_log_rates: Option<Result<Box<culprit_core::telemetry::ConfiguredLogRateRead>>>,
}

impl Workloads {
    fn single(mut requested: WorkloadPart) -> Self {
        let configured_log_rates = requested.configured_log_rates.take();
        Self {
            catalog: requested.catalog.clone(),
            requested,
            populations: Vec::new(),
            stop_followup: false,
            pending_compilation: None,
            compilation: Default::default(),
            configured_log_rates,
        }
    }

    pub(super) fn take_configured_log_rates(
        &mut self,
    ) -> Option<Result<Box<culprit_core::telemetry::ConfiguredLogRateRead>>> {
        self.configured_log_rates.take()
    }

    pub(super) fn take_compilation(&mut self) -> signals::CompilationRead {
        std::mem::take(&mut self.compilation)
    }

    fn finish_compilation(
        &mut self,
        minutes: u32,
        shutdown: SourceShutdown,
        context: Option<culprit_core::telemetry::NativeReadContext>,
    ) {
        if let Some(pending) = self.pending_compilation.take() {
            let compilation = pending.finish(minutes, &mut self.participants(), shutdown, context);
            self.compilation = compilation;
        }
    }

    pub(super) fn participants(&mut self) -> Vec<signals::QueryParticipant<'_>> {
        std::iter::once(&mut self.requested)
            .chain(self.populations.iter_mut().map(|(_, part)| part))
            .flat_map(WorkloadPart::participants)
            .collect()
    }

    pub(super) fn database_identity(
        &self,
        database: &str,
        host: &str,
        port: u16,
    ) -> Result<culprit_core::DatabaseIdentity> {
        let mut contexts = std::iter::once(&self.requested)
            .chain(self.populations.iter().map(|(_, part)| part))
            .filter_map(|part| part.database.as_ref());
        let Some(context) = contexts.next() else {
            return Ok(culprit_core::DatabaseIdentity::server(Engine::Mssql, database, host, port, None));
        };
        if context.database != database || contexts.any(|other| other != context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(context.identity(host, port))
    }

    pub(super) fn project(self, shared: Snapshot, options: &SnapshotOptions) -> Result<PopulationSnapshots> {
        retention::project(shared, self, options)
    }

    pub(super) fn project_requested(self, mut shared: Snapshot) -> Snapshot {
        shared.runtime_populations = self.catalog;
        self.requested.apply(&mut shared);
        shared
    }

    fn allows_cache_fallback(&self) -> bool {
        // An unselected overview is not a source failure when exact native populations were retained.
        !self.stop_followup
            && self.requested.workload.is_none()
            && self.populations.iter().all(|(_, part)| part.workload.is_none())
    }

    fn fail_provisional(&mut self, error: &culprit_core::Error) {
        self.pending_compilation = None;
        self.compilation = signals::CompilationRead::failed(error);
        self.stop_followup = true;
        self.requested = failed_part(error, self.catalog.clone());
        for (_, part) in &mut self.populations {
            *part = failed_part(error, self.catalog.clone());
        }
    }
}

impl WorkloadPart {
    pub(super) fn participants(&mut self) -> Vec<signals::QueryParticipant<'_>> {
        let Some(workload) = &mut self.workload else { return Vec::new() };
        let runtime_scope = match (&workload.population_catalog, &workload.population) {
            (Some(catalog), None) if catalog.is_valid() && !catalog.partitioned => QueryStoreScope::Local,
            (Some(catalog), Some(population)) if catalog.is_valid() && catalog.contains(population) => {
                QueryStoreScope::ReplicaGroup { id: population.id.clone() }
            }
            _ => QueryStoreScope::Unknown,
        };
        let identities = &self.query_identities;
        let statements = &self.compilation_statements;
        let kind = workload.kind;
        workload
            .queries
            .iter_mut()
            .map(|query| {
                let identity = identities.get(&query.id);
                let compilation_statement = statements.get(&query.id);
                signals::QueryParticipant {
                    query,
                    identity,
                    compilation_statement,
                    kind,
                    runtime_scope: runtime_scope.clone(),
                }
            })
            .collect()
    }

    fn apply(self, snapshot: &mut Snapshot) {
        // Connected-member wait totals cannot stand in for a selected Query Store group's missing/empty waits.
        if self.workload.as_ref().is_some_and(|workload| workload.kind == Some(WorkloadKind::QueryStore))
            || snapshot.options.as_ref().is_some_and(|options| options.workload_population.is_some())
            || !self.waits.is_empty()
        {
            snapshot.waits = self.waits;
        }
        snapshot.workload = self.workload;
        self.outcome.append_to(snapshot);
        let identity = snapshot.database_identity();
        snapshot.bind_identity(identity);
    }

    pub(super) fn finish_window(
        &mut self,
        window: &crate::query_store::window_sql::FrozenWindow,
        verification: crate::query_store::window_sql::WindowVerification,
        catalog: Option<&RuntimePopulationCatalog>,
    ) {
        self.catalog = catalog.cloned();
        let Some(workload) = &mut self.workload else { return };
        workload.population_catalog = catalog.cloned();
        self.database = Some(window.database.clone());
        workload.window_start = window.start.clone();
        workload.window_end = window.end.clone();
        workload.window_hours = window.hours;
        if let Some(timeline) = &mut workload.timeline
            && let Some(end) = &window.end
        {
            timeline.end = end.clone();
        }
        if verification == crate::query_store::window_sql::WindowVerification::WaitCaptureChanged {
            self.outcome.partial(&sources::QUERY_WAITS, CollectionStop::SourceChanged.error().message_source());
        }
    }
}

pub(super) async fn collect(
    provider: &MssqlProvider,
    probe: &Probe,
    options: &SnapshotOptions,
    automatic: bool,
) -> Result<Workloads> {
    if !automatic || options.workload_population.is_some() {
        let (part, compilation) = collect_workload(provider, probe, options).await?;
        let mut workloads = Workloads::single(part);
        workloads.compilation = compilation;
        return Ok(workloads);
    }
    if !probe.query_store_readable() {
        let (part, compilation) = collect_workload(provider, probe, options).await?;
        let mut workloads = Workloads::single(part);
        workloads.compilation = compilation;
        return Ok(workloads);
    }
    let mut reader = crate::query_store::Reader::open(provider.pool.clone());
    if probe.is_azure_sql_database() {
        reader.request_configured_log_rates()?;
    }
    let mut workloads = acquire(&mut reader, options).await;
    let shutdown = reader.close().await;
    if shutdown == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    workloads.configured_log_rates = reader
        .configured_log_rates(shutdown, &provider.endpoint_host, provider.endpoint_port)
        .map(|read| read.map(Box::new))
        .transpose();
    workloads.finish_compilation(
        options.window_in_minutes(),
        shutdown,
        reader.compilation_context(&provider.endpoint_host, provider.endpoint_port),
    );
    if workloads.allows_cache_fallback() {
        workloads.requested = collect_plan_cache(provider, probe, options, workloads.requested).await?;
    }
    Ok(workloads)
}

async fn acquire<R: NativeRead>(reader: &mut crate::query_store::Reader<R>, options: &SnapshotOptions) -> Workloads {
    let catalog = match reader.prepare_population_batch(options.top_queries as usize, options.window_in_minutes()).await
    {
        Ok(catalog) => catalog,
        Err(error) => {
            let mut workloads = Workloads::single(failed_part(&error, reader.population_catalog().cloned()));
            workloads.stop_followup =
                reader.retirement_reason().is_some() || crate::read::stops_remaining_reads(&error);
            return workloads;
        }
    };
    let mut workloads = Workloads {
        requested: WorkloadPart { catalog: Some(catalog.clone()), ..Default::default() },
        populations: Vec::new(),
        catalog: Some(catalog.clone()),
        stop_followup: false,
        pending_compilation: None,
        compilation: Default::default(),
        configured_log_rates: None,
    };
    let selectors = if catalog.partitioned {
        catalog.populations.iter().map(|population| Some(population.identity.clone())).collect()
    } else {
        vec![None]
    };
    let mut retained = retention::WorkloadBudget::default();
    let mut terminal_error: Option<culprit_core::Error> = None;
    for selector in selectors {
        let mut selected = options.clone();
        selected.workload_population = selector.clone();
        let result = if let Some(error) = &terminal_error {
            Err(error.clone())
        } else if retained.exhausted() {
            Err(CollectionStop::ByteLimit.error())
        } else {
            match reader.select_batch_population(selector.as_ref()).await {
                Ok(()) => workload::read_query_store(reader, &selected).await,
                Err(error) => Err(error),
            }
        };
        let mut part = WorkloadPart { catalog: Some(catalog.clone()), ..Default::default() };
        let result = result.and_then(|result| {
            retained.retain(&result.workload, &result.compilation_statements)?;
            Ok(result)
        });
        if let Err(error) = &result
            && crate::read::stops_remaining_reads(error)
            && !retained.exhausted()
        {
            terminal_error = Some(error.clone());
        }
        if let Some(error) = reader.retirement_reason() {
            terminal_error = Some(error.clone());
        }
        if let Some(result) = part.outcome.read(&sources::QUERY_STORE, result) {
            part.accept(&sources::QUERY_STORE, Capability::QueryStore, result);
        }
        match selector {
            Some(selector) => workloads.populations.push((selector, part)),
            None => workloads.requested = part,
        }
    }
    if terminal_error.is_none()
        && workloads.participants().iter().any(|participant| participant.kind == Some(WorkloadKind::QueryStore))
    {
        let pending = signals::read_compilations(reader, &workloads.participants()).await;
        workloads.pending_compilation = Some(pending);
    }
    let verification = match terminal_error {
        Some(error) => Err(error),
        None => reader.finish_population_batch().await,
    };
    match verification {
        Ok((window, verification)) => {
            workloads.catalog = reader.population_catalog().cloned();
            workloads.requested.finish_window(&window, verification, workloads.catalog.as_ref());
            for (_, part) in &mut workloads.populations {
                part.finish_window(&window, verification, workloads.catalog.as_ref());
            }
        }
        Err(error) => workloads.fail_provisional(&error),
    }
    workloads.stop_followup |= reader.retirement_reason().is_some();
    let catalog = workloads.catalog.clone().unwrap_or(catalog);
    if catalog.partitioned && !workloads.stop_followup {
        if catalog.requires_selection {
            workloads.requested = failed_part(
                &culprit_core::Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("provider.mssql.query_store.population_required"),
                ),
                Some(catalog),
            );
        } else if let [(_, part)] = workloads.populations.as_slice() {
            workloads.requested = part.clone();
        }
    }
    workloads
}

pub(super) fn failed_part(error: &culprit_core::Error, catalog: Option<RuntimePopulationCatalog>) -> WorkloadPart {
    let mut part = WorkloadPart { catalog, ..Default::default() };
    part.outcome.source_failed(&sources::QUERY_STORE, error);
    part
}

#[cfg(test)]
mod acquisition_tests;
#[cfg(test)]
mod tests;
