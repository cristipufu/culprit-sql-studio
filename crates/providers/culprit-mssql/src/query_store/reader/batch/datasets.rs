//! One frozen native dataset feeds every selected population in a batch.
use super::*;
use crate::query_store::{decode, replicas::Population, series_sql, waits};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct PlanWait {
    measurement: waits::Summary,
    identity: NativePlanIdentity,
}

impl<R: NativeRead> Reader<R> {
    pub(in crate::query_store::reader) async fn batch_summary(&mut self, minutes: u32) -> Result<Vec<PlanSummary>> {
        self.load_batch_summaries(minutes).await?;
        let population = self.population().await?;
        Ok(self.batch_summaries()?.iter().filter(|row| row.population == population).cloned().collect())
    }

    async fn load_batch_summaries(&mut self, minutes: u32) -> Result<()> {
        self.admit()?;
        if let Some(result) = self.batch.as_ref().and_then(|batch| batch.summaries.as_ref()) {
            return result.as_ref().map(|_| ()).map_err(Clone::clone);
        }
        let window = self.window(minutes).await?;
        let layout = self.native_layout.ok_or_else(invalid_data)?;
        let population = self.acquisition_population()?;
        let result = self
            .read(
                &runtime::summary_sql(layout.tempdb, layout.log_bytes, population),
                vec![window.parameter()],
                MAX_RESULT_ROWS,
            )
            .await
            .and_then(|rows| {
                let summaries = rows.iter().map(PlanSummary::decode).collect::<Result<Vec<_>>>()?;
                self.validate_batch_summaries(&summaries)?;
                Ok(summaries)
            });
        if let Some(batch) = &mut self.batch {
            batch.summaries = Some(result.clone());
        }
        if let Err(error) = &result {
            self.observe_failure(error);
        }
        result.map(|_| ())
    }

    fn batch_summaries(&self) -> Result<&[PlanSummary]> {
        self.batch
            .as_ref()
            .and_then(|batch| batch.summaries.as_ref())
            .ok_or_else(invalid_data)?
            .as_deref()
            .map_err(Clone::clone)
    }

    fn validate_batch_summaries(&mut self, summaries: &[PlanSummary]) -> Result<()> {
        let mut queries = HashMap::new();
        let mut plans = HashMap::new();
        let mut observed = BTreeSet::new();
        for summary in summaries {
            if !self.batch_population_is_known(summary.population)
                || !summary.identity.is_valid()
                || !summary.plan_identity.is_valid()
                || !observed.insert((summary.population, summary.plan_id.clone()))
                || queries
                    .insert(summary.identity.query_id.clone(), &summary.identity)
                    .is_some_and(|identity| identity != &summary.identity)
                || plans
                    .insert(summary.plan_id.clone(), &summary.plan_identity)
                    .is_some_and(|identity| identity != &summary.plan_identity)
            {
                return Err(self.source_changed());
            }
        }
        Ok(())
    }

    pub(in crate::query_store::reader) async fn batch_waits(
        &mut self,
        minutes: u32,
        plan_ids: Option<&[String]>,
    ) -> Result<Vec<waits::Summary>> {
        self.load_batch_summaries(minutes).await?;
        let window = self.window(minutes).await?;
        if window.capture == culprit_core::telemetry::WaitCaptureState::Unavailable {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("provider.mssql.query_waits.not_captured"),
            ));
        }
        if self.batch.as_ref().and_then(|batch| batch.waits.as_ref()).is_none() {
            let result = self
                .read(&waits::all_plans_sql(self.acquisition_population()?), vec![window.parameter()], MAX_RESULT_ROWS)
                .await
                .and_then(|rows| self.decode_batch_waits(&rows));
            if let Some(batch) = &mut self.batch {
                batch.waits = Some(result.clone());
            }
            if let Err(error) = &result {
                self.observe_failure(error);
            }
            result?;
        }
        let population = self.population().await?;
        let batch = self.batch.as_ref().ok_or_else(invalid_data)?;
        let measurements = batch.waits.as_ref().ok_or_else(invalid_data)?.as_ref().map_err(Clone::clone)?;
        let plans = self
            .batch_summaries()?
            .iter()
            .filter(|summary| summary.population == population)
            .map(|summary| (summary.plan_id.as_str(), &summary.plan_identity))
            .collect::<HashMap<_, _>>();
        Ok(measurements
            .iter()
            .filter(|wait| wait.measurement.population == population)
            .filter(|wait| {
                plan_ids.is_none_or(|ids| {
                    ids.contains(&wait.identity.plan_id)
                        && plans.get(wait.identity.plan_id.as_str()).is_some_and(|original| **original == wait.identity)
                })
            })
            .map(|wait| {
                let mut measurement = wait.measurement.clone();
                if plan_ids.is_none() {
                    measurement.plan_id = None;
                }
                measurement
            })
            .collect())
    }

    fn decode_batch_waits(&mut self, rows: &[Rec]) -> Result<Vec<PlanWait>> {
        let waits = rows
            .iter()
            .map(|row| {
                let measurement = waits::Summary::decode(row, true)?;
                let query = decode::query_identity(row)?;
                let identity = decode::plan_identity_for_query(row, "", query)?
                    .filter(|identity| identity.is_valid() && measurement.plan_id.as_ref() == Some(&identity.plan_id))
                    .ok_or_else(invalid_data)?;
                Ok(PlanWait { measurement, identity })
            })
            .collect::<Result<Vec<_>>>()?;
        if !self.batch_waits_are_consistent(&waits)? {
            return Err(self.source_changed());
        }
        Ok(waits)
    }

    fn batch_waits_are_consistent(&self, waits: &[PlanWait]) -> Result<bool> {
        let summaries = self.batch_summaries()?;
        let queries = summaries
            .iter()
            .map(|summary| (summary.identity.query_id.as_str(), &summary.identity))
            .collect::<HashMap<_, _>>();
        let plans = summaries
            .iter()
            .map(|summary| (summary.plan_id.as_str(), &summary.plan_identity))
            .collect::<HashMap<_, _>>();
        let mut observed = BTreeSet::new();
        for wait in waits {
            let PlanWait { measurement, identity } = wait;
            let changed = plans.get(identity.plan_id.as_str()).is_some_and(|previous| *previous != identity)
                || queries.get(identity.query.query_id.as_str()).is_some_and(|previous| **previous != identity.query);
            if !self.batch_population_is_known(measurement.population)
                || changed
                || !observed.insert((measurement.population, identity.plan_id.clone(), measurement.category_id))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) async fn series_interval(&mut self) -> Result<u32> {
        self.admit()?;
        if let Some(result) = self.batch.as_ref().and_then(|batch| batch.interval.as_ref()) {
            return result.clone();
        }
        let result = self.read_rows(series_sql::INTERVAL_SQL).await.and_then(|rows| {
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            u32::try_from(integer(row, "minutes")?).ok().filter(|minutes| *minutes > 0).ok_or_else(invalid_data)
        });
        if let Some(batch) = &mut self.batch {
            batch.interval = Some(result.clone());
        }
        result
    }

    pub(crate) async fn series_rows(
        &mut self,
        window_minutes: u32,
        bucket_minutes: u32,
        query_ids: &[String],
    ) -> Result<(Vec<Rec>, Vec<Rec>)> {
        self.admit()?;
        if self.batch.is_none() {
            let population = self.population().await?;
            let totals = self.window_rows(&series_sql::totals_sql(bucket_minutes, population), window_minutes).await?;
            self.validate_series_populations(&totals, population)?;
            let queries = if totals.is_empty() || query_ids.is_empty() {
                Vec::new()
            } else {
                self.window_rows(
                    &series_sql::per_query_sql(bucket_minutes, &crate::workload::id_list(query_ids), population),
                    window_minutes,
                )
                .await?
            };
            self.validate_series_populations(&queries, population)?;
            return Ok((totals, queries));
        }
        self.load_batch_series(window_minutes, bucket_minutes).await?;
        let population = self.population().await?;
        let batch = self.batch.as_ref().ok_or_else(invalid_data)?;
        if query_ids.iter().any(|id| !batch.trend_queries.contains(id)) {
            return Err(invalid_data());
        }
        let selected = |result: &Option<Result<Vec<Rec>>>, queries: bool| -> Result<Vec<Rec>> {
            Ok(result
                .as_ref()
                .ok_or_else(invalid_data)?
                .as_ref()
                .map_err(Clone::clone)?
                .iter()
                .filter(|row| Population::from_row(row).ok() == Some(population))
                .filter(|row| !queries || query_ids.contains(&row.string("query_id")))
                .cloned()
                .collect())
        };
        Ok((selected(&batch.totals, false)?, selected(&batch.per_query, true)?))
    }

    async fn load_batch_series(&mut self, window_minutes: u32, bucket_minutes: u32) -> Result<()> {
        self.load_batch_summaries(window_minutes).await?;
        let batch = self.batch.as_ref().ok_or_else(invalid_data)?;
        if batch.series_minutes.is_some_and(|minutes| minutes != bucket_minutes) {
            return Err(invalid_data());
        }
        if batch.series_minutes.is_none() {
            let askers =
                batch.askers.as_ref().and_then(|result| result.as_ref().ok()).map(Vec::as_slice).unwrap_or_default();
            let queries =
                crate::workload::population_trend_queries(self.batch_summaries()?, batch.top_queries, askers)?;
            let batch = self.batch.as_mut().ok_or_else(invalid_data)?;
            batch.series_minutes = Some(bucket_minutes);
            batch.trend_queries = queries;
        }
        let population = self.acquisition_population()?;
        if self.batch.as_ref().and_then(|batch| batch.totals.as_ref()).is_none() {
            let result = self
                .window_rows(&series_sql::totals_sql(bucket_minutes, population), window_minutes)
                .await
                .and_then(|rows| {
                    self.validate_series_populations(&rows, population)?;
                    Ok(rows)
                });
            if let Some(batch) = &mut self.batch {
                batch.totals = Some(result.clone());
            }
            result?;
        }
        self.batch
            .as_ref()
            .and_then(|batch| batch.totals.as_ref())
            .ok_or_else(invalid_data)?
            .as_ref()
            .map_err(Clone::clone)?;
        if self.batch.as_ref().and_then(|batch| batch.per_query.as_ref()).is_none() {
            let queries = self.batch.as_ref().ok_or_else(invalid_data)?.trend_queries.clone();
            let totals_empty = self
                .batch
                .as_ref()
                .and_then(|batch| batch.totals.as_ref())
                .ok_or_else(invalid_data)?
                .as_ref()
                .map_err(Clone::clone)?
                .is_empty();
            let result = if totals_empty || queries.is_empty() {
                Ok(Vec::new())
            } else {
                self.window_rows(
                    &series_sql::per_query_sql(bucket_minutes, &crate::workload::id_list(&queries), population),
                    window_minutes,
                )
                .await
                .and_then(|rows| self.validate_batch_series_queries(rows, &queries))
            };
            if let Some(batch) = &mut self.batch {
                batch.per_query = Some(result.clone());
            }
            result?;
        }
        Ok(())
    }

    fn validate_batch_series_queries(&mut self, rows: Vec<Rec>, ids: &[String]) -> Result<Vec<Rec>> {
        self.validate_series_populations(&rows, self.acquisition_population()?)?;
        let queries = self
            .batch_summaries()?
            .iter()
            .map(|summary| ((summary.population, summary.identity.query_id.clone()), summary.identity.clone()))
            .collect::<HashMap<_, _>>();
        for row in &rows {
            let identity = decode::query_identity(row)?;
            let population = Population::from_row(row)?;
            if !ids.contains(&identity.query_id)
                || queries.get(&(population, identity.query_id.clone())) != Some(&identity)
            {
                return Err(self.source_changed());
            }
        }
        Ok(rows)
    }

    fn validate_series_populations(&mut self, rows: &[Rec], expected: Population) -> Result<()> {
        for row in rows {
            let population = Population::from_row(row)?;
            if !self.batch_population_is_known(population)
                || expected != Population::AllGroups && population != expected
            {
                return Err(self.source_changed());
            }
        }
        Ok(())
    }

    fn acquisition_population(&self) -> Result<Population> {
        Ok(if self.native_layout.ok_or_else(invalid_data)?.partitioned {
            Population::AllGroups
        } else {
            Population::Unpartitioned
        })
    }

    fn batch_population_is_known(&self, population: Population) -> bool {
        match population {
            Population::ReplicaGroup(_) => self.replica_catalog.as_ref().is_some_and(|catalog| {
                population
                    .identity()
                    .ok()
                    .is_some_and(|identity| catalog.select(identity.as_ref()).ok() == Some(population))
            }),
            Population::Unpartitioned => self.native_layout.is_some_and(|layout| !layout.partitioned),
            Population::AllGroups => false,
        }
    }
}

#[cfg(test)]
mod tests;
