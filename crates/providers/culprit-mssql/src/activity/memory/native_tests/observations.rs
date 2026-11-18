//! One complete existing source owner per point. A failed point is recorded and is never retried.
use super::{SOURCE_QUERY_LIMIT, SOURCE_ROW_LIMIT, options};
use crate::{
    activity::{ActivityReader, IdentityDetail},
    conn::Pool,
    read::invalid_data,
    test_support::{owned_case::until_cancelled, save_cost_receipt},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{MemoryParticipantsReport, Result, SourceOutcome, source_id, telemetry::*};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, path::Path, sync::Arc};

pub(super) struct MetadataProbe {
    pub observation: String,
    pub selectors: String,
}

pub(super) struct NativePoint {
    pub native_edition: i64,
    pub native_major: u32,
    pub context: NativeReadContext,
    pub observations: Vec<ExecutionGrantObservation>,
    pub report: MemoryParticipantsReport,
    pub source: StateSourceCoverage,
    pub queries: u32,
    pub metadata: MetadataProbe,
}

impl NativePoint {
    /// Initial fixture lookup uses the held actor's exact login generation. Subsequent acceptance compares the
    /// original full request and grant identity; no historical actor is recovered by matching its session number.
    pub fn actor(&self, session: &NativeActivityIdentity) -> Result<Option<&ExecutionGrantObservation>> {
        let mut rows = self
            .observations
            .iter()
            .filter(|row| row.request.as_ref().is_some_and(|request| &request.key.session == session));
        let Some(row) = rows.next() else { return Ok(None) };
        if rows.next().is_some() {
            return Err(invalid_data());
        }
        let key = row.key()?;
        // This is the shared interpreter's current-generation qualification, including every embedded MARS peer.
        Ok(self
            .report
            .entries
            .iter()
            .any(|entry| entry.observation_key == key && entry.grant_admission.is_some())
            .then_some(row))
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "native_engine_edition": self.native_edition,
            "native_major": self.native_major,
            "context": self.context,
            "native_source": self.source,
            "observations": self.observations,
            "explanation": self.report,
            "source_queries": self.queries,
        })
    }
}

pub(super) async fn capture(
    pool: Arc<Pool>,
    port: u16,
    cancellation: &CollectionCancellation,
    directory: &Path,
    ordinal: usize,
) -> Result<NativePoint> {
    let mut reader = ActivityReader::open(pool, options(), "127.0.0.1".into(), port)?;
    let outcome = AssertUnwindSafe(until_cancelled(cancellation, async {
        let mut frame = reader.observe_memory().await?;
        let layout = super::super::fields::observed_layout(
            &frame.grants.first().map(|row| row.available_columns.clone()).unwrap_or_default(),
        );
        let targets = frame
            .activity
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.current_locator.is_some())
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let metadata = MetadataProbe {
            observation: super::super::sql::observation(
                &layout,
                IdentityDetail::Endpoint,
                frame.context.request_page_server_read_capability,
            ),
            selectors: super::super::super::selectors(&frame.activity, &targets, false)?,
        };
        reader.enrich_memory(&mut frame).await?;
        let context = reader.verify_memory_context(&frame.owner).await?;
        let complete_scope = frame.complete_scope;
        let edition = frame.context.edition;
        let major = frame.context.major;
        let observations = frame.into_observations()?;
        Ok((context, complete_scope, observations, metadata, edition, major))
    }))
    .catch_unwind()
    .await;
    let queries = reader.query_count();
    // Keep and await this same native owner even when the borrowed read was cancelled or panicked.
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics();
    let summary = metrics.as_ref().map(|metrics| metrics.summary(None)).transpose();
    let state = match &outcome {
        Ok(Ok(_)) => "completed",
        Ok(Err(_)) => "failed",
        Err(_) => "panicked",
    };
    let receipt = save_cost_receipt(
        directory,
        &format!("source-point-{ordinal:02}"),
        &serde_json::json!({
            "status": state,
            "source_queries": queries,
            "source_query_limit": SOURCE_QUERY_LIMIT,
            "shutdown": crate::read::measurement::shutdown_state(shutdown),
            "source_read_work": summary.as_ref().ok(),
            "metrics_error": summary.as_ref().err(),
            "error": outcome.as_ref().ok().and_then(|result| result.as_ref().err()),
            "cost_approval": false,
        }),
    );
    if shutdown != SourceShutdown::Quiescent || metrics.as_ref().is_some_and(ReadWorkMetrics::has_unconfirmed_work) {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let (context, complete_scope, observations, metadata, native_edition, native_major) = match outcome {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    // Native failure/uncertain settlement takes precedence over private receipt/projection failures.
    receipt?;
    let _ = summary?;
    let metrics = metrics.ok_or_else(invalid_data)?;
    if !metrics.is_valid() || metrics.templates.iter().any(|row| row.timeouts > 0 || row.unconfirmed_cancellations > 0)
    {
        return Err(invalid_data());
    }
    if queries > SOURCE_QUERY_LIMIT
        || !context.has_qualified_local_owner()
        || observations.len() > SOURCE_ROW_LIMIT as usize
    {
        return Err(invalid_data());
    }
    let source = StateSourceCoverage::observed(
        source_id::CURRENT_REQUEST_MEMORY.into(),
        u32::try_from(observations.len()).map_err(|_| invalid_data())?,
        complete_scope,
    );
    let outcome =
        SourceOutcome { source: source.source.clone(), state: source.state, reason: None, reason_message: None };
    let rows = observations.iter().collect::<Vec<_>>();
    let report = culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &rows },
        Some(&outcome),
    )?;
    let point = NativePoint { native_edition, native_major, context, observations, report, source, queries, metadata };
    save_cost_receipt(directory, &format!("native-point-{ordinal:02}"), &point.snapshot())?;
    Ok(point)
}
