//! One native cache relation and decoder serve application summaries and streamed grant telemetry.
mod projection;
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

const MAX_APPLICATION_ENTRIES: usize = 10_000;
use crate::cached_statements::{MAX_HASH_SELECTORS, hashes_parameter, valid_hash};
const LARGEST_HASHES: &str = r#"SELECT TOP(30) CONVERT(varchar(18),qs.query_hash,1) AS query_hash
 FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_plan_attributes(qs.plan_handle) pa
 WHERE pa.attribute='dbid' AND CAST(pa.value AS int)=DB_ID()
 AND qs.total_grant_kb>0 AND qs.query_hash IS NOT NULL
 GROUP BY qs.query_hash
 ORDER BY SUM(CAST(qs.total_grant_kb AS decimal(38,0)))-SUM(CAST(qs.total_used_grant_kb AS decimal(38,0))) DESC
 OPTION(MAXDOP 1)"#;
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
    fn can_summarize(&self) -> bool {
        self.has("total_grant_kb") && self.has("total_used_grant_kb") && self.has("max_grant_kb")
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

pub(crate) enum ApplicationSelection<'a> {
    Largest,
    Hashes(&'a [String]),
}
async fn application<R: NativeRead>(
    reader: &mut R,
    selection: ApplicationSelection<'_>,
) -> Result<Option<projection::Summary>> {
    let Some(layout) = Layout::read(reader).await? else { return Ok(None) };
    if !layout.can_summarize() {
        return Ok(None);
    }
    let hashes = match selection {
        ApplicationSelection::Hashes(hashes) => hashes.to_vec(),
        ApplicationSelection::Largest => reader
            .query(LARGEST_HASHES, vec![], 31)
            .await
            .map_err(ReadError::into_error)?
            .iter()
            .map(|row| text(row, "query_hash"))
            .collect::<Result<Vec<_>>>()?,
    };
    let rows = selected_entries(reader, &layout, &hashes).await?;
    Ok(Some(projection::summarize(&rows)?))
}

async fn selected_entries<R: NativeRead>(
    reader: &mut R,
    layout: &Layout,
    hashes: &[String],
) -> Result<Vec<StatementGrantStatistics>> {
    if hashes.len() > MAX_APPLICATION_ENTRIES {
        return Err(CollectionStop::RowLimit.error());
    }
    if !hashes.iter().all(|hash| valid_hash(hash)) {
        return Err(invalid_data());
    }
    let hashes = hashes
        .iter()
        .map(|hash| format!("0x{}", hash[2..].to_ascii_uppercase()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let parameters = hashes.chunks(MAX_HASH_SELECTORS).map(hashes_parameter).collect::<Result<Vec<_>>>()?;
    let sql = select_sql(layout, true);
    let mut entries = Vec::new();
    for parameter in parameters {
        let limit = MAX_APPLICATION_ENTRIES - entries.len() + 1;
        let rows = reader
            .query(&sql, vec![Parameter::Integer(limit as i64), parameter], limit)
            .await
            .map_err(ReadError::into_error)?;
        if entries.len() + rows.len() > MAX_APPLICATION_ENTRIES {
            return Err(CollectionStop::RowLimit.error());
        }
        entries.extend(rows.iter().map(|row| decode(row, layout)).collect::<Result<Vec<_>>>()?);
    }
    Ok(entries)
}

pub(crate) async fn collect_application(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut culprit_core::Coverage,
    selection: ApplicationSelection<'_>,
) -> Option<Vec<culprit_core::GrantStat>> {
    let mut reader =
        crate::read::ReadSession::new(pool, CollectionBudget { page_bytes: 8 * 1024 * 1024, ..Default::default() });
    let result = application(&mut reader, selection).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let result = result.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied && error.hint.is_none() {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    });
    match result {
        Ok(None) => {
            coverage.not_applicable(
                &crate::sources::GRANTS,
                culprit_core::message!("provider.mssql.grants.not_applicable"),
            );
            None
        }
        Ok(Some(summary)) => {
            let rows = coverage.read_rows(&crate::sources::GRANTS, Ok(summary.grants));
            if !summary.complete {
                coverage.partial(&crate::sources::GRANTS, culprit_core::message!("provider.mssql.grants.partial"));
            }
            rows
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
