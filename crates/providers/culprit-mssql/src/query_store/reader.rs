//! Bounded native operations. The caller owns this reader through completion and shutdown.
use super::runtime::{self, PlanSummary};
use crate::{
    conn::Pool,
    read::{NativeRead, Parameter, ReadError, ReadSession, integer, invalid_data},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{CollectionBudget, CollectionStop, SourceShutdown},
};
use std::sync::Arc;
const MAX_RESULT_ROWS: usize = 10_000;
const SNAPSHOT_BYTES: u32 = 8 * 1024 * 1024;
const SNAPSHOT_TIMEOUT_MS: u32 = 120_000;

pub(crate) struct Reader<R = ReadSession> {
    io: R,
    layout: Option<RuntimeLayout>,
    window: Option<super::window_sql::FrozenWindow>,
    log_bytes: Option<bool>,
    stopped: Option<culprit_core::Error>,
}
#[derive(Clone, Copy)]
struct RuntimeLayout {
    tempdb: bool,
    waits: bool,
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
        Self { io, layout: None, window: None, log_bytes: None, stopped: None }
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
        let rows = self.read(runtime::LAYOUT, vec![], 1).await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let tempdb = match integer(row, "tempdb")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        let layout = RuntimeLayout { tempdb, waits: super::window_sql::waits_supported(row)? };
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
        self.layout = Some(layout);
        self.window = Some(window.clone());
        Ok(window)
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
        result
    }
    pub(crate) async fn summary(&mut self, window_minutes: u32) -> Result<Vec<PlanSummary>> {
        let window = self.window(window_minutes).await?;
        let layout = self.layout.ok_or_else(invalid_data)?;
        self.read(&runtime::summary_sql(layout.tempdb), vec![window.parameter()], MAX_RESULT_ROWS)
            .await?
            .iter()
            .map(PlanSummary::decode)
            .collect()
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
        self.read(&super::waits::summary_sql(plan_ids.is_some()), parameters, MAX_RESULT_ROWS)
            .await?
            .iter()
            .map(|row| super::waits::Summary::decode(row, plan_ids.is_some()))
            .collect()
    }
    pub(crate) async fn definitions(
        &mut self,
        ids: &[String],
        kind: super::definitions::Kind,
        limit: usize,
    ) -> Result<Vec<super::definitions::Definition>> {
        super::definitions::read(self, ids, kind, limit).await
    }
    pub(crate) async fn costs(&mut self, window_minutes: u32, ids: &[String]) -> Result<Vec<super::costs::RunCosts>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let log = self.has_log_bytes().await?;
        let scope = self.window(window_minutes).await?.parameter();
        self.read(&super::costs::runs_sql(log), vec![scope, ids_parameter(ids)?], MAX_RESULT_ROWS)
            .await?
            .iter()
            .map(super::costs::RunCosts::decode)
            .collect()
    }
    async fn has_log_bytes(&mut self) -> Result<bool> {
        if let Some(value) = self.log_bytes {
            return Ok(value);
        }
        let rows = self.read(super::costs::LAYOUT, vec![], 1).await?;
        let value = match integer(rows.first().ok_or_else(invalid_data)?, "log_bytes")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        self.log_bytes = Some(value);
        Ok(value)
    }
    pub(crate) async fn compilations(&mut self, ids: &[String]) -> Result<Vec<super::costs::Compilation>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.read(&super::costs::compilation_sql(), vec![ids_parameter(ids)?], MAX_RESULT_ROWS)
            .await?
            .iter()
            .map(super::costs::Compilation::decode)
            .collect()
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
mod tests;
