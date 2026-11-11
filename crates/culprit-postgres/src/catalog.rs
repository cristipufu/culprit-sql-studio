//! Tables, indexes, columns, foreign keys and configuration. No workload planning or scan orchestration.
use crate::{
    probe::Probe,
    rows::{query, query_one},
};
use culprit_core::{ColumnInfo, ForeignKey, IndexStat, Result, Setting, TableStat};
use tokio_postgres::Client;

pub(crate) async fn tables(client: &Client, version_num: u32) -> Result<Vec<TableStat>> {
    let sql = crate::native::relations::table_query;
    let rows = match query(client, &sql(version_num, crate::native::relations::SizeRead::Physical, None)).await {
        Ok(rows) => rows,
        Err(error) if error.code == culprit_core::ErrorCode::Timeout => {
            query(client, &sql(version_num, crate::native::relations::SizeRead::CatalogEstimate, None)).await?
        }
        Err(error) => return Err(error),
    };
    rows.iter()
        .map(|row| crate::native::relations::table(row, &row.string("database_oid")))
        .map(|result| result.map(|table| crate::native::relations::project(&table)))
        .collect()
}

pub(crate) async fn indexes(client: &Client, probe: &Probe) -> Result<Vec<IndexStat>> {
    let sql = crate::native::indexes::query(probe.version_num, probe.timescale_version.is_some(), true, None);
    let rows = query(client, &sql).await?;
    let mut indexes = rows
        .into_iter()
        .map(|row| crate::native::indexes::decode(row, None))
        .map(|result| result.map(|reading| crate::native::indexes::project(&reading)))
        .collect::<Result<Vec<_>>>()?;

    // Exact sizes take a lock per index; fall back to the page-count estimate if any is held up.
    let sizes = query(
        client,
        "SELECT n.nspname AS schema_name, i.relname AS index_name, pg_relation_size(x.indexrelid) AS bytes
         FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_namespace n ON n.oid = i.relnamespace
         WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_toast%'",
    )
    .await;
    if let Ok(sizes) = sizes {
        for row in sizes {
            let (schema, name) = (row.string("schema_name"), row.string("index_name"));
            if let Some(index) = indexes.iter_mut().find(|i| i.schema == schema && i.name == name) {
                index.size_bytes = row.number("bytes").or(index.size_bytes);
            }
        }
    }
    Ok(indexes)
}

pub(crate) async fn columns(client: &Client) -> Result<Vec<ColumnInfo>> {
    let kind = crate::native::catalog::Kind::Columns;
    query(client, &crate::native::catalog::query(kind, true, 100_001))
        .await?
        .into_iter()
        .map(|row| {
            crate::native::catalog::decode(kind, row, None, culprit_core::telemetry::MAX_CATALOG_EXPRESSION_BYTES)
        })
        .map(|result| result.and_then(crate::native::catalog::column))
        .collect()
}
pub(crate) async fn foreign_keys(client: &Client) -> Result<Vec<ForeignKey>> {
    let kind = crate::native::catalog::Kind::ForeignKeys;
    query(client, &crate::native::catalog::query(kind, false, 100_001))
        .await?
        .into_iter()
        .map(|row| {
            crate::native::catalog::decode(kind, row, None, culprit_core::telemetry::MAX_CATALOG_EXPRESSION_BYTES)
        })
        .map(|result| result.map(crate::native::catalog::foreign_key))
        .collect()
}

pub(crate) async fn settings(client: &Client) -> Result<Vec<Setting>> {
    let rows = query(client, &crate::native::settings::query(crate::native::settings::SETTINGS, 1000)).await?;
    let mut out = rows
        .iter()
        .map(crate::native::settings::decode)
        .map(|result| result.map(crate::native::settings::project))
        .collect::<Result<Vec<_>>>()?;
    if let Some(row) =
        query_one(client, "SELECT age(datfrozenxid) AS age FROM pg_database WHERE datname = current_database()").await?
    {
        out.push(Setting {
            name: "datfrozenxid_age".into(),
            value: row.string("age"),
            default_value: None,
            scope: "database".into(),
        });
    }
    Ok(out)
}

/// Render a pg_settings value with its unit the way Postgres displays it (`4MB`, `200ms`).
pub fn pretty_setting(value: &str, unit: Option<&str>) -> String {
    let Some(unit) = unit else { return value.to_string() };
    let Ok(number) = value.trim().parse::<i64>() else { return value.to_string() };
    if number < 0 {
        return number.to_string();
    }
    const MEMORY: &[(&str, i64)] = &[("TB", 1 << 40), ("GB", 1 << 30), ("MB", 1 << 20), ("kB", 1 << 10), ("B", 1)];
    const TIME: &[(&str, i64)] = &[("d", 86_400_000), ("h", 3_600_000), ("min", 60_000), ("s", 1_000), ("ms", 1)];
    let (base, scale) = match unit {
        "B" => (MEMORY, 1),
        "kB" => (MEMORY, 1 << 10),
        "8kB" => (MEMORY, 8 << 10),
        "16kB" => (MEMORY, 16 << 10),
        "32kB" => (MEMORY, 32 << 10),
        "MB" => (MEMORY, 1 << 20),
        "ms" => (TIME, 1),
        "s" => (TIME, 1_000),
        "min" => (TIME, 60_000),
        _ => return format!("{value} {unit}"),
    };
    let amount = number * scale;
    if amount == 0 {
        return format!("0{}", base.last().map(|(u, _)| *u).unwrap_or(""));
    }
    for (label, size) in base {
        if amount % size == 0 {
            return format!("{}{label}", amount / size);
        }
    }
    amount.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_render_like_postgres() {
        assert_eq!(pretty_setting("16384", Some("8kB")), "128MB");
        assert_eq!(pretty_setting("4096", Some("kB")), "4MB");
        assert_eq!(pretty_setting("200", Some("ms")), "200ms");
        assert_eq!(pretty_setting("60", Some("s")), "1min");
        assert_eq!(pretty_setting("4", None), "4");
        assert_eq!(pretty_setting("-1", Some("kB")), "-1");
    }
}
