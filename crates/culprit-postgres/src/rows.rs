use culprit_core::{
    Result,
    limits::{MAX_RESULT_BYTES, MAX_RESULT_ROWS},
    telemetry::CollectionStop,
};
use tokio_postgres::{Client, SimpleQueryMessage, SimpleQueryRow};

use crate::connect::map_error;

/// Appended to every statement Culprit sends, so its own queries can be told apart in pg_stat_statements
/// (which keeps trailing comments but drops leading ones).
pub const TAG: &str = "/* culprit */";

/// Separator used by `string_agg` in catalog queries; it cannot appear in identifiers.
pub const LIST_SEPARATOR: char = '\u{1f}';

/// A row from the simple-query protocol, where every value arrives as text.
#[derive(Clone, Debug)]
pub struct Row {
    columns: Vec<(String, Option<String>)>,
}

impl Row {
    pub fn from_fields(columns: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        Self { columns: columns.into_iter().collect() }
    }
    pub(crate) fn from_native(row: SimpleQueryRow) -> Self {
        Self::from_fields(
            row.columns()
                .iter()
                .enumerate()
                .map(|(index, column)| (column.name().to_owned(), row.get(index).map(str::to_owned))),
        )
    }
    pub(crate) fn value(&self, column: &str) -> Option<Option<&str>> {
        self.columns.iter().find(|(name, _)| name == column).map(|(_, value)| value.as_deref())
    }

    pub fn text(&self, column: &str) -> Option<String> {
        self.value(column).flatten().map(str::to_owned)
    }

    pub fn string(&self, column: &str) -> String {
        self.text(column).unwrap_or_default()
    }

    pub fn number(&self, column: &str) -> Option<f64> {
        let value = self.value(column).flatten()?;
        value.trim().parse::<f64>().ok().filter(|v| v.is_finite())
    }

    pub fn flag(&self, column: &str) -> bool {
        matches!(self.value(column).flatten(), Some("t" | "true" | "on" | "1"))
    }

    pub fn list(&self, column: &str) -> Vec<String> {
        self.text(column)
            .map(|v| v.split(LIST_SEPARATOR).filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }
}

pub async fn query(client: &Client, sql: &str) -> Result<Vec<Row>> {
    checked_query(client, sql, culprit_core::limits::MAX_RESULT_ROWS).await
}

pub(crate) async fn checked_query(client: &Client, sql: &str, limit: usize) -> Result<Vec<Row>> {
    let (rows, stopped) = bounded_rows(client, sql, limit).await?;
    if let Some(stopped) = stopped {
        return Err(stopped.error());
    }
    Ok(rows)
}

/// Retain a bounded prefix of collector rows, draining completion and late errors after retention stops. The caller
/// must record partial coverage when truncated; dropping rows never establishes absence or a complete catalog.
pub async fn query_bounded(client: &Client, sql: &str, limit: usize) -> Result<(Vec<Row>, bool)> {
    let (rows, stopped) = bounded_rows(client, sql, limit).await?;
    Ok((rows, stopped.is_some()))
}

async fn bounded_rows(client: &Client, sql: &str, limit: usize) -> Result<(Vec<Row>, Option<CollectionStop>)> {
    use futures_util::TryStreamExt;
    let stream = client.simple_query_raw(&tagged(sql)).await.map_err(map_error)?;
    tokio::pin!(stream);
    let mut budget = culprit_read::ReadBudget::new(limit.min(MAX_RESULT_ROWS), MAX_RESULT_BYTES);
    let mut rows = Vec::new();
    let mut stopped = None;
    while let Some(message) = stream.try_next().await.map_err(map_error)? {
        if let SimpleQueryMessage::Row(row) = message {
            if stopped.is_some() {
                continue;
            }
            let bytes = (0..row.len()).fold(0usize, |total, i| total.saturating_add(row.get(i).map_or(0, str::len)));
            if let Err(error) = budget.row_started().and_then(|_| budget.charge_values(bytes)) {
                stopped = Some(CollectionStop::from_error(&error));
                continue;
            }
            rows.push(Row::from_native(row));
        }
    }
    Ok((rows, stopped))
}

pub async fn query_one(client: &Client, sql: &str) -> Result<Option<Row>> {
    Ok(query(client, sql).await?.into_iter().next())
}

pub fn tagged(sql: &str) -> String {
    // YSQL's QPM has its own supported opt-out; comments are inert on PostgreSQL. Keep TAG last so scans exclude Culprit's own statements.
    format!("{sql}\n/* __YB_STAT_PLANS_SKIP */ {TAG}")
}

pub(crate) fn monitoring(sql: &str) -> String {
    tagged(&format!(
        "{sql}\n/* culprit:monitor */ /* culprit:template:{} */",
        culprit_core::telemetry::digest(sql.as_bytes())
    ))
}

/// UTC ISO-8601 text for a timestamptz expression, matching the format other providers emit.
pub fn iso(expression: &str) -> String {
    format!("to_char(({expression}) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')")
}

pub fn quote_ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        return name.to_string();
    }
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_quote_only_when_needed() {
        assert_eq!(quote_ident("app_user"), "app_user");
        assert_eq!(quote_ident("AppUser"), "\"AppUser\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }
}
