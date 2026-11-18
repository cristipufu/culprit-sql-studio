//! Bounded native operations. The caller owns this reader through completion and shutdown.
use super::runtime::{self, PlanSummary};
use crate::{
    conn::Pool,
    read::{NativeRead, Parameter, ReadError, ReadSession, integer, invalid_data},
    value::Rec,
};
use culprit_core::{
    Result, RuntimePopulationCatalog,
    telemetry::{CollectionBudget, CollectionStop, NativeIdentity, SourceShutdown},
};
use std::sync::Arc;
mod batch;
mod summaries;
const MAX_RESULT_ROWS: usize = 10_000;
const SNAPSHOT_BYTES: u32 = 8 * 1024 * 1024;
const SNAPSHOT_TIMEOUT_MS: u32 = 120_000;
// Synapse exposes placeholder count/context/memory values. Only these native SQL Server database profiles
// own the query compilation measurements consumed here; an unknown edition never inherits that meaning.
const COMPILATION_NATIVE_EDITIONS: &[i64] = &[2, 3, 4, 5, 8];

pub(crate) struct Reader<R = ReadSession> {
    io: R,
    layout: Option<RuntimeLayout>,
    window: Option<super::window_sql::FrozenWindow>,
    governance_detail: crate::database_identity::log_rates::Detail,
    window_verified: bool,
    governance_available: bool,
    governance_error: Option<culprit_core::Error>,
    stopped: Option<culprit_core::Error>,
    requested_population: Option<NativeIdentity>,
    population_catalog: Option<RuntimePopulationCatalog>,
    native_layout: Option<NativeLayout>,
    replica_catalog: Option<super::replicas::Catalog>,
    empty_replica_catalog: bool,
    summaries: Option<Result<Vec<PlanSummary>>>,
    connection_generation: Option<u64>,
    batch: Option<batch::Batch>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct NativeLayout {
    edition: i64,
    tempdb: bool,
    log_bytes: bool,
    page_server_reads: super::page_server::Availability,
    waits: bool,
    variants: super::variants_sql::Availability,
    mode: super::mode::ReadMode,
    partitioned: bool,
}
#[derive(Clone, Copy)]
struct RuntimeLayout {
    variants: super::variants_sql::Availability,
    population: super::replicas::Population,
    mode: super::mode::ReadMode,
}
impl NativeLayout {
    fn supports_compilation(&self) -> bool {
        COMPILATION_NATIVE_EDITIONS.contains(&self.edition)
    }
    fn decode(row: &Rec) -> Result<Self> {
        let flag = |name| match integer(row, name)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_data()),
        };
        let mode =
            super::mode::ReadMode::from_native(&crate::read::text(row, "actual_state_desc")?).ok_or_else(|| {
                culprit_core::Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("collection.query_store_unavailable"),
                )
            })?;
        Ok(Self {
            edition: integer(row, "engine_edition")?,
            tempdb: flag("tempdb")?,
            log_bytes: flag("log_bytes")?,
            page_server_reads: super::page_server::decode_availability(row)?,
            waits: super::window_sql::waits_supported(row)?,
            variants: super::variants_sql::Availability::decode(row, "variants")?,
            mode,
            partitioned: flag("replicas")?,
        })
    }
}
impl Reader {
    pub(crate) fn open(pool: Arc<Pool>) -> Self {
        let budget = CollectionBudget {
            page_bytes: SNAPSHOT_BYTES,
            total_timeout_ms: SNAPSHOT_TIMEOUT_MS,
            ..CollectionBudget::default()
        };
        Self::new(ReadSession::new(pool, budget))
    }
}
impl<R: NativeRead> Reader<R> {
    pub(crate) fn new(io: R) -> Self {
        Self {
            io,
            layout: None,
            window: None,
            governance_detail: Default::default(),
            window_verified: false,
            governance_available: false,
            governance_error: None,
            stopped: None,
            requested_population: None,
            population_catalog: None,
            native_layout: None,
            replica_catalog: None,
            empty_replica_catalog: false,
            summaries: None,
            connection_generation: None,
            batch: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn with_test_layout(io: R, row: &Rec) -> Result<Self> {
        let mut reader = Self::new(io);
        reader.native_layout = Some(NativeLayout::decode(row)?);
        Ok(reader)
    }
    /// The scan may retain current configuration from its existing governance metadata read. Ordinary history,
    /// compiler admission and resource-pressure templates keep their identity-only projection.
    pub(crate) fn request_configured_log_rates(&mut self) -> Result<()> {
        self.admit()?;
        if self.window.is_some() || self.batch.is_some() || self.native_layout.is_some() {
            return Err(invalid_data());
        }
        self.governance_detail = crate::database_identity::log_rates::Detail::ConfiguredLogRates;
        Ok(())
    }

    pub(crate) fn configured_log_rates(
        &self,
        shutdown: SourceShutdown,
        host: &str,
        port: u16,
    ) -> Result<Option<culprit_core::telemetry::ConfiguredLogRateRead>> {
        if shutdown != SourceShutdown::Quiescent {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if self.governance_detail != crate::database_identity::log_rates::Detail::ConfiguredLogRates
            || !self.window_verified
            || self.stopped.is_some()
        {
            return Ok(None);
        }
        if let Some(error) = &self.governance_error {
            return Err(error.clone());
        }
        let Some(window) = &self.window else { return Ok(None) };
        let Some(context) = window.compilation_context(host, port) else { return Ok(None) };
        let Some(point) = window.configured_log_rates.clone()? else { return Ok(None) };
        let read = culprit_core::telemetry::ConfiguredLogRateRead {
            context,
            source: culprit_core::telemetry::StateSourceCoverage::observed(
                culprit_core::telemetry::CONFIGURED_LOG_RATES_SOURCE.into(),
                1,
                false,
            ),
            observation: Some(Box::new(point)),
        };
        read.is_valid_read(true).then_some(Some(read)).ok_or_else(invalid_data)
    }

    /// Inspect only this reader's already acquired source profile; never perform replacement discovery.
    pub(crate) fn require_compilation_profile(&self) -> Result<()> {
        self.admit()?;
        if self.native_layout.is_some_and(|layout| layout.supports_compilation()) {
            return Ok(());
        }
        Err(culprit_core::Error::new(culprit_core::ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))
    }
    pub(crate) fn select_population(&mut self, requested: Option<&NativeIdentity>) -> Result<()> {
        self.admit()?;
        if (self.layout.is_some() || self.population_catalog.is_some())
            && self.requested_population.as_ref() != requested
        {
            return Err(invalid_data());
        }
        if let Some(requested) = requested {
            super::replicas::requested_group(requested)?;
        }
        self.requested_population = requested.cloned();
        Ok(())
    }
    pub(crate) fn population_catalog(&self) -> Option<&RuntimePopulationCatalog> {
        self.population_catalog.as_ref()
    }
    pub(crate) fn selected_population(&self) -> Result<Option<NativeIdentity>> {
        self.layout.map(|layout| layout.population.identity()).transpose().map(Option::flatten)
    }
    pub(crate) fn accepts_local_capture(&self) -> Option<bool> {
        self.layout.map(|layout| layout.mode.accepts_local_capture())
    }
    pub(crate) fn is_population_batch(&self) -> bool {
        self.batch.is_some()
    }
    /// Original native window metadata remains query-global; a selected runtime group supplies no compiler owner.
    pub(crate) fn compilation_context(
        &self,
        host: &str,
        port: u16,
    ) -> Option<culprit_core::telemetry::NativeReadContext> {
        self.window.as_ref()?.compilation_context(host, port)
    }
    /// An owned whole-read stop is distinct from a source-local result cap such as one oversized dataset.
    pub(crate) fn retirement_reason(&self) -> Option<&culprit_core::Error> {
        self.stopped.as_ref()
    }
    pub(crate) async fn population(&mut self) -> Result<super::replicas::Population> {
        Ok(self.runtime_layout().await?.population)
    }
    fn admit(&self) -> Result<()> {
        if let Some(error) = &self.stopped {
            return Err(error.clone());
        }
        if self.window.as_ref().is_some_and(|window| !window.retained) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(())
    }
    fn observe_failure(&mut self, error: &culprit_core::Error) {
        if crate::read::stops_remaining_reads(error) {
            self.stopped = Some(error.clone());
            // Cached rows are provisional until the one owning native window/catalog check succeeds.
            self.batch = None;
            self.summaries = None;
            if let Some(window) = &mut self.window {
                window.invalidate_endpoint();
            }
        }
    }
    pub(crate) fn source_changed(&mut self) -> culprit_core::Error {
        let error = CollectionStop::SourceChanged.error();
        self.observe_failure(&error);
        error
    }
    pub(crate) async fn read_rows(&mut self, sql: &str) -> Result<Vec<Rec>> {
        self.read(sql, vec![], MAX_RESULT_ROWS).await
    }
    pub(crate) async fn window_rows(&mut self, sql: &str, minutes: u32) -> Result<Vec<Rec>> {
        let parameter = self.window(minutes).await?.parameter();
        self.read(sql, vec![parameter], MAX_RESULT_ROWS).await
    }
    pub(crate) async fn read(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> Result<Vec<Rec>> {
        let rows = self.query_native(sql, parameters, limit + 1).await.map_err(ReadError::into_error)?;
        if rows.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(rows)
    }
    pub(crate) async fn window(&mut self, window_minutes: u32) -> Result<super::window_sql::FrozenWindow> {
        self.runtime_layout().await?;
        self.native_window(window_minutes).await
    }
    /// Freeze native database/interval authority independently of runtime population discovery.
    async fn native_window(&mut self, window_minutes: u32) -> Result<super::window_sql::FrozenWindow> {
        self.admit()?;
        if let Some(window) = &self.window {
            return if window.matches_request(window_minutes) { Ok(window.clone()) } else { Err(invalid_data()) };
        }
        let layout = self.native_layout().await?;
        let detail = if self.governance_available {
            self.governance_detail
        } else {
            crate::database_identity::log_rates::Detail::Identity
        };
        let rows = self
            .read(
                &super::window_sql::metadata_sql_detail(layout.waits, false, layout.edition, detail),
                vec![Parameter::Integer(i64::from(window_minutes))],
                1,
            )
            .await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let window = match super::window_sql::FrozenWindow::decode_detail(row, window_minutes, layout.waits, detail) {
            Ok(window) => window,
            Err(error) => {
                self.observe_failure(&error);
                return Err(error);
            }
        };
        if !window.retained
            || window.database.edition != layout.edition
            || window.page_server_reads != layout.page_server_reads
        {
            return Err(self.source_changed());
        }
        self.window = Some(window.clone());
        Ok(window)
    }
    async fn runtime_layout(&mut self) -> Result<RuntimeLayout> {
        self.admit()?;
        if let Some(layout) = self.layout {
            return Ok(layout);
        }
        let native = self.native_layout().await?;
        self.prepare_population_catalog(native).await?;
        if self.empty_replica_catalog && self.population_catalog.is_none() {
            // A population-only call has no authenticated runtime window or rows to establish bounded membership.
            return Err(self.source_changed());
        }
        let population = match &self.replica_catalog {
            Some(catalog) => match catalog.select(self.requested_population.as_ref()) {
                Ok(population) => population,
                Err(error) => {
                    self.stopped = Some(error.clone());
                    return Err(error);
                }
            },
            None if self.requested_population.is_some() => {
                if self.empty_replica_catalog {
                    self.select_observed_population()?
                } else {
                    let error = culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("provider.mssql.query_store.population_missing"),
                    );
                    self.stopped = Some(error.clone());
                    return Err(error);
                }
            }
            None if self.empty_replica_catalog => self.select_observed_population()?,
            None => super::replicas::Population::Unpartitioned,
        };
        let layout = RuntimeLayout { variants: native.variants, mode: native.mode, population };
        self.layout = Some(layout);
        Ok(layout)
    }

    async fn native_layout(&mut self) -> Result<NativeLayout> {
        self.admit()?;
        if let Some(layout) = self.native_layout {
            return Ok(layout);
        }
        let rows = self.read(&runtime::layout_query(self.governance_detail), vec![], 1).await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let layout = NativeLayout::decode(row)?;
        if self.governance_detail == crate::database_identity::log_rates::Detail::ConfiguredLogRates {
            match crate::database_identity::log_rates::capability(row) {
                Ok(available) => self.governance_available = available,
                Err(error) => self.governance_error = Some(error),
            }
        }
        self.native_layout = Some(layout);
        Ok(layout)
    }

    /// Runtime membership has its own authority. Query-global compiler facts need only the native layout.
    async fn prepare_population_catalog(&mut self, layout: NativeLayout) -> Result<()> {
        self.admit()?;
        if self.population_catalog.is_some() || self.empty_replica_catalog {
            return Ok(());
        }
        match layout.partitioned {
            false => {
                if layout.mode == super::mode::ReadMode::ReadCaptureSecondary {
                    let error = culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("provider.mssql.query_store.population_unavailable"),
                    );
                    self.stopped = Some(error.clone());
                    return Err(error);
                }
                self.population_catalog = Some(super::replicas::unpartitioned_catalog());
            }
            true => {
                let rows =
                    self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
                if rows.is_empty() {
                    self.empty_replica_catalog = true;
                    return Ok(());
                }
                let catalog = super::replicas::Catalog::decode(&rows).map_err(|_| self.source_changed())?;
                self.population_catalog = Some(catalog.population_catalog()?);
                self.replica_catalog = Some(catalog);
            }
        }
        Ok(())
    }

    async fn query_native(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        row_limit: usize,
    ) -> crate::read::ReadResult<Vec<Rec>> {
        self.admit().map_err(ReadError::Source)?;
        let result = self.io.query(sql, parameters, row_limit).await;
        match &result {
            Err(ReadError::Source(error)) => self.observe_failure(error),
            Err(ReadError::Stopped(stop)) => self.observe_failure(&stop.error()),
            Ok(_) => (),
        }
        let rows = result?;
        let generation = self.io.connection_generation();
        if self.connection_generation.is_some() && generation != self.connection_generation {
            return Err(ReadError::Source(self.source_changed()));
        }
        self.connection_generation = generation;
        let charged = self.batch.as_mut().map(|batch| batch.charge(&rows)).transpose();
        if let Err(error) = charged {
            self.stopped = Some(error.clone());
            self.batch = None;
            self.summaries = None;
            return Err(ReadError::Source(error));
        }
        Ok(rows)
    }
    pub(crate) async fn verify_window(&mut self) -> Result<super::window_sql::WindowVerification> {
        self.window_verified = false;
        let mut window = self.window.as_ref().ok_or_else(invalid_data)?.clone();
        let (sql, parameters) = window.verify_query();
        let result = self.read(&sql, parameters, 1).await.and_then(|rows| {
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            window.verify(row)
        });
        if result.is_err() {
            window.invalidate_endpoint();
        }
        self.window = Some(window);
        result.as_ref().map_err(Clone::clone)?;
        if self.window.as_ref().is_some_and(|window| !window.retained) {
            return result;
        }
        self.verify_population().await?;
        self.refresh_observed_window()?;
        self.window_verified = result.is_ok();
        result
    }
    pub(crate) async fn verify_population(&mut self) -> Result<()> {
        self.admit()?;
        if self.is_population_batch() || self.empty_replica_catalog {
            return self.verify_batch_catalog().await;
        }
        let layout = self.layout.ok_or_else(invalid_data)?;
        if let super::replicas::Population::ReplicaGroup(_) = layout.population {
            let rows = self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
            let current = super::replicas::Catalog::decode(&rows).map_err(|_| self.source_changed())?;
            if current.select(layout.population.identity()?.as_ref()).ok() != Some(layout.population) {
                return Err(self.source_changed());
            }
        }
        Ok(())
    }
    async fn verify_batch_catalog(&mut self) -> Result<()> {
        let rows = self.read(runtime::LAYOUT, vec![], 1).await?;
        let [row] = rows.as_slice() else { return Err(self.source_changed()) };
        if NativeLayout::decode(row).ok() != self.native_layout {
            return Err(self.source_changed());
        }
        if self.empty_replica_catalog {
            let rows = self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
            if !rows.is_empty() {
                return Err(self.source_changed());
            }
        } else if let Some(original) = self.replica_catalog.clone() {
            let rows = self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
            let current = super::replicas::Catalog::decode(&rows).map_err(|_| self.source_changed())?;
            if current != original {
                return Err(self.source_changed());
            }
        }
        Ok(())
    }
    pub(crate) async fn finish_population_batch(
        &mut self,
    ) -> Result<(super::window_sql::FrozenWindow, super::window_sql::WindowVerification)> {
        self.admit()?;
        if !self.is_population_batch() {
            return Err(invalid_data());
        }
        let verification = self.verify_window().await?;
        if verification == super::window_sql::WindowVerification::RuntimePopulationChanged {
            return Err(self.source_changed());
        }
        Ok((self.window.as_ref().ok_or_else(invalid_data)?.clone(), verification))
    }
    pub(crate) async fn summary(&mut self, window_minutes: u32) -> Result<Vec<PlanSummary>> {
        let native = self.native_layout().await?;
        self.prepare_population_catalog(native).await?;
        if !self.empty_replica_catalog {
            self.runtime_layout().await?;
        }
        if self.batch.is_none() && !self.empty_replica_catalog {
            self.summaries = None;
        }
        self.load_summaries(window_minutes).await?;
        let population = self.population().await?;
        Ok(self.summaries()?.iter().filter(|row| row.population == population).cloned().collect())
    }
    pub(crate) async fn waits(
        &mut self,
        window_minutes: u32,
        plan_ids: Option<&[String]>,
    ) -> Result<Vec<super::waits::Summary>> {
        if plan_ids.is_some_and(|ids| ids.is_empty()) {
            return Ok(Vec::new());
        }
        if self.batch.is_some() {
            return self.batch_waits(window_minutes, plan_ids).await;
        }
        let window = self.window(window_minutes).await?;
        if window.capture == culprit_core::telemetry::WaitCaptureState::Unavailable {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("provider.mssql.query_waits.not_captured"),
            ));
        }
        let mut parameters = vec![window.parameter()];
        if let Some(ids) = plan_ids {
            parameters.push(ids_parameter(ids)?);
        }
        let population = self.population().await?;
        self.read(&super::waits::summary_sql(plan_ids.is_some(), population), parameters, MAX_RESULT_ROWS)
            .await?
            .iter()
            .map(|row| {
                let summary = super::waits::Summary::decode(row, plan_ids.is_some())?;
                if summary.population != population {
                    return Err(self.source_changed());
                }
                Ok(summary)
            })
            .collect()
    }
    pub(crate) async fn definitions(
        &mut self,
        ids: &[String],
        kind: super::definitions::Kind,
        limit: usize,
    ) -> Result<Vec<super::definitions::Definition>> {
        let result = super::definitions::read(self, ids, kind, limit).await;
        if let Err(error) = &result {
            self.observe_failure(error);
        }
        result
    }
    pub(crate) async fn compilations(&mut self, ids: &[String]) -> Result<Vec<super::costs::Compilation>> {
        self.require_compilation_profile()?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.read(&super::costs::compilation_sql(), vec![ids_parameter(ids)?], MAX_RESULT_ROWS).await?;
        let mut observed = std::collections::BTreeSet::new();
        let mut compilations = Vec::with_capacity(rows.len());
        for row in &rows {
            let compilation = super::costs::Compilation::decode(row)?;
            if !ids.contains(&compilation.identity.query_id) || !observed.insert(compilation.identity.query_id.clone())
            {
                return Err(self.source_changed());
            }
            compilations.push(compilation);
        }
        Ok(compilations)
    }
    pub(crate) fn variant_families_supported(&self) -> bool {
        self.variant_family_availability() == super::variants_sql::Availability::Readable
    }
    pub(crate) fn variant_family_availability(&self) -> super::variants_sql::Availability {
        self.layout.map(|layout| layout.variants).unwrap_or(super::variants_sql::Availability::Unavailable)
    }
    pub(crate) async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> NativeRead for Reader<R> {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        row_limit: usize,
    ) -> crate::read::ReadResult<Vec<Rec>> {
        self.query_native(sql, parameters, row_limit).await
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

pub(crate) fn ids_parameter(ids: &[String]) -> Result<Parameter> {
    let mut xml = String::from("<ids>");
    for id in ids {
        let id = id.parse::<i64>().ok().filter(|id| *id >= 0).ok_or_else(invalid_data)?;
        xml.push_str(&format!("<id id=\"{id}\"/>"));
    }
    xml.push_str("</ids>");
    Ok(Parameter::Text(xml))
}

#[cfg(test)]
mod compilation_profile_tests;
#[cfg(test)]
mod native_compilation;
#[cfg(test)]
mod native_observed_populations;
#[cfg(test)]
mod native_page_server;
#[cfg(test)]
mod native_population_profile;
#[cfg(test)]
mod observed_tests;
#[cfg(test)]
mod replica_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod configured_log_rate_tests;
