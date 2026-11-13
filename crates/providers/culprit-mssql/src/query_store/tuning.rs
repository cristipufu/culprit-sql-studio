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
            && crate::read::stops_remaining_reads(error)
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
    async fn terminal_tuning_failures_stop_siblings_without_retry_or_claiming_feature_absence() {
        use crate::resources::test_support::FakeRead;
        use culprit_core::{Error, ErrorCode};
        let failures = [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::Pressure]
            .into_iter()
            .map(CollectionStop::error)
            .chain(
                [ErrorCode::AuthFailed, ErrorCode::ConnectionFailed]
                    .into_iter()
                    .map(|code| Error::new(code, "fixture native failure")),
            );
        for failure in failures {
            let mut reader = FakeRead::new(vec![Ok(vec![native::tests::layout_row()]), Err(failure.clone().into())]);
            let report = read(&mut reader).await;
            assert_eq!(report.parts.len(), Kind::ALL.len());
            for (_, result) in report.parts {
                let error = result.unwrap_err();
                assert_eq!(error.code, failure.code);
                assert_eq!(error.detail, failure.detail);
            }
            assert_eq!(reader.calls.len(), 2);
        }
    }

    #[tokio::test]
    async fn a_completed_tuning_permission_error_keeps_independent_native_sources_available() {
        use crate::resources::test_support::FakeRead;
        use culprit_core::{Error, ErrorCode};
        let mut reader = FakeRead::new(vec![
            Ok(vec![native::tests::layout_row()]),
            Err(Error::new(ErrorCode::PermissionDenied, "fixture denied").into()),
            Ok(vec![]),
            Ok(vec![native::tests::forced_row()]),
            Ok(vec![]),
        ]);
        let report = read(&mut reader).await;
        assert_eq!(report.parts[0].1.as_ref().unwrap_err().code, ErrorCode::PermissionDenied);
        assert!(matches!(&report.parts[1].1, Ok(Some(values)) if values.is_empty()));
        assert!(matches!(&report.parts[2].1, Ok(Some(values)) if values.len() == 1));
        assert!(matches!(&report.parts[3].1, Ok(Some(values)) if values.is_empty()));
        assert_eq!(reader.calls.len(), 5);
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
