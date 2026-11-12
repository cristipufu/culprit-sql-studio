//! Native tuning facts and app projection share one reader and one decoder per source.
pub(crate) mod native;
mod projection;
use crate::read::NativeRead;
use culprit_core::{Result, telemetry::*};
pub(crate) use native::{Kind, Layout, read_kind};

pub(crate) struct Collected {
    parts: Vec<(Kind, Result<Option<Vec<TuningFact>>>)>,
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Collected {
    let layout = Layout::read(reader).await;
    let mut stopped: Option<culprit_core::Error> = None;
    let mut parts = Vec::new();
    for kind in Kind::ALL {
        let result = match (&layout, &stopped) {
            (_, Some(error)) | (Err(error), _) => Err(error.clone()),
            (Ok(layout), None) => read_kind(reader, kind, layout, 10_000).await,
        };
        if let Err(error) = &result
            && matches!(
                CollectionStop::from_error(error),
                CollectionStop::TimedOut
                    | CollectionStop::Cancelled
                    | CollectionStop::CancellationUnconfirmed
                    | CollectionStop::QueryLimit
                    | CollectionStop::ByteLimit
            )
        {
            stopped = Some(error.clone());
        }
        parts.push((kind, result));
    }
    Collected { parts }
}

/// "[a],[b] ASC" → ["a", "b"]: a column list as SQL Server writes it, brackets and sort order dropped.
fn column_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut current, mut quoted) = (String::new(), false);
    for c in text.chars().chain([',']) {
        match c {
            '[' => quoted = true,
            ']' => quoted = false,
            ',' if !quoted => {
                let mut column = current.trim().to_string();
                for order in [" ASC", " DESC"] {
                    if column.to_ascii_uppercase().ends_with(order) {
                        column.truncate(column.len() - order.len());
                    }
                }
                let column = column.trim();
                if !column.is_empty() {
                    out.push(column.to_string());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    out
}

/// What's between the parentheses that open at `text`'s first `(`, and what follows them.
fn parenthesized(text: &str) -> Option<(&str, &str)> {
    let start = text.find('(')?;
    let mut depth = 0;
    for (i, c) in text[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[start + 1..start + i], &text[start + i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// The index in a recommendation's script: `CREATE NONCLUSTERED INDEX [name] ON [schema].[table] ([a], [b]) INCLUDE
/// ([c]) WITH (ONLINE = ON)` or `DROP INDEX [name] ON [schema].[table]`.
fn index_from_script(script: &str) -> Option<culprit_core::RecommendedIndex> {
    let upper = script.to_ascii_uppercase();
    let after_index = upper.find("INDEX ")? + "INDEX ".len();
    let on = after_index + upper[after_index..].find(" ON ")?;
    let name = script[after_index..on].trim().trim_matches(['[', ']']).to_string();
    let rest = &script[on + " ON ".len()..];
    let object_end = [rest.find(['(', ';']), rest.to_ascii_uppercase().find(" WITH")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    let object = column_list(&rest[..object_end].replace("].[", "],["));
    let (schema, table) = match object.as_slice() {
        [.., schema, table] => (schema.clone(), table.clone()),
        [table] => ("dbo".to_string(), table.clone()),
        [] => return None,
    };
    let after_object = &rest[object_end..];
    let keys = after_object.trim_start().starts_with('(').then(|| parenthesized(after_object)).flatten();
    let (columns, include) = match keys {
        Some((keys, after)) => {
            let include = after
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("INCLUDE")
                .then(|| parenthesized(after).map(|(cols, _)| column_list(cols)))
                .flatten()
                .unwrap_or_default();
            (column_list(keys), include)
        }
        None => (Vec::new(), Vec::new()),
    };
    Some(culprit_core::RecommendedIndex { schema, table, name: (!name.is_empty()).then_some(name), columns, include })
}

/// The index an index recommendation is about: from its details, else from its script.
fn recommended_index(r: &crate::value::Rec, script: Option<&str>) -> Option<culprit_core::RecommendedIndex> {
    let from_script = script.and_then(index_from_script);
    let (Some(schema), Some(table)) = (r.str("index_schema"), r.str("index_table")) else { return from_script };
    let unquote = |s: String| s.trim().trim_matches(['[', ']']).to_string();
    let columns = r.str("index_columns").map(|c| column_list(&c)).unwrap_or_default();
    Some(culprit_core::RecommendedIndex {
        schema: unquote(schema),
        table: unquote(table),
        name: r.str("index_name").map(unquote).or_else(|| from_script.as_ref().and_then(|i| i.name.clone())),
        columns: if columns.is_empty() {
            from_script.as_ref().map(|i| i.columns.clone()).unwrap_or_default()
        } else {
            columns
        },
        include: r
            .str("included_columns")
            .map(|c| column_list(&c))
            .filter(|c| !c.is_empty())
            .or_else(|| from_script.as_ref().map(|i| i.include.clone()))
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use native::{forced_plans_sql, recommendations_sql};
    #[tokio::test]
    async fn tuning_command_failure_is_not_retried_or_hidden_as_feature_absence() {
        use crate::{
            read::ReadError,
            resources::test_support::FakeRead,
            value::{Rec, Val},
        };
        use culprit_core::telemetry::CollectionStop;
        let mut reader = super::super::Reader::new(FakeRead::new(vec![
            Ok(vec![Rec::of(&[
                ("automatic_tuning", Val::Int(1)),
                ("recommendations", Val::Int(0)),
                ("forcing_type", Val::Int(0)),
                ("hints", Val::Int(0)),
                ("hint_replica", Val::Int(0)),
                ("hint_source", Val::Int(0)),
                ("query_store", Val::Int(1)),
                ("metadata_visibility", Val::Bool(true)),
            ])]),
            Err(ReadError::Stopped(CollectionStop::TimedOut)),
        ]));
        let report = read(&mut reader).await;
        let error = report.parts[0].1.as_ref().err().unwrap();
        assert_eq!(CollectionStop::from_error(error), CollectionStop::TimedOut);
    }
    #[test]
    fn query_store_times_are_switched_to_utc_not_shifted_again() {
        // Query Store keeps datetimeoffset values: shifting them by the server's offset counted it twice.
        let forced = forced_plans_sql(true);
        assert!(forced.contains("SWITCHOFFSET(last_execution_time, '+00:00')"), "{forced}");
        assert!(!forced.contains("GETUTCDATE"));
        let recommendations = recommendations_sql();
        assert!(recommendations.contains("SWITCHOFFSET(MAX(p.last_execution_time), '+00:00')"));
    }

    #[test]
    fn an_index_recommendation_script_names_its_table_and_columns() {
        let i = index_from_script(
            "CREATE NONCLUSTERED INDEX [nci_wi_tickets_8F2A] ON [shop].[support_tickets] ([account_id], [is_deleted] ASC) \
             INCLUDE ([status], [opened_at]) WITH (ONLINE = ON)",
        )
        .unwrap();
        assert_eq!(i.name.as_deref(), Some("nci_wi_tickets_8F2A"));
        assert_eq!((i.schema.as_str(), i.table.as_str()), ("shop", "support_tickets"));
        assert_eq!(i.columns, ["account_id", "is_deleted"]);
        assert_eq!(i.include, ["status", "opened_at"]);

        let d = index_from_script("DROP INDEX [ix_orders_legacy] ON [shop].[orders] WITH (ONLINE = ON);").unwrap();
        assert_eq!(d.name.as_deref(), Some("ix_orders_legacy"));
        assert_eq!((d.schema.as_str(), d.table.as_str()), ("shop", "orders"));
        assert!(d.columns.is_empty() && d.include.is_empty());

        assert_eq!(column_list("[a],[b, c] DESC, d"), ["a", "b, c", "d"]);
        assert!(index_from_script("EXEC sp_query_store_force_plan @query_id = 1, @plan_id = 2").is_none());
    }
}
