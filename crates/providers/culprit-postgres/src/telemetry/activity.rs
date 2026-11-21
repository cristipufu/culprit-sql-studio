//! An unfiltered metadata baseline. Optional blocker/SQL reads belong to a later investigation.
use super::*;
use crate::native::{
    activity,
    local_context::{self, Context},
};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
#[cfg(test)]
mod tests;

const MAX_BASELINE_ROWS: u32 = 500;
const MAX_BASELINE_QUERIES: u32 = 6; // Owned setup, two database/member bounds and one Activity projection.
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(MAX_BASELINE_ROWS);
    budget.page_rows = budget.page_rows.min(MAX_BASELINE_ROWS);
    budget.max_queries = budget.max_queries.min(MAX_BASELINE_QUERIES);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(1_000);
    budget.total_timeout_ms = budget.total_timeout_ms.min(5_000);
    budget
}
struct ActivityReader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
    profile: Option<culprit_read::SourceProfileGuard>,
}
#[cfg(test)]
pub(crate) fn fixture_source(
    io: crate::progress::test_support::Reader,
    request: &CollectionRequest,
) -> Box<dyn TelemetrySource> {
    Box::new(
        FrozenSource::new(
            ActivityReader { io, host: "fixture.invalid".into(), port: 9, verified: None, profile: None },
            request,
        )
        .unwrap(),
    )
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.collection() != TelemetryCollection::Activity {
        return Err(native::invalid());
    }
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    let reader = ActivityReader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        verified: None,
        profile: None,
    };
    Ok(Box::new(FrozenSource::new(reader, &request)?))
}
pub(crate) fn profile_source(
    connector: Connector,
    request: &CollectionRequest,
    admission: &SourceProfileAdmission,
) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.selection != CollectionSelection::Activity || bounded_budget(&request.budget) != request.budget {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let profile = culprit_read::SourceProfileGuard::new(request, admission)?;
    let reader = ActivityReader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()).with_profile(profile.clone()),
        verified: None,
        profile: Some(profile),
    };
    Ok(Box::new(FrozenSource::new(reader, request)?))
}
#[async_trait::async_trait]
impl<R: crate::read::NativeRead> ObservationReader for ActivityReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let capture = match &self.profile {
            Some(profile) => capture_with_profile(&mut self.io, &self.host, self.port, request, Some(profile)).await?,
            None => capture(&mut self.io, &self.host, self.port, request).await?,
        };
        self.verified = Some(capture.description.clone());
        Ok(capture)
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        // Required source checks already bracket the frozen native observation. Paging performs no more SQL.
        if let Some(profile) = &self.profile {
            profile.verify_final(&NativeReadContext::from_activity(description)?, self.io.connection_generation())?;
        }
        Ok(self.verified.as_ref() == Some(description))
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

async fn capture<R: crate::read::QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &CollectionRequest,
) -> Result<FrozenObservation> {
    capture_with_profile(reader, host, port, request, None).await
}
async fn capture_with_profile<R: crate::read::QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &CollectionRequest,
    profile: Option<&culprit_read::SourceProfileGuard>,
) -> Result<FrozenObservation> {
    if let Some(profile) = profile {
        profile.ensure_active()?;
    }
    let before = metadata::activity(reader, host, port).await?;
    if before.version < 120_000 {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let owner = Context::observe(reader, &before).await?;
    if let Some(profile) = profile {
        let owner = owner.as_ref().ok_or_else(|| CollectionStop::SourceChanged.error())?;
        profile.bind_native(&owner.read_context()?, reader.connection_generation())?;
    }
    let read = activity::observe_baseline_rows(
        reader,
        &activity::query(before.version, request.budget.max_snapshot_rows as usize + 1),
        &before.database_oid,
        &request.budget,
    )
    .await?;
    if read.frame.records.iter().any(|row| {
        row.native_text("backend_type").as_deref() != Some("client backend")
            || row.native_text("database_oid").as_deref() != Some(before.database_oid.as_str())
    }) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let completion = local_context::complete(reader, host, port, &before, owner.as_ref()).await?;
    let visible = completion.full_visibility && read.frame.fully_visible_for(&before.database_oid);
    let context = completion.context;
    if let Some(profile) = profile {
        profile.verify_final(&context, reader.connection_generation())?;
    }
    let qualified = context.has_qualified_local_owner();
    let mut observations = read.frame.observations();
    for row in &mut observations {
        row.blocking_status = ActivityBlockingStatus::Unknown;
    }
    let source = "pg_stat_activity";
    let mut coverage =
        StateSourceCoverage::observed(source.into(), observations.len() as u32, visible && qualified && !read.limited);
    if !completion.full_visibility {
        coverage.error_code = Some(ErrorCode::PermissionDenied);
    }
    if read.limited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    } else if !visible || !qualified {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let description = CollectionDescription {
        partition: context.partition,
        target: context.target,
        source: source.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: context.database_writable,
            epoch: Some(before.activity_epoch()),
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            at: read.frame.at,
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: read.native_rows,
            observations: observations.len() as u32,
            coverage,
        }),
    };
    if !description.matches_request(request) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(FrozenObservation { description, records: observations.into_iter().map(TelemetryRecord::from).collect() })
}
