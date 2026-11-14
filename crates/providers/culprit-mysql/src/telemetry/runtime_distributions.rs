//! Freeze independent native quantile populations before delivery; never borrow a counter epoch or query text.
use crate::{
    connect::{Connector, Flavor, Scope},
    native::{
        self,
        metadata::{self, ConnectionContext},
        runtime_distributions as distributions, statements,
    },
    read::{OwnedRead, ReadSession},
};
use culprit_core::{ErrorCode, MeasurementScope, ResetMarker, Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::{collections::BTreeSet, sync::Arc};

// Seven setup phases + three parameterized identity reads (six phases) + two column reads + parameterized summary
// and histogram reads (four phases) require nineteen phases. Keep the fixed twenty-four phase envelope; native
// cancellation retains separate cleanup admission. Visibility/CPU/overflow probes do not qualify these populations.
const MAX_QUERIES: u32 = 24;

pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min((2 * distributions::MAX_DIGESTS) as u32),
        page_rows: requested.page_rows.min((2 * distributions::MAX_DIGESTS) as u32),
        page_bytes: requested.page_bytes.min(1024 * 1024),
        total_bytes: requested.total_bytes.min(8 * 1024 * 1024),
        max_sql_bytes: requested.max_sql_bytes.min(16 * 1024),
        statement_timeout_ms: requested.statement_timeout_ms.min(2_000),
        total_timeout_ms: requested.total_timeout_ms.min(10_000),
        ..requested.clone()
    }
}

pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.collection() != TelemetryCollection::Snapshot(SnapshotDataset::RuntimeDistributions) {
        return Err(native::invalid());
    }
    let budget = bounded_budget(&request.budget);
    Ok(Box::new(FrozenSource::new(
        Reader {
            io: ReadSession::new(connector.clone(), budget.clone(), Scope::Server),
            connector,
            budget,
            context: None,
        },
        request,
    )?))
}

struct Reader<R: OwnedRead> {
    io: R,
    connector: Arc<Connector>,
    budget: CollectionBudget,
    context: Option<(ConnectionContext, BTreeSet<String>)>,
}

#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        let before = metadata::connection_context(&mut self.io, &self.connector).await?;
        if !before.performance_schema {
            return Err(culprit_core::Error::new(
                ErrorCode::Unsupported,
                culprit_core::message!("collection.unsupported"),
            ));
        }
        let columns = statements::columns(&mut self.io).await?;
        if !["SCHEMA_NAME", "DIGEST"].iter().all(|column| columns.contains(*column)) {
            return Err(native::invalid());
        }
        let digest_limit = (self.budget.max_snapshot_rows as usize).div_ceil(2).min(distributions::MAX_DIGESTS);
        let mut selected =
            distributions::read_summary(&mut self.io, &before.database_name, &columns, digest_limit).await?;
        let selected_complete = selected.len() <= digest_limit;
        selected.truncate(digest_limit);
        let mut digests = BTreeSet::new();
        let mut points = Vec::new();
        for row in selected {
            let identity = distributions::identity(&row, &before.database_name)?;
            if !digests.insert(identity.id.clone()) {
                return Err(native::invalid());
            }
            if let Some(distribution) = distributions::summary(&row) {
                points.push(StatementRuntimeDistribution {
                    identity,
                    scope: MeasurementScope::Database,
                    observed_at: native::text(&row, "observed_at")?,
                    reset: ResetMarker::Unknown,
                    distribution: distributions::valid_observation(distribution),
                });
            }
        }
        let summary_supported = columns.contains("QUANTILE_95") || columns.contains("QUANTILE_99");
        let histogram_supported =
            before.flavor == Flavor::Mysql && culprit_core::version_at_least(&before.version, [8, 0, 19]);
        let digests: Vec<_> = digests.into_iter().collect();
        let remaining = (self.budget.max_snapshot_rows as usize).saturating_sub(points.len());
        let histogram_digests = &digests[..digests.len().min(remaining)];
        let histogram_limited = histogram_supported && histogram_digests.len() < digests.len();
        let mut histogram_error = None;
        if histogram_supported && !histogram_digests.is_empty() {
            match distributions::read_histograms(
                &mut self.io,
                &before.database_name,
                histogram_digests,
                distributions::MAX_TOTAL_BINS,
            )
            .await
            {
                Ok(histograms) => {
                    for point in histograms {
                        points.push(StatementRuntimeDistribution {
                            identity: NativeIdentity::new(point.digest).qualified("schema", &before.database_name),
                            scope: MeasurementScope::Database,
                            observed_at: point.observed_at,
                            reset: ResetMarker::Unknown,
                            distribution: point.distribution,
                        });
                    }
                }
                Err(error) if error.code == ErrorCode::PermissionDenied => histogram_error = Some(error.code),
                Err(error) => return Err(error),
            }
        }
        let output_truncated = points.len() > self.budget.max_snapshot_rows as usize;
        points.truncate(self.budget.max_snapshot_rows as usize);
        let after = metadata::connection_context(&mut self.io, &self.connector).await?;
        if !before.same_source(&after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let bounds = ObservationBounds::new(&before.at, &after.at).ok_or_else(native::invalid)?;
        if points.iter().any(|point| !point.is_valid() || !bounds.contains(&point.observed_at)) {
            return Err(native::invalid());
        }
        let mut sources = Vec::new();
        for (source, supported, error) in [
            (distributions::SUMMARY, summary_supported, None),
            (distributions::HISTOGRAM, histogram_supported, histogram_error),
        ] {
            let count = points.iter().filter(|point| point.distribution.source == source).count() as u32;
            sources.push(if !supported || error.is_some() {
                StateSourceCoverage::unavailable(source.into(), error.unwrap_or(ErrorCode::Unsupported), None)
            } else {
                // Native containers, independent resets and monitoring-session membership are not fully qualified.
                // A bounded successful read is retained as partial evidence, never complete workload coverage.
                let mut coverage = StateSourceCoverage::observed(source.into(), count, false);
                if !selected_complete || histogram_limited || output_truncated {
                    coverage.stopped = Some(CollectionStop::RowLimit);
                }
                coverage
            });
        }
        let records = points
            .into_iter()
            .map(|point| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: point.distribution.source.clone(),
                    key: point.key()?,
                    fact: OperationalFact::RuntimeDistribution(Box::new(point)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let description = CollectionDescription {
            target: before.database.clone().into(),
            source: "mysql-runtime-distributions".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::RuntimeDistributions,
                observed_from: before.at.clone(),
                observed_until: after.at,
                records: records.len() as u32,
                sources,
            }),
        };
        self.context = Some((before, columns));
        Ok(FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let (before, columns) = self.context.as_ref().ok_or_else(native::invalid)?;
        let after = metadata::connection_context(&mut self.io, &self.connector).await?;
        let after_columns = statements::columns(&mut self.io).await?;
        Ok(before.same_source(&after) && *columns == after_columns)
    }

    fn query_count(&self) -> u32 {
        self.io.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

#[cfg(test)]
mod tests;
