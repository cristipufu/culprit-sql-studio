//! Multiple runtime populations share one owned native budget and immutable, incarnation-qualified definitions.
use super::*;
use crate::query_store::definitions::{Definition, Kind};
use culprit_core::telemetry::{NativePlanIdentity, NativeQueryIdentity};
use std::collections::{BTreeMap, HashMap};

mod datasets;
const MAX_BATCH_ROWS: usize = 100_000;

pub(super) struct Batch {
    budget: culprit_read::ReadBudget,
    definitions: BTreeMap<String, Definition>,
    askers: Option<Result<Vec<Rec>>>,
    waits: Option<Result<Vec<datasets::PlanWait>>>,
    interval: Option<Result<u32>>,
    totals: Option<Result<Vec<Rec>>>,
    per_query: Option<Result<Vec<Rec>>>,
    series_minutes: Option<u32>,
    trend_queries: Vec<String>,
    top_queries: usize,
}

impl Batch {
    fn new(top_queries: usize) -> Self {
        Self {
            budget: culprit_read::ReadBudget::new(MAX_BATCH_ROWS, CollectionBudget::default().total_bytes as usize),
            definitions: BTreeMap::new(),
            askers: None,
            waits: None,
            interval: None,
            totals: None,
            per_query: None,
            series_minutes: None,
            trend_queries: Vec::new(),
            top_queries,
        }
    }

    pub(super) fn charge(&mut self, rows: &[Rec]) -> Result<()> {
        for row in rows {
            self.budget.row_started()?;
            self.budget.charge_values(crate::read::value_bytes(row))?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", content = "identity")]
enum ExpectedDefinition {
    Query(NativeQueryIdentity),
    Plan(NativePlanIdentity),
}

impl ExpectedDefinition {
    fn key(&self, limit: usize) -> Result<String> {
        serde_json::to_vec(&(self, limit))
            .map(|bytes| culprit_core::telemetry::digest(&bytes))
            .map_err(|_| invalid_data())
    }

    fn matches(&self, definition: &Definition) -> bool {
        match self {
            Self::Query(identity) => definition.query.as_ref() == Some(identity) && definition.plan.is_none(),
            Self::Plan(identity) => {
                definition.query.as_ref() == Some(&identity.query) && definition.plan.as_ref() == Some(identity)
            }
        }
    }
}

impl<R: NativeRead> Reader<R> {
    /// Discovery uses the same initial layout/catalog read as a selected workload. It does not choose a group.
    pub(crate) async fn prepare_population_batch(
        &mut self,
        top_queries: usize,
        window_minutes: u32,
    ) -> Result<RuntimePopulationCatalog> {
        self.admit()?;
        if self.batch.is_some() || self.native_layout.is_some() || self.layout.is_some() || self.window.is_some() {
            return Err(invalid_data());
        }
        self.batch = Some(Batch::new(top_queries));
        let native = self.native_layout().await?;
        self.prepare_population_catalog(native).await?;
        if self.empty_replica_catalog {
            self.load_summaries(window_minutes).await?;
        }
        self.population_catalog.clone().ok_or_else(invalid_data)
    }

    /// A new logical population does not reset the native session, deadline, row/byte limits or definition cache.
    pub(crate) async fn select_batch_population(&mut self, population: Option<&NativeIdentity>) -> Result<()> {
        self.admit()?;
        if self.batch.is_none() {
            return Err(invalid_data());
        }
        if let Some(population) = population {
            super::super::replicas::requested_group(population)?;
        }
        match &self.replica_catalog {
            Some(catalog) => {
                catalog.select(population)?;
            }
            None if population.is_some() => {
                if self.empty_replica_catalog {
                    let catalog = self.population_catalog.as_ref().ok_or_else(invalid_data)?;
                    if !population.is_some_and(|population| catalog.contains(population)) {
                        return Err(culprit_core::Error::new(
                            culprit_core::ErrorCode::Unsupported,
                            culprit_core::t!("provider.mssql.query_store.population_missing"),
                        ));
                    }
                } else {
                    return Err(culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("provider.mssql.query_store.population_missing"),
                    ));
                }
            }
            None => (),
        }
        self.requested_population = population.cloned();
        self.layout = None;
        self.runtime_layout().await?;
        Ok(())
    }

    pub(crate) async fn missing_index_askers(&mut self) -> Result<Vec<Rec>> {
        self.admit()?;
        if let Some(cached) = self.batch.as_ref().and_then(|batch| batch.askers.as_ref()) {
            return cached.clone();
        }
        let result = self.read_rows(super::super::catalog_sql::MISSING_INDEX_ASKERS_SQL).await;
        if let Some(batch) = &mut self.batch {
            batch.askers = Some(result.clone());
        }
        result
    }

    pub(crate) async fn query_definitions(
        &mut self,
        ids: &[String],
        identities: &HashMap<String, NativeQueryIdentity>,
        limit: usize,
    ) -> Result<Vec<Definition>> {
        let expected = ids
            .iter()
            .map(|id| {
                let identity = identities.get(id).filter(|identity| identity.is_valid()).ok_or_else(invalid_data)?;
                Ok((id.clone(), ExpectedDefinition::Query(identity.clone())))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.qualified_definitions(expected, Kind::Query, limit).await
    }

    pub(crate) async fn plan_definitions(
        &mut self,
        ids: &[String],
        identities: &HashMap<String, NativePlanIdentity>,
        limit: usize,
    ) -> Result<Vec<Definition>> {
        let expected = ids
            .iter()
            .map(|id| {
                let identity = identities.get(id).filter(|identity| identity.is_valid()).ok_or_else(invalid_data)?;
                Ok((id.clone(), ExpectedDefinition::Plan(identity.clone())))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.qualified_definitions(expected, Kind::Plan, limit).await
    }

    async fn qualified_definitions(
        &mut self,
        expected: BTreeMap<String, ExpectedDefinition>,
        kind: Kind,
        limit: usize,
    ) -> Result<Vec<Definition>> {
        self.admit()?;
        let mut found = BTreeMap::new();
        let mut missing = Vec::new();
        for (id, identity) in &expected {
            let key = identity.key(limit)?;
            match self.batch.as_ref().and_then(|batch| batch.definitions.get(&key)) {
                Some(definition) if identity.matches(definition) => {
                    found.insert(id.clone(), definition.clone());
                }
                Some(_) => return Err(self.source_changed()),
                None => missing.push(id.clone()),
            }
        }
        let definitions = self.definitions(&missing, kind, limit).await?;
        if definitions.len() != missing.len() {
            return Err(self.source_changed());
        }
        for definition in definitions {
            let Some(identity) = expected.get(&definition.id).filter(|identity| identity.matches(&definition)) else {
                return Err(self.source_changed());
            };
            if let Some(batch) = &mut self.batch {
                batch.definitions.insert(identity.key(limit)?, definition.clone());
            }
            found.insert(definition.id.clone(), definition);
        }
        Ok(found.into_values().collect())
    }
}

#[cfg(test)]
mod tests;
