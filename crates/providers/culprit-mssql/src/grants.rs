//! One native cache relation and decoder retain original statement-grant observations for scans and telemetry.
mod projection;
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

const MAX_APPLICATION_ENTRIES: usize = 10_000;
use crate::cached_statements::{MAX_HASH_SELECTORS, hashes_parameter, valid_hash};
const COLUMNS: [&str; 8] = [
    "total_grant_kb",
    "total_used_grant_kb",
    "last_grant_kb",
    "last_used_grant_kb",
    "min_grant_kb",
    "min_used_grant_kb",
    "max_grant_kb",
    "max_used_grant_kb",
];

#[derive(Clone)]
pub(crate) struct Layout {
    columns: Vec<String>,
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Option<Self>> {
        let projection=COLUMNS.iter().map(|column|format!("CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_exec_query_stats') AND name=N'{column}') THEN 1 ELSE 0 END AS {column}")).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility,{projection} OPTION(MAXDOP 1)"
        );
        let rows = reader.query(&sql, vec![], 2).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let mut columns = Vec::new();
        for name in COLUMNS {
            match integer(row, name)? {
                1 => columns.push(name.to_string()),
                0 => {}
                _ => return Err(invalid_data()),
            }
        }
        if columns.is_empty() {
            if crate::read::optional_boolean(row, "metadata_visibility")? != Some(true) {
                return Err(culprit_core::Error::new(
                    culprit_core::ErrorCode::PermissionDenied,
                    culprit_core::t!("provider.mssql.grants.metadata_unavailable"),
                )
                .with_hint("GRANT VIEW DEFINITION TO [<user>];"));
            }
            return Ok(None);
        }
        Ok(Some(Self { columns }))
    }
    fn has(&self, name: &str) -> bool {
        self.columns.iter().any(|column| column == name)
    }
}

pub(crate) fn select_sql(layout: &Layout, selected: bool) -> String {
    let fields = COLUMNS
        .iter()
        .map(|name| {
            format!("{} AS {name}", if layout.has(name) { format!("qs.{name}") } else { "CAST(NULL AS bigint)".into() })
        })
        .collect::<Vec<_>>()
        .join(",");
    let hashes = if selected {
        r#"WITH culprit_hashes AS (
 SELECT CONVERT(binary(8),n.h.value('@v[1]','varchar(18)'),1) AS query_hash
 FROM (SELECT CAST(@P2 AS xml) AS body) x CROSS APPLY x.body.nodes('/hashes/h') n(h))"#
    } else {
        ""
    };
    format!(
        r#"{hashes}
 SELECT TOP (@P1) {context},
 qs.execution_count,CONVERT(varchar(40),qs.last_execution_time,126) AS last_execution,
 {fields}
 {relation}
 WHERE {database}{predicate} OPTION(MAXDOP 1)"#,
        context = crate::cached_statements::context_columns(),
        relation = crate::cached_statements::relation(),
        database = crate::cached_statements::DATABASE_PREDICATE,
        predicate = if selected { " AND qs.query_hash IN (SELECT query_hash FROM culprit_hashes)" } else { "" }
    )
}

pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<StatementGrantStatistics> {
    let context = crate::cached_statements::context(row)?;
    let measured = context.identity.has_plan_handle();
    let counter = |name| -> Result<Option<String>> {
        if measured { Ok(optional_integer(row, name)?.map(|value| value.to_string())) } else { Ok(None) }
    };
    let value = StatementGrantStatistics {
        statement: projection::statement(&context, optional_text(row, "text_prefix")?.as_deref()),
        context,
        available_columns: layout.columns.clone(),
        executions: measured.then(|| integer(row, "execution_count").map(|value| value.to_string())).transpose()?,
        last_execution_without_offset: optional_text(row, "last_execution")?
            .map(|value| crate::sql_time::native_time(&value))
            .transpose()?,
        total_grant_kibibytes: counter("total_grant_kb")?,
        total_used_kibibytes: counter("total_used_grant_kb")?,
        last_grant_kibibytes: counter("last_grant_kb")?,
        last_used_kibibytes: counter("last_used_grant_kb")?,
        min_grant_kibibytes: counter("min_grant_kb")?,
        min_used_kibibytes: counter("min_used_grant_kb")?,
        max_grant_kibibytes: counter("max_grant_kb")?,
        max_used_kibibytes: counter("max_used_grant_kb")?,
        reset: culprit_core::ResetMarker::Unknown,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

fn validate_hashes(hashes: &[String]) -> Result<()> {
    if hashes.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if hashes.len() > MAX_APPLICATION_ENTRIES {
        return Err(CollectionStop::RowLimit.error());
    }
    if !hashes.iter().all(|hash| valid_hash(hash)) {
        return Err(invalid_data());
    }
    Ok(())
}
async fn application<R: NativeRead>(
    reader: &mut R,
    hashes: &[String],
) -> Result<Option<Vec<StatementGrantStatistics>>> {
    validate_hashes(hashes)?;
    let Some(layout) = Layout::read(reader).await? else { return Ok(None) };
    Ok(Some(selected_entries(reader, &layout, hashes).await?))
}

async fn selected_entries<R: NativeRead>(
    reader: &mut R,
    layout: &Layout,
    hashes: &[String],
) -> Result<Vec<StatementGrantStatistics>> {
    let hashes = hashes
        .iter()
        .map(|hash| format!("0x{}", hash[2..].to_ascii_uppercase()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let parameters = hashes.chunks(MAX_HASH_SELECTORS).map(hashes_parameter).collect::<Result<Vec<_>>>()?;
    let sql = select_sql(layout, true);
    let mut entries = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    for parameter in parameters {
        let limit = MAX_APPLICATION_ENTRIES - entries.len() + 1;
        let rows = reader
            .query(&sql, vec![Parameter::Integer(limit as i64), parameter], limit)
            .await
            .map_err(ReadError::into_error)?;
        if entries.len() + rows.len() > MAX_APPLICATION_ENTRIES {
            return Err(CollectionStop::RowLimit.error());
        }
        for row in rows {
            let entry = decode(&row, layout)?;
            if !identities.insert(entry.context.identity.key()?) {
                return Err(invalid_data());
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

async fn capture_application<R: NativeRead>(
    reader: &mut R,
    hashes: &[String],
    host: &str,
    port: u16,
) -> Result<StatementGrantSnapshot> {
    validate_hashes(hashes)?;
    let capture = culprit_read::admission::observation_identity()?;
    let before = crate::local_source::Context::read(reader, host, port).await?;
    let rows = application(reader, hashes).await?;
    let after = before.recheck(reader, host, port).await?;
    let context = before.completed(&after)?;
    let supported = rows.is_some();
    let observations = rows.unwrap_or_default();
    let source = crate::sources::GRANTS.name.to_string();
    let coverage = if supported {
        StateSourceCoverage::observed(
            source.clone(),
            observations.len() as u32,
            // Workload hash selectors preserve useful positives but never exhaust the whole cache.
            false,
        )
    } else {
        StateSourceCoverage::not_applicable(source.clone())
    };
    let snapshot = StatementGrantSnapshot {
        capture,
        description: CollectionDescription {
            target: context.target,
            partition: context.partition,
            source,
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementGrants,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: observations.len() as u32,
                sources: vec![coverage],
            }),
        },
        observations,
    };
    if !snapshot.is_valid() {
        return Err(invalid_data());
    }
    Ok(snapshot)
}

async fn collect_from<R: NativeRead>(
    reader: &mut R,
    hashes: &[String],
    host: &str,
    port: u16,
) -> Result<StatementGrantSnapshot> {
    let result = capture_application(reader, hashes, host, port).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

pub(crate) async fn collect_application(
    provider: &crate::MssqlProvider,
    probe: &crate::probe::Probe,
    coverage: &mut culprit_core::Coverage,
    hashes: &[String],
) -> Option<StatementGrantSnapshot> {
    if hashes.is_empty() {
        coverage.source_failed(
            &crate::sources::GRANTS,
            &culprit_core::Error::new(
                culprit_core::ErrorCode::QueryFailed,
                culprit_core::message!("provider.mssql.grants.no_selectors"),
            )
            .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceUnavailable)),
        );
        return None;
    }
    let mut reader = crate::read::ReadSession::new(
        provider.pool.clone(),
        CollectionBudget {
            max_snapshot_rows: MAX_APPLICATION_ENTRIES as u32,
            page_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
    );
    let result = collect_from(&mut reader, hashes, &provider.endpoint_host, provider.endpoint_port).await;
    let result = result.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied && error.hint.is_none() {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    });
    match result {
        Ok(snapshot) => {
            let Some(state) = snapshot
                .description
                .observation
                .snapshot()
                .and_then(|native| if let [state] = native.sources.as_slice() { Some(state) } else { None })
            else {
                coverage.source_failed(&crate::sources::GRANTS, &invalid_data());
                return None;
            };
            if state.state == culprit_core::SourceState::NotApplicable {
                coverage.not_applicable(
                    &crate::sources::GRANTS,
                    culprit_core::message!("provider.mssql.grants.not_applicable"),
                );
            } else {
                coverage.completed(&crate::sources::GRANTS, snapshot.observations.len());
            }
            if state.state == culprit_core::SourceState::Partial {
                coverage.partial(&crate::sources::GRANTS, culprit_core::message!("provider.mssql.grants.partial"));
            }
            Some(snapshot)
        }
        Err(error) => {
            coverage.source_failed(&crate::sources::GRANTS, &error);
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod native_tests;
