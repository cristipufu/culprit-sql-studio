//! One native activity reader. Consumers choose scope, enrichment and presentation after reading the same facts.
pub(crate) mod memory;
mod resources;
mod rows;
mod sql;

use crate::{
    conn::Pool,
    read::{NativeRead, Parameter, ReadError, ReadSession, integer, invalid_data, stops_remaining_reads, text},
    value::Rec,
};
use culprit_core::{DatabaseIdentity, Engine, Error, ErrorCode, Result, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const DETAIL_METADATA_ALLOWANCE: u32 = 4096;
#[derive(Clone, Copy)]
pub(crate) enum ActivityScope {
    Visible,
    CurrentDatabase,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityDetail {
    Endpoint,
    AzureDatabase,
}
pub(crate) struct ReadOptions {
    pub budget: CollectionBudget,
    pub scope: ActivityScope,
    pub identity: IdentityDetail,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ActivityContext {
    pub database: DatabaseIdentity,
    pub epoch: Option<String>,
    pub at: String,
    pub writable: Option<bool>,
}
impl ActivityContext {
    fn decode(row: &Rec, host: &str, port: u16, identity: IdentityDetail) -> Result<Self> {
        let edition = integer(row, "edition")?;
        if identity == IdentityDetail::AzureDatabase && edition != 5 {
            return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.azure_sql_only")));
        }
        if !crate::read::boolean(row, "database_wide")? {
            let grant = crate::probe::monitoring_grant(
                edition,
                integer(row, "major")?.try_into().map_err(|_| invalid_data())?,
                &text(row, "reader_login")?,
                &text(row, "reader_user")?,
            );
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                culprit_core::t!("provider.mssql.activity.visibility", grant = grant),
            )
            .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceUnavailable)));
        }
        let mut database = DatabaseIdentity::server(Engine::Mssql, &text(row, "database_name")?, host, port, None);
        let epoch = if identity == IdentityDetail::AzureDatabase {
            database.incarnation = Some(text(row, "incarnation")?);
            Some(digest(
                format!(
                    "{}|{}|{}|activity-v6",
                    text(row, "physical_incarnation")?,
                    text(row, "server_version")?,
                    integer(row, "compatibility_level")?
                )
                .as_bytes(),
            ))
        } else {
            None
        };
        Ok(Self {
            database,
            epoch,
            at: canonical_time(&text(row, "observed_at")?)?,
            writable: crate::read::database_writable(row)?,
        })
    }
}

/// Intentionally not serializable: raw SQL is projected to Live or redacted by the export boundary.
#[derive(Clone)]
pub(crate) enum NativeStatement {
    Available(String),
    Unavailable(PayloadUnavailable),
}
impl NativeStatement {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Available(text) => Some(text),
            Self::Unavailable(_) => None,
        }
    }
}
#[derive(Clone)]
struct RequestLocator {
    handle: String,
    start: i32,
    end: i32,
    database: i32,
}
#[derive(Clone)]
struct PreviousBatchLocator {
    start: String,
    end: String,
}
#[derive(Clone)]
pub(crate) struct ActivityRecord {
    pub facts: ActivityObservation,
    pub current_sql: NativeStatement,
    pub previous_sql: NativeStatement,
    current_locator: Option<RequestLocator>,
    previous_locator: Option<PreviousBatchLocator>,
}
impl ActivityRecord {
    pub(crate) fn into_redacted_observation(self) -> ActivityObservation {
        let mut observation = self.facts;
        observation.statement = match self.current_sql {
            NativeStatement::Available(text) => DefinitionPayload::sql(&text, Engine::Mssql),
            NativeStatement::Unavailable(reason) => DefinitionPayload::Unavailable { reason },
        };
        observation
    }
}
pub(crate) struct ActivityFrame {
    pub context: ActivityContext,
    pub records: Vec<ActivityRecord>,
}
impl ActivityFrame {
    pub(crate) fn observations(&self) -> Vec<ActivityObservation> {
        self.records.iter().map(|record| record.facts.clone()).collect()
    }
    pub(crate) fn keys(&self) -> BTreeSet<ActivityKey> {
        self.records.iter().map(|record| record.facts.key.clone()).collect()
    }
}

pub(crate) struct ActivityReader<R = ReadSession> {
    io: R,
    options: ReadOptions,
    host: String,
    port: u16,
    observed: bool,
}
impl ActivityReader {
    pub(crate) fn open(pool: Arc<Pool>, options: ReadOptions, host: String, port: u16) -> Result<Self> {
        options.budget.validate()?;
        let io = ReadSession::new(pool, options.budget.clone());
        Ok(Self::new(io, options, host, port))
    }
}
impl<R: NativeRead> ActivityReader<R> {
    fn new(io: R, options: ReadOptions, host: String, port: u16) -> Self {
        Self { io, options, host, port, observed: false }
    }
    pub(crate) fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.io.read_metrics()
    }
    pub(crate) fn query_count(&self) -> u32 {
        self.io.queries()
    }
    pub(crate) fn sql_limit(&self) -> u32 {
        self.options.budget.max_sql_bytes
    }
    pub(crate) async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
    pub(crate) async fn observe(&mut self) -> Result<ActivityFrame> {
        if self.observed {
            return Err(invalid_data());
        }
        self.observed = true;
        let limit = self.options.budget.max_snapshot_rows;
        let sql = sql::metadata(self.options.identity);
        let scope = match self.options.scope {
            ActivityScope::Visible => 0,
            ActivityScope::CurrentDatabase => 1,
        };
        let rows = self
            .io
            .query(&sql, vec![Parameter::Integer(i64::from(limit) + 1), Parameter::Integer(scope)], limit as usize + 1)
            .await
            .map_err(ReadError::into_error)?;
        let context = ActivityContext::decode(
            rows.first().ok_or_else(invalid_data)?,
            &self.host,
            self.port,
            self.options.identity,
        )?;
        let mut records = Vec::new();
        for row in &rows {
            if context != ActivityContext::decode(row, &self.host, self.port, self.options.identity)? {
                return Err(CollectionStop::SourceChanged.error());
            }
            if let Some(record) = rows::record(row)? {
                records.push(record);
            }
        }
        if records.len() > limit as usize {
            return Err(CollectionStop::RowLimit.error());
        }
        let frame = ActivityFrame { context, records };
        ActivityEvidence::validate(&frame.observations())?;
        Ok(frame)
    }
    /// The selector contains keys from this frozen frame. Earlier batches are an explicit, separate enrichment.
    pub(crate) async fn enrich(
        &mut self,
        frame: &mut ActivityFrame,
        selected: &BTreeSet<ActivityKey>,
        previous_batches: bool,
        max_sql_bytes: u32,
    ) -> Result<ActivityDetailStatus> {
        self.enrich_using_policy(frame, selected, previous_batches, max_sql_bytes, sql::QueryPolicy::Activity).await
    }
    async fn enrich_using_policy(
        &mut self,
        frame: &mut ActivityFrame,
        selected: &BTreeSet<ActivityKey>,
        previous_batches: bool,
        max_sql_bytes: u32,
        policy: sql::QueryPolicy,
    ) -> Result<ActivityDetailStatus> {
        let mut status = ActivityDetailStatus::NotNeeded;
        for previous in [false, true] {
            if previous && !previous_batches {
                continue;
            }
            let targets: Vec<_> = frame
                .records
                .iter()
                .enumerate()
                .filter(|(_, record)| selected.contains(&record.facts.key))
                .filter(
                    |(_, record)| {
                        if previous { record.previous_locator.is_some() } else { record.current_locator.is_some() }
                    },
                )
                .map(|(index, _)| index)
                .collect();
            if targets.is_empty() {
                continue;
            }
            let selectors = selectors(frame, &targets, previous)?;
            let cap =
                (self.options.budget.page_bytes / (targets.len() as u32)).saturating_sub(DETAIL_METADATA_ALLOWANCE) / 3;
            let cap = cap.min(max_sql_bytes).min(self.options.budget.max_sql_bytes);
            let sql = sql::details(previous, policy);
            let result = self
                .io
                .query(&sql, vec![Parameter::Text(selectors), Parameter::Integer(i64::from(cap))], targets.len())
                .await;
            let detail_rows = match result {
                Ok(rows) => rows,
                Err(ReadError::Stopped(reason)) => return Err(reason.error()),
                Err(ReadError::Source(error)) if stops_remaining_reads(&error) => return Err(error),
                Err(ReadError::Source(error)) => {
                    mark_unavailable(frame, &targets, previous);
                    return Ok(ActivityDetailStatus::Unavailable { code: error.code });
                }
            };
            match decode_details(frame, &targets, &detail_rows, cap, previous) {
                Ok(prepared) => {
                    for (index, statement) in prepared {
                        let record = &mut frame.records[index];
                        if previous { record.previous_sql = statement } else { record.current_sql = statement }
                    }
                    status = ActivityDetailStatus::Collected;
                }
                Err(_) => {
                    mark_unavailable(frame, &targets, previous);
                    return Ok(ActivityDetailStatus::Unavailable { code: ErrorCode::QueryFailed });
                }
            }
        }
        Ok(status)
    }
}
fn mark_unavailable(frame: &mut ActivityFrame, targets: &[usize], previous: bool) {
    for &index in targets {
        let statement = NativeStatement::Unavailable(PayloadUnavailable::SourceUnavailable);
        if previous {
            frame.records[index].previous_sql = statement
        } else {
            frame.records[index].current_sql = statement
        }
    }
}
fn decode_details(
    frame: &ActivityFrame,
    targets: &[usize],
    rows: &[Rec],
    cap: u32,
    previous: bool,
) -> Result<Vec<(usize, NativeStatement)>> {
    if rows.len() != targets.len() {
        return Err(invalid_data());
    }
    let mut by_id: BTreeMap<_, _> = targets
        .iter()
        .map(|&index| {
            let key = &frame.records[index].facts.key;
            ((key.session.id.clone(), key.request.as_ref().map(|request| request.id.clone())), index)
        })
        .collect();
    if by_id.len() != targets.len() {
        return Err(invalid_data());
    }
    rows.iter()
        .map(|row| {
            let index = by_id.remove(&rows::detail_key(row, previous)?).ok_or_else(invalid_data)?;
            Ok((index, rows::statement(row, &frame.records[index], cap, previous)?))
        })
        .collect()
}
fn selectors(frame: &ActivityFrame, targets: &[usize], previous: bool) -> Result<String> {
    let mut xml = String::from("<activity>");
    for &index in targets {
        let record = &frame.records[index];
        let key = &record.facts.key;
        let session = key.session.id.parse::<i32>().map_err(|_| invalid_data())?;
        let login = crate::sql_time::native_time(&key.session.generation)?;
        if previous {
            let locator = record.previous_locator.as_ref().ok_or_else(invalid_data)?;
            xml.push_str(&format!(
                "<row session=\"{session}\" login=\"{login}\" last_start=\"{}\" last_end=\"{}\"/>",
                locator.start, locator.end
            ));
        } else {
            let request = key.request.as_ref().ok_or_else(invalid_data)?;
            let request_id = request.id.parse::<i32>().map_err(|_| invalid_data())?;
            let started = crate::sql_time::native_time(&request.generation)?;
            let locator = record.current_locator.as_ref().ok_or_else(invalid_data)?;
            xml.push_str(&format!("<row session=\"{session}\" login=\"{login}\" request=\"{request_id}\" started=\"{started}\" handle=\"{}\" begin=\"{}\" end=\"{}\" database=\"{}\"/>",locator.handle,locator.start,locator.end,locator.database));
        }
    }
    xml.push_str("</activity>");
    Ok(xml)
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod native_tests;
