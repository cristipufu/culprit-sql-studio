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
    tempdb: Option<bool>,
    log_bytes: Option<bool>,
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
        Self { io, tempdb: None, log_bytes: None }
    }
    pub(crate) async fn read_rows(&mut self, sql: &str) -> Result<Vec<Rec>> {
        self.read(sql, vec![], MAX_RESULT_ROWS).await
    }
    pub(crate) async fn read(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> Result<Vec<Rec>> {
        let rows = self.io.query(sql, parameters, limit + 1).await.map_err(ReadError::into_error)?;
        if rows.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(rows)
    }
    pub(crate) async fn summary(&mut self, window_minutes: u32) -> Result<Vec<PlanSummary>> {
        let tempdb = match self.tempdb {
            Some(value) => value,
            None => {
                let rows = self.read(runtime::LAYOUT, vec![], 1).await?;
                let value = match integer(rows.first().ok_or_else(invalid_data)?, "tempdb")? {
                    0 => false,
                    1 => true,
                    _ => return Err(invalid_data()),
                };
                self.tempdb = Some(value);
                value
            }
        };
        self.read(&runtime::summary_sql(tempdb), vec![Parameter::Integer(i64::from(window_minutes))], MAX_RESULT_ROWS)
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
        let mut parameters = vec![Parameter::Integer(i64::from(window_minutes))];
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
        self.read(
            &super::costs::runs_sql(log, true),
            vec![Parameter::Integer(i64::from(window_minutes)), ids_parameter(ids)?],
            MAX_RESULT_ROWS,
        )
        .await?
        .iter()
        .map(super::costs::RunCosts::decode)
        .collect()
    }
    pub(crate) async fn total_log_bytes(&mut self, window_minutes: u32) -> Result<Option<f64>> {
        if !self.has_log_bytes().await? {
            return Ok(None);
        }
        let rows = self
            .read(&super::costs::runs_sql(true, false), vec![Parameter::Integer(i64::from(window_minutes))], 1)
            .await?;
        crate::read::optional_nonnegative_number(rows.first().ok_or_else(invalid_data)?, "log_bytes")
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
        self.io.query(sql, parameters, row_limit).await
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
