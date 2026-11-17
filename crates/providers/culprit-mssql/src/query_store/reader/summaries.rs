//! One fully drained native summary owns its observed groups and every selected workload derived from them.
use super::*;
use crate::query_store::replicas::Population;
use culprit_core::{Error, ErrorCode, RuntimePopulationMembership};
use std::collections::{BTreeSet, HashMap};

impl<R: NativeRead> Reader<R> {
    pub(super) async fn load_summaries(&mut self, minutes: u32) -> Result<()> {
        self.admit()?;
        if let Some(result) = &self.summaries {
            if self.window.as_ref().is_none_or(|window| !window.matches_request(minutes)) {
                return Err(invalid_data());
            }
            return result.as_ref().map(|_| ()).map_err(Clone::clone);
        }
        let window = match self.native_window(minutes).await {
            Ok(window) => window,
            Err(error) => {
                self.summary_failed(&error);
                return Err(error);
            }
        };
        if self.empty_replica_catalog {
            let membership = window.start.as_ref().zip(window.end.as_ref()).map(|(from, until)| {
                RuntimePopulationMembership::ObservedWindow { from: from.clone(), until: until.clone() }
            });
            if membership.is_none_or(|membership| !membership.is_valid()) {
                let error = unobserved();
                self.summary_failed(&error);
                return Err(error);
            }
        }
        let layout = self.native_layout.ok_or_else(invalid_data)?;
        let population = if self.batch.is_some() || self.empty_replica_catalog {
            if layout.partitioned { Population::AllGroups } else { Population::Unpartitioned }
        } else {
            self.layout.ok_or_else(invalid_data)?.population
        };
        let result = self
            .read(
                &runtime::summary_sql(layout.tempdb, layout.log_bytes, layout.page_server_reads, population),
                vec![window.parameter()],
                MAX_RESULT_ROWS,
            )
            .await
            .and_then(|rows| {
                let summaries = rows.iter().map(PlanSummary::decode).collect::<Result<Vec<_>>>()?;
                self.validate_summaries(&summaries, population)?;
                if self.empty_replica_catalog {
                    let (Some(from), Some(until)) = (&window.start, &window.end) else {
                        return Err(unobserved());
                    };
                    self.population_catalog = Some(super::super::replicas::observed_catalog(
                        summaries.iter().map(|row| row.population),
                        from,
                        until,
                    )?);
                }
                Ok(summaries)
            });
        if let Err(error) = &result {
            self.summary_failed(error);
        }
        self.summaries = Some(result.clone());
        result.map(|_| ())
    }

    fn summary_failed(&mut self, error: &Error) {
        self.observe_failure(error);
        if self.empty_replica_catalog {
            // Failed observed membership cannot authorize a later dataset or an independent fallback read.
            self.stopped = Some(error.clone());
            self.batch = None;
            if let Some(window) = &mut self.window {
                window.invalidate_endpoint();
            }
        }
    }

    pub(super) fn summaries(&self) -> Result<&[PlanSummary]> {
        self.summaries.as_ref().ok_or_else(invalid_data)?.as_deref().map_err(Clone::clone)
    }

    fn validate_summaries(&mut self, summaries: &[PlanSummary], expected: Population) -> Result<()> {
        let mut queries = HashMap::new();
        let mut plans = HashMap::new();
        let mut observed = BTreeSet::new();
        for summary in summaries {
            let membership = if self.empty_replica_catalog && self.population_catalog.is_none() {
                matches!(summary.population, Population::ReplicaGroup(_))
            } else {
                match summary.population {
                    Population::ReplicaGroup(_) => self.population_catalog.as_ref().is_some_and(|catalog| {
                        summary.population.identity().ok().flatten().is_some_and(|identity| catalog.contains(&identity))
                    }),
                    Population::Unpartitioned => self.native_layout.is_some_and(|layout| !layout.partitioned),
                    Population::AllGroups => false,
                }
            };
            if !membership
                || expected != Population::AllGroups && summary.population != expected
                || self.native_layout.is_none_or(|layout| {
                    summary.page_server_reads.statistics().availability != layout.page_server_reads
                })
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

    pub(super) fn select_observed_population(&self) -> Result<Population> {
        let catalog = self
            .population_catalog
            .as_ref()
            .filter(|catalog| {
                catalog.is_valid() && matches!(catalog.membership, RuntimePopulationMembership::ObservedWindow { .. })
            })
            .ok_or_else(unobserved)?;
        let selected = match &self.requested_population {
            Some(selected) if catalog.contains(selected) => selected,
            Some(_) => {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    culprit_core::t!("provider.mssql.query_store.population_missing"),
                ));
            }
            None if catalog.populations.len() == 1 => &catalog.populations[0].identity,
            None => {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    culprit_core::t!("provider.mssql.query_store.population_required"),
                ));
            }
        };
        Ok(Population::ReplicaGroup(super::super::replicas::requested_group(selected)?))
    }

    pub(super) fn refresh_observed_window(&mut self) -> Result<()> {
        if !self.empty_replica_catalog {
            return Ok(());
        }
        let window = self.window.as_ref().ok_or_else(invalid_data)?;
        let (Some(from), Some(until)) = (&window.start, &window.end) else {
            return Err(self.source_changed());
        };
        let catalog = self.population_catalog.as_mut().ok_or_else(unobserved)?;
        catalog.membership = RuntimePopulationMembership::ObservedWindow { from: from.clone(), until: until.clone() };
        if !catalog.is_valid() {
            return Err(self.source_changed());
        }
        Ok(())
    }
}

fn unobserved() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("provider.mssql.query_store.population_unobserved"))
}
