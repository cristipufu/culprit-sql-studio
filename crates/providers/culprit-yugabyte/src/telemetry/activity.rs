//! A node-qualified YSQL metadata baseline. DocDB lock RPCs and SQL text are separate investigations.
use super::*;
use culprit_core::MeasurementScope;
use culprit_postgres::wire::{NativeRead, activity as sql_activity};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
const MAX_BASELINE_ROWS: u32 = 500;
const MAX_BASELINE_QUERIES: u32 = 8; // Three metadata reads at either bound, one local frame and owned setup.
fn bounded_request(request: &CollectionRequest) -> CollectionRequest {
    let mut request = request.clone();
    request.budget.max_snapshot_rows = request.budget.max_snapshot_rows.min(MAX_BASELINE_ROWS);
    request.budget.page_rows = request.budget.page_rows.min(MAX_BASELINE_ROWS);
    request.budget.max_queries = request.budget.max_queries.min(MAX_BASELINE_QUERIES);
    request.budget.statement_timeout_ms = request.budget.statement_timeout_ms.min(1_000);
    request.budget.total_timeout_ms = request.budget.total_timeout_ms.min(5_000);
    request
}
struct ActivityReader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.collection() != TelemetryCollection::Activity {
        return Err(native::invalid());
    }
    let request = bounded_request(request);
    Ok(Box::new(FrozenSource::new(
        ActivityReader {
            host: connector.host.clone(),
            port: connector.port,
            io: ReadSession::for_engine(connector, request.budget.clone(), Engine::Yugabyte),
            verified: None,
        },
        &request,
    )?))
}
#[async_trait::async_trait]
impl<R: NativeRead + culprit_postgres::wire::NoticeRead> ObservationReader for ActivityReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let capture = capture(&mut self.io, &self.host, self.port, request).await?;
        self.verified = Some(capture.description.clone());
        Ok(capture)
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
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

async fn capture<R: culprit_postgres::wire::NoticeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &CollectionRequest,
) -> Result<FrozenObservation> {
    let before = metadata::read(reader, host, port).await?;
    let notices = reader.notices();
    let sql = sql_activity::query_layout(
        false,
        "'client backend','yb-conn-mgr worker connection'",
        ",a.yb_backend_xid::text AS transaction_id",
        request.budget.max_snapshot_rows as usize + 1,
    );
    let read =
        sql_activity::observe_baseline_rows(reader, &sql, &before.probe.oid.to_string(), &request.budget).await?;
    let after = metadata::read(reader, host, port).await?;
    if !before.same_activity_source(&after) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let visible = before.probe.statement_role.is_none()
        && after.probe.statement_role.is_none()
        && read.frame.fully_visible_for(&before.probe.oid.to_string());
    let complete = visible && !read.limited && reader.notices() == notices;
    let source = format!("ysql-activity-{}", before.node);
    let mut observations = read.frame.observations();
    for row in &mut observations {
        row.key.session.id = format!("{}/{}", before.node, row.key.session.id);
        row.blocking_status = ActivityBlockingStatus::Unknown;
        if let Some(identity) = &mut row.statement_identity {
            identity.qualifiers.insert("node".into(), before.node.clone());
        }
    }
    ActivityEvidence::validate(&observations)?;
    let mut coverage = StateSourceCoverage::observed(source.clone(), observations.len() as u32, complete);
    if before.probe.statement_role.is_some() || after.probe.statement_role.is_some() {
        coverage.error_code = Some(ErrorCode::PermissionDenied);
    }
    if read.limited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    } else if !complete {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let description = CollectionDescription {
        target: before.database.clone().into(),
        source,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(digest(
                format!(
                    "{}|{}|{}|ysql-activity-v1",
                    before.node,
                    before.probe.oid,
                    before.probe.started.as_deref().ok_or_else(native::invalid)?
                )
                .as_bytes(),
            )),
            observed_from: canonical_time(&before.probe.now)?,
            observed_until: canonical_time(&after.probe.now)?,
            at: read.frame.at,
            scope: MeasurementScope::Node,
            population: ActivityPopulation::NodeClientBackendsAndConnectionWorkers,
            native_rows: read.native_rows,
            observations: observations.len() as u32,
            coverage,
        }),
    };
    if !description.observation.activity().is_some_and(ActivitySnapshotDescription::is_valid) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(FrozenObservation { description, records: observations.into_iter().map(TelemetryRecord::from).collect() })
}
