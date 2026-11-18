//! Bounded owned-fixture setup only. Source readers never configure Query Store or execute these probes.
use super::owned_case::until_cancelled;
use super::{OwnedSourceFixture, save_cost_receipt};
use crate::{
    conn::Pool,
    query_store::decode,
    read::measurement::shutdown_state,
    read::{NativeRead, Parameter, ReadError, ReadResult, ReadSession, invalid_data},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::*};
use futures_util::FutureExt;
use serde_json::json;
use std::{collections::BTreeMap, panic::AssertUnwindSafe, path::Path, sync::Arc};
use tokio::time::Instant;

pub(crate) const MAX_PROBES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProbePolicy {
    Collector,
    OwnedApplication,
}

/// Only this module constructs the fixed, bounded probe after nonce-owned fixture admission.
/// It carries no transferable permission for an arbitrary statement or a different pool.
pub(crate) struct OwnedApplicationProbe {
    pool: Arc<Pool>,
    sql: String,
}
impl OwnedApplicationProbe {
    pub(crate) fn authorizes_pool(&self, pool: &Arc<Pool>) -> bool {
        Arc::ptr_eq(&self.pool, pool)
    }
    pub(crate) fn sql(&self) -> &str {
        &self.sql
    }
}

#[async_trait::async_trait]
trait ProbeRead: NativeRead {
    async fn application_probe(&mut self, probe: &OwnedApplicationProbe) -> ReadResult<Vec<crate::value::Rec>>;
}
#[async_trait::async_trait]
impl ProbeRead for ReadSession {
    async fn application_probe(&mut self, probe: &OwnedApplicationProbe) -> ReadResult<Vec<crate::value::Rec>> {
        self.owned_application_probe(probe).await
    }
}
const OWNED_TABLE_SETUP_SQL: &str = r#"CREATE TABLE /* culprit */ dbo.culprit_owned_compile_probe(id bigint NOT NULL);
INSERT /* culprit */ dbo.culprit_owned_compile_probe(id) VALUES(1);
SELECT /* culprit */ COUNT_BIG(*) AS fixture_rows,MIN(id) AS fixture_value
FROM dbo.culprit_owned_compile_probe OPTION(MAXDOP 1)"#;
const EMPTY_LOOKUP_INVENTORY_SQL: &str = r#"SELECT /* culprit */ COUNT_BIG(*) AS query_rows,
 COUNT_BIG(CASE WHEN CHARINDEX(@P1,t.query_sql_text)>0 THEN 1 END) AS marker_query_rows,
 COUNT_BIG(CASE WHEN CHARINDEX(@P2,t.query_sql_text)>0 THEN 1 END) AS alias_query_rows
FROM sys.query_store_query q
LEFT JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
OPTION(MAXDOP 1)"#;
pub(crate) async fn seed(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
    count: usize,
    policy: ProbePolicy,
    receipt_name: &str,
) -> Result<BTreeMap<String, NativeQueryIdentity>> {
    validate_count(count)?;
    let mut owner = ReadSession::new(
        fixture.pool.clone(),
        CollectionBudget {
            max_queries: (count + 6) as u32,
            statement_timeout_ms: 1500,
            total_timeout_ms: 20_000,
            ..Default::default()
        },
    );
    let started = Instant::now();
    let mut progress = SetupProgress::default();
    let result = AssertUnwindSafe(until_cancelled(
        cancellation,
        seed_and_resolve(&mut owner, &mut progress, count, policy, Some(&fixture.pool)),
    ))
    .catch_unwind()
    .await;
    let shutdown = owner.close().await;
    let written = save_cost_receipt(
        directory,
        receipt_name,
        &json!({
            "queries":owner.queries(),"read_metrics":owner.read_metrics(),"client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "error":result.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "error_detail":result.as_ref().ok().and_then(|result|result.as_ref().err()).and_then(|error|error.detail),
            "panicked":result.is_err(),"shutdown":shutdown_state(shutdown),"probe_policy":policy,
            "setup_progress":progress,
            "expected_probe_count":count,"retained_probe_count":result.as_ref().ok().and_then(|result|result.as_ref().ok()).map(|rows|rows.len()),
            "outside_source_measurement":true,"production_admission":"unqualified",
            "setup":"one tagged owned-table CREATE/INSERT/verification batch, the requested distinct one-row probes, flush and exact native identity/provenance lookup; only explicit owned-application probes omit monitoring tags; supervisor owns table teardown",
        }),
    );
    let result = match result {
        Ok(result) => result,
        Err(panic) => {
            if let Err(error) = written {
                eprintln!(
                    "owned Query Store setup receipt unavailable: code={:?},detail={:?}",
                    error.code, error.detail
                );
            }
            std::panic::resume_unwind(panic)
        }
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    match result {
        Ok(identities) => {
            written?;
            Ok(identities)
        }
        Err(original) => {
            if let Err(error) = written {
                eprintln!(
                    "owned Query Store setup receipt unavailable: code={:?},detail={:?}",
                    error.code, error.detail
                );
            }
            Err(original)
        }
    }
}
fn validate_count(count: usize) -> Result<()> {
    if (1..=MAX_PROBES).contains(&count) { Ok(()) } else { Err(invalid_data()) }
}
async fn seed_and_resolve<R: ProbeRead>(
    owner: &mut R,
    progress: &mut SetupProgress,
    count: usize,
    policy: ProbePolicy,
    application_pool: Option<&Arc<Pool>>,
) -> Result<BTreeMap<String, NativeQueryIdentity>> {
    validate_count(count)?;
    if policy == ProbePolicy::OwnedApplication && application_pool.is_none() {
        return Err(invalid_data());
    }
    progress.stage = SetupStage::Options;
    let options = owner
        .query(
            "SELECT /* culprit */ actual_state_desc,query_capture_mode_desc FROM sys.database_query_store_options",
            vec![],
            1,
        )
        .await
        .map_err(ReadError::into_error)?;
    progress.options_rows = Some(options.len());
    let [options] = options.as_slice() else { return Err(progress.reject(SetupFailure::OptionsRows)) };
    progress.read_write = options.str("actual_state_desc").map(|state| state == "READ_WRITE");
    progress.capture_all = options.str("query_capture_mode_desc").map(|mode| mode == "ALL");
    if progress.read_write != Some(true) || progress.capture_all != Some(true) {
        return Err(progress.reject(SetupFailure::OptionsNotEligible));
    }
    progress.stage = SetupStage::OwnedTable;
    let table = owner.query(OWNED_TABLE_SETUP_SQL, vec![], 1).await.map_err(ReadError::into_error)?;
    let [table] = table.as_slice() else { return Err(progress.reject(SetupFailure::OwnedTableRows)) };
    let table_rows = progress.fixture_integer(table, "fixture_rows", SetupFailure::OwnedTableRows)?;
    progress.owned_table_rows = Some(table_rows.to_string());
    if table_rows != 1 {
        return Err(progress.reject(SetupFailure::OwnedTableRows));
    }
    let value = progress.fixture_integer(table, "fixture_value", SetupFailure::OwnedTableValue)?;
    progress.owned_table_value = Some(value.to_string());
    if value != 1 {
        return Err(progress.reject(SetupFailure::OwnedTableValue));
    }
    let marker = format!("/* owned compilation stage probe {} */", std::process::id());
    progress.stage = SetupStage::Probes;
    for index in 0..count {
        let sql = probe_sql(&marker, index, policy);
        let rows = match policy {
            ProbePolicy::Collector => owner.query(&sql, vec![], 1).await,
            ProbePolicy::OwnedApplication => {
                let probe = OwnedApplicationProbe { pool: application_pool.ok_or_else(invalid_data)?.clone(), sql };
                owner.application_probe(&probe).await
            }
        }
        .map_err(ReadError::into_error)?;
        progress.last_probe_rows = Some(rows.len());
        let [row] = rows.as_slice() else { return Err(progress.reject(SetupFailure::ProbeRows)) };
        if progress.fixture_integer(row, &format!("culprit_owned_compile_{index}"), SetupFailure::ProbeValue)? != 1 {
            return Err(progress.reject(SetupFailure::ProbeValue));
        }
        progress.probes_completed += 1;
    }
    progress.stage = SetupStage::Flush;
    owner.query("EXEC /* culprit */ sys.sp_query_store_flush_db", vec![], 1).await.map_err(ReadError::into_error)?;
    progress.flush_completed = true;
    progress.stage = SetupStage::Lookup;
    let sql = format!(
        "SELECT TOP({}) /* culprit */ {},CAST(q.is_internal_query AS int) AS engine_internal,CAST(q.is_clouddb_internal_query AS int) AS cloud_internal,t.query_sql_text AS fixture_text FROM sys.query_store_query q JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id WHERE CHARINDEX(@P1,t.query_sql_text)>0 OPTION(MAXDOP 1)",
        count + 1,
        crate::query_store::catalog_sql::query_identity_columns!("q.query_id")
    );
    let rows =
        owner.query(&sql, vec![Parameter::Text(marker.clone())], count + 1).await.map_err(ReadError::into_error)?;
    resolve_population(owner, &marker, &rows, progress, count, policy).await
}

fn probe_sql(marker: &str, index: usize, policy: ProbePolicy) -> String {
    match policy {
        ProbePolicy::Collector => format!(
            "SELECT /* culprit */ {marker} (SELECT /* culprit */ id FROM dbo.culprit_owned_compile_probe) AS culprit_owned_compile_{index}"
        ),
        ProbePolicy::OwnedApplication => {
            format!("SELECT {marker} (SELECT id FROM dbo.culprit_owned_compile_probe) AS culprit_owned_compile_{index}")
        }
    }
}

async fn resolve_population<R: crate::read::NativeRead>(
    owner: &mut R,
    marker: &str,
    rows: &[crate::value::Rec],
    progress: &mut SetupProgress,
    count: usize,
    policy: ProbePolicy,
) -> Result<BTreeMap<String, NativeQueryIdentity>> {
    let original = decode_population(rows, progress, count, policy);
    if rows.is_empty() && original.is_err() {
        // The lookup has drained successfully. One same-owner diagnostic compares all rows with marker/alias
        // matches; it never retries probes, supplies identities or changes the failure.
        // Other setup/observer queries may contribute to query_rows, so this is not proof of probe membership.
        progress.empty_lookup_inventory = Some(EmptyLookupInventory::default());
        let inventory = read_inventory(owner, marker).await;
        progress.empty_lookup_inventory = Some(match inventory {
            Ok(counts) => EmptyLookupInventory { complete: true, counts: Some(counts), ..Default::default() },
            Err(error) => {
                EmptyLookupInventory { error: Some(error.code), error_detail: error.detail, ..Default::default() }
            }
        });
    }
    original
}

async fn read_inventory<R: crate::read::NativeRead>(owner: &mut R, marker: &str) -> Result<QueryStoreInventoryCounts> {
    let rows = owner
        .query(
            EMPTY_LOOKUP_INVENTORY_SQL,
            vec![Parameter::Text(marker.into()), Parameter::Text("culprit_owned_compile_".into())],
            1,
        )
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let query_rows = crate::read::integer(row, "query_rows")?;
    let marker_query_rows = crate::read::integer(row, "marker_query_rows")?;
    let alias_query_rows = crate::read::integer(row, "alias_query_rows")?;
    if marker_query_rows > query_rows || alias_query_rows > query_rows {
        return Err(invalid_data());
    }
    Ok(QueryStoreInventoryCounts {
        query_rows: query_rows.to_string(),
        marker_query_rows: marker_query_rows.to_string(),
        alias_query_rows: alias_query_rows.to_string(),
    })
}

fn decode_population(
    rows: &[crate::value::Rec],
    progress: &mut SetupProgress,
    count: usize,
    policy: ProbePolicy,
) -> Result<BTreeMap<String, NativeQueryIdentity>> {
    validate_count(count)?;
    progress.lookup_rows = Some(rows.len());
    progress.stage = SetupStage::VerifyPopulation;
    let mut identities = BTreeMap::new();
    let mut retained_probes = std::collections::BTreeSet::new();
    for row in rows {
        progress.rows_examined += 1;
        // Prove that the actual retained population contains all intended distinct projections. Neither a
        // comment match, literal value nor the number of dispatched probes establishes Query Store cardinality.
        let text = match crate::read::text(row, "fixture_text") {
            Ok(text) => text,
            Err(error) => {
                progress.failure = Some(SetupFailure::MissingProjectionText);
                return Err(error);
            }
        };
        let probe = match probe_projection(&text, count) {
            Some(probe) => probe,
            None => {
                progress.rejected_projection_shape = Some(ProjectionShape::of(&text));
                return Err(progress.reject(SetupFailure::ProjectionNotRecognized));
            }
        };
        let origin = crate::statement_origin::read(row, Some(&text)).origin;
        let expected_origin = match policy {
            ProbePolicy::Collector => origin.is_culprit(),
            ProbePolicy::OwnedApplication => origin == StatementOrigin::Application,
        };
        if !expected_origin {
            progress.rejected_origin = Some(origin);
            return Err(progress.reject(SetupFailure::ProbeProvenance));
        }
        progress.matched_projections += 1;
        if !retained_probes.insert(probe) {
            return Err(progress.reject(SetupFailure::DuplicateProjection));
        }
        progress.unique_projections = retained_probes.len();
        let identity = match decode::query_identity(row) {
            Ok(identity) => identity,
            Err(error) => {
                progress.failure = Some(SetupFailure::NativeIdentity);
                progress.identity_fields = Some(IdentityFields::of(row));
                return Err(error);
            }
        };
        progress.decoded_identities += 1;
        if identities.insert(identity.query_id.clone(), identity).is_some() {
            return Err(progress.reject(SetupFailure::DuplicateIdentity));
        }
        progress.unique_identities = identities.len();
    }
    if identities.len() != count || retained_probes.len() != count {
        return Err(progress.reject(SetupFailure::PopulationCardinality));
    }
    progress.stage = SetupStage::Complete;
    Ok(identities)
}

fn probe_projection(text: &str, count: usize) -> Option<usize> {
    text.split_whitespace()
        .last()?
        .strip_prefix("culprit_owned_compile_")?
        .parse::<usize>()
        .ok()
        .filter(|index| *index < count)
}

#[derive(Default, serde::Serialize)]
struct SetupProgress {
    stage: SetupStage,
    options_rows: Option<usize>,
    read_write: Option<bool>,
    capture_all: Option<bool>,
    owned_table_rows: Option<String>,
    owned_table_value: Option<String>,
    probes_completed: usize,
    last_probe_rows: Option<usize>,
    flush_completed: bool,
    lookup_rows: Option<usize>,
    rows_examined: usize,
    matched_projections: usize,
    unique_projections: usize,
    decoded_identities: usize,
    unique_identities: usize,
    failure: Option<SetupFailure>,
    rejected_projection_shape: Option<ProjectionShape>,
    rejected_origin: Option<StatementOrigin>,
    identity_fields: Option<IdentityFields>,
    empty_lookup_inventory: Option<EmptyLookupInventory>,
}
impl SetupProgress {
    fn fixture_integer(&mut self, row: &crate::value::Rec, field: &str, failure: SetupFailure) -> Result<i64> {
        crate::read::integer(row, field).inspect_err(|_| {
            self.failure = Some(failure);
        })
    }

    fn reject(&mut self, failure: SetupFailure) -> culprit_core::Error {
        self.failure = Some(failure);
        invalid_data()
    }
}

#[derive(Default, serde::Serialize)]
struct EmptyLookupInventory {
    complete: bool,
    counts: Option<QueryStoreInventoryCounts>,
    error: Option<culprit_core::ErrorCode>,
    error_detail: Option<culprit_core::ErrorDetail>,
}

/// Same-point Query Store rows, including any unrelated setup/observer queries. These are not execution counts
/// or an inventory of distinct intended probes; only decode_population can admit the requested exact native identities.
#[derive(serde::Serialize)]
struct QueryStoreInventoryCounts {
    query_rows: String,
    marker_query_rows: String,
    alias_query_rows: String,
}

#[derive(Default, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum SetupStage {
    #[default]
    NotStarted,
    Options,
    OwnedTable,
    Probes,
    Flush,
    Lookup,
    VerifyPopulation,
    Complete,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum SetupFailure {
    OptionsRows,
    OptionsNotEligible,
    OwnedTableRows,
    OwnedTableValue,
    ProbeRows,
    ProbeValue,
    MissingProjectionText,
    ProjectionNotRecognized,
    ProbeProvenance,
    DuplicateProjection,
    NativeIdentity,
    DuplicateIdentity,
    PopulationCardinality,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct ProjectionShape {
    last_token_present: bool,
    ends_in_semicolon: bool,
    bracket_quoted: bool,
    double_quoted: bool,
    expected_prefix: bool,
}
impl ProjectionShape {
    fn of(text: &str) -> Self {
        let last = text.split_whitespace().last();
        Self {
            last_token_present: last.is_some(),
            ends_in_semicolon: text.trim_end().ends_with(';'),
            bracket_quoted: last.is_some_and(|word| word.starts_with('[')),
            double_quoted: last.is_some_and(|word| word.starts_with('"')),
            expected_prefix: last.is_some_and(|word| word.starts_with("culprit_owned_compile_")),
        }
    }
}

#[derive(serde::Serialize)]
struct IdentityFields {
    kinds: BTreeMap<&'static str, &'static str>,
    initial_compile_is_rfc3339: bool,
}
impl IdentityFields {
    fn of(row: &crate::value::Rec) -> Self {
        use crate::value::Val;
        let kinds = ["query_id", "context_settings_id", "object_id", "query_hash", "query_initial_compile_at"]
            .into_iter()
            .map(|field| {
                let kind = match row.columns().find(|(name, _)| *name == field).map(|(_, value)| value) {
                    None => "absent",
                    Some(Val::Null) => "null",
                    Some(Val::Int(value)) if *value >= 0 => "nonnegative-integer",
                    Some(Val::Int(_)) => "negative-integer",
                    Some(Val::Text(value)) if value.is_empty() => "empty-text",
                    Some(Val::Text(_)) => "text",
                    Some(Val::Float(_)) => "float",
                    Some(Val::Bool(_)) => "boolean",
                    Some(Val::Bytes(_)) => "bytes",
                };
                (field, kind)
            })
            .collect();
        let initial_compile_is_rfc3339 =
            row.str("query_initial_compile_at").is_some_and(|value| canonical_time(&value).is_ok());
        Self { kinds, initial_compile_is_rfc3339 }
    }
}

#[cfg(test)]
mod tests;

mod closed_interval;
pub(crate) use closed_interval::seed_closed_interval;
