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
const MAX_RESULT_ROWS: usize = 10_000;
const SNAPSHOT_BYTES: u32 = 8 * 1024 * 1024;
const SNAPSHOT_TIMEOUT_MS: u32 = 120_000;

pub(crate) struct Reader<R = ReadSession> {
    io: R,
    layout: Option<RuntimeLayout>,
    window: Option<super::window_sql::FrozenWindow>,
    stopped: Option<culprit_core::Error>,
    requested_population: Option<NativeIdentity>,
    population_catalog: Option<RuntimePopulationCatalog>,
}
#[derive(Clone, Copy)]
struct RuntimeLayout {
    tempdb: bool,
    log_bytes: bool,
    waits: bool,
    variants: super::variants_sql::Availability,
    population: super::replicas::Population,
    mode: super::mode::ReadMode,
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
        Self { io, layout: None, window: None, stopped: None, requested_population: None, population_catalog: None }
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
    pub(crate) fn selected_population(&self) -> Option<NativeIdentity> {
        self.layout.and_then(|layout| layout.population.identity())
    }
    pub(crate) fn accepts_local_capture(&self) -> Option<bool> {
        self.layout.map(|layout| layout.mode.accepts_local_capture())
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
        self.admit()?;
        let result = self.io.query(sql, parameters, limit + 1).await.map_err(ReadError::into_error);
        if let Err(error) = &result {
            self.observe_failure(error);
        }
        let rows = result?;
        if rows.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(rows)
    }
    pub(crate) async fn window(&mut self, window_minutes: u32) -> Result<super::window_sql::FrozenWindow> {
        self.admit()?;
        if let Some(window) = &self.window {
            return if window.matches_request(window_minutes) { Ok(window.clone()) } else { Err(invalid_data()) };
        }
        let layout = self.runtime_layout().await?;
        let rows = self
            .read(
                &super::window_sql::metadata_sql(layout.waits, false),
                vec![Parameter::Integer(i64::from(window_minutes))],
                1,
            )
            .await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let window = match super::window_sql::FrozenWindow::decode(row, window_minutes, layout.waits) {
            Ok(window) => window,
            Err(error) => {
                self.observe_failure(&error);
                return Err(error);
            }
        };
        if !window.retained {
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
        let rows = self.read(runtime::LAYOUT, vec![], 1).await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let tempdb = match integer(row, "tempdb")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        let log_bytes = match integer(row, "log_bytes")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        let variants = super::variants_sql::Availability::decode(row, "variants")?;
        let mode =
            super::mode::ReadMode::from_native(&crate::read::text(row, "actual_state_desc")?).ok_or_else(|| {
                culprit_core::Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("collection.query_store_unavailable"),
                )
            })?;
        let waits = super::window_sql::waits_supported(row)?;
        let population = match integer(row, "replicas")? {
            0 => {
                if mode == super::mode::ReadMode::ReadCaptureSecondary {
                    let error = culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("provider.mssql.query_store.population_unavailable"),
                    );
                    self.stopped = Some(error.clone());
                    return Err(error);
                }
                self.population_catalog = Some(super::replicas::unpartitioned_catalog());
                if self.requested_population.is_some() {
                    let error = culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("provider.mssql.query_store.population_missing"),
                    );
                    self.stopped = Some(error.clone());
                    return Err(error);
                }
                super::replicas::Population::Unpartitioned
            }
            1 => {
                let rows =
                    self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
                let catalog = super::replicas::Catalog::decode(&rows).map_err(|_| self.source_changed())?;
                self.population_catalog = Some(catalog.population_catalog()?);
                match catalog.select(self.requested_population.as_ref()) {
                    Ok(population) => population,
                    Err(error) => {
                        self.stopped = Some(error.clone());
                        return Err(error);
                    }
                }
            }
            _ => return Err(invalid_data()),
        };
        let layout = RuntimeLayout { tempdb, log_bytes, waits, variants, population, mode };
        self.layout = Some(layout);
        Ok(layout)
    }
    pub(crate) async fn verify_window(&mut self) -> Result<super::window_sql::WindowVerification> {
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
        result
    }
    pub(crate) async fn verify_population(&mut self) -> Result<()> {
        self.admit()?;
        let layout = self.layout.ok_or_else(invalid_data)?;
        if let super::replicas::Population::ReplicaGroup(_) = layout.population {
            let rows = self.read(super::catalog_sql::REPLICAS, vec![], super::replicas::MAX_REPLICA_GROUP_ROWS).await?;
            let current = super::replicas::Catalog::decode(&rows).map_err(|_| self.source_changed())?;
            if current.select(layout.population.identity().as_ref()).ok() != Some(layout.population) {
                return Err(self.source_changed());
            }
        }
        Ok(())
    }
    pub(crate) async fn summary(&mut self, window_minutes: u32) -> Result<Vec<PlanSummary>> {
        let window = self.window(window_minutes).await?;
        let layout = self.layout.ok_or_else(invalid_data)?;
        let result = self
            .read(
                &runtime::summary_sql(layout.tempdb, layout.log_bytes, layout.population),
                vec![window.parameter()],
                MAX_RESULT_ROWS,
            )
            .await?
            .iter()
            .map(|row| {
                let summary = PlanSummary::decode(row)?;
                if summary.population != layout.population {
                    return Err(self.source_changed());
                }
                Ok(summary)
            })
            .collect();
        if let Err(error) = &result {
            self.observe_failure(error);
        }
        result
    }
    pub(crate) async fn waits(
        &mut self,
        window_minutes: u32,
        plan_ids: Option<&[String]>,
    ) -> Result<Vec<super::waits::Summary>> {
        if plan_ids.is_some_and(|ids| ids.is_empty()) {
            return Ok(Vec::new());
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
        self.admit().map_err(ReadError::Source)?;
        let result = self.io.query(sql, parameters, row_limit).await;
        match &result {
            Err(ReadError::Source(error)) => self.observe_failure(error),
            Err(ReadError::Stopped(stop)) => self.observe_failure(&stop.error()),
            Ok(_) => (),
        }
        result
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
mod replica_tests;
#[cfg(test)]
mod tests;
