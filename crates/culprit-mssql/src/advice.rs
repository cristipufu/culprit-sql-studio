//! Read-only index advisor inputs: statements that differ only in literals and Query Store tuning state.

use std::collections::HashSet;

use culprit_core::QueryFamily;

use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_text, required_nonnegative_number, text};
use crate::value::Rec;

/// Ad-hoc plans in this database's plan cache grouped by query_hash: the fallback when Query Store is off, and the
/// memory each family's single-use plans hold. A plan belongs to the database it was compiled in (the plan attribute
/// `dbid`); the batch text's own dbid is empty for ad-hoc and prepared statements. Variants are counted as for Query
/// Store, and Culprit's own statements are left out.
fn cache_families_sql() -> String {
    format!(
        r#"
SELECT TOP 15 CONVERT(varchar(20), qs.query_hash, 1) AS query_hash,
  COUNT(DISTINCT HASHBYTES('SHA2_256', LEFT({body}, 4000))) AS variants, SUM(qs.execution_count) AS executions,
  SUM(qs.total_elapsed_time) / 1000.0 AS total_ms,
  SUM(CAST(cp.size_in_bytes AS bigint)) AS cached_bytes,
  MAX(LEFT(CAST(st.text AS nvarchar(max)), 600)) AS sample
FROM sys.dm_exec_query_stats qs
JOIN sys.dm_exec_cached_plans cp ON cp.plan_handle = qs.plan_handle
CROSS APPLY sys.dm_exec_plan_attributes(qs.plan_handle) pa
CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) st
WHERE cp.objtype IN ('Adhoc', 'Prepared') AND pa.attribute = 'dbid' AND CAST(pa.value AS int) = DB_ID()
  AND {application}
GROUP BY qs.query_hash
HAVING COUNT(DISTINCT HASHBYTES('SHA2_256', LEFT({body}, 4000))) >= 10
ORDER BY variants DESC OPTION(MAXDOP 1)
"#,
        body = crate::query_store::families::body_sql("CAST(st.text AS nvarchar(max))"),
        application = crate::statement_origin::application_candidates_sql("st.text"),
    )
}

/// Statements that read the server's own catalog and statistics (monitoring tools, IDEs, audits): real load, but no
/// application's literals to fix.
fn is_monitoring_text(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "sys.dm_",
        "sys.query_store",
        "sys.objects",
        "sys.columns",
        "sys.indexes",
        "information_schema.",
        "msdb.",
        " sys.",
        "(sys.",
    ]
    .iter()
    .any(|marker| t.contains(marker))
}

pub(crate) struct QueryFamilies {
    pub families: Vec<QueryFamily>,
    pub cache_failure: Option<culprit_core::Error>,
}

pub(crate) async fn query_families<R: NativeRead>(
    client: &mut impl NativeRead,
    window_minutes: u32,
    query_store: Option<&mut crate::query_store::Reader<R>>,
) -> culprit_core::Result<QueryFamilies> {
    let mut families: Vec<QueryFamily> = Vec::new();
    // Families whose heaviest member reads the catalog (a monitoring tool's): no application literals to fix.
    let mut monitoring: HashSet<String> = HashSet::new();
    let has_query_store = query_store.is_some();
    if let Some(reader) = query_store {
        let rows = reader
            .read(
                &crate::query_store::families::sql(),
                vec![crate::read::Parameter::Integer(i64::from(window_minutes.max(1)))],
                75,
            )
            .await?;
        for row in rows {
            let family = decode_family(&row, FamilySource::QueryStore)?;
            if is_monitoring_text(&family.sample) {
                monitoring.insert(family.key);
                continue;
            }
            if monitoring.contains(&family.key) {
                continue;
            }
            match families.iter_mut().find(|existing| existing.key == family.key) {
                Some(existing) => existing.query_ids.extend(family.query_ids),
                None => families.push(family),
            }
        }
    }

    // The plan cache adds memory per family, and stands in for Query Store when it is off.
    let cached =
        client.query(&cache_families_sql(), vec![], 16).await.map_err(ReadError::into_error).and_then(|rows| {
            rows.iter().map(|row| decode_family(row, FamilySource::PlanCache)).collect::<culprit_core::Result<Vec<_>>>()
        });
    let cached = match cached {
        Ok(rows) => rows,
        Err(error)
            if has_query_store
                && culprit_core::telemetry::CollectionStop::from_error(&error)
                    != culprit_core::telemetry::CollectionStop::CancellationUnconfirmed =>
        {
            return Ok(QueryFamilies { families, cache_failure: Some(error) });
        }
        Err(error) => return Err(error),
    };
    for family in cached {
        if monitoring.contains(&family.key) || is_monitoring_text(&family.sample) {
            continue;
        }
        match families.iter_mut().find(|existing| existing.key == family.key) {
            Some(existing) => existing.cached_bytes = family.cached_bytes,
            None if !has_query_store => families.push(family),
            None => {}
        }
    }
    Ok(QueryFamilies { families, cache_failure: None })
}

#[derive(Clone, Copy)]
enum FamilySource {
    QueryStore,
    PlanCache,
}
fn decode_family(row: &Rec, source: FamilySource) -> culprit_core::Result<QueryFamily> {
    let key = text(row, "query_hash")?;
    if !key.strip_prefix("0x").is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid_data());
    }
    let (sample, cached_bytes, query_ids) = match source {
        FamilySource::QueryStore => {
            // Only the first ranked member carries the family's native sample text.
            let query_id = text(row, "query_id")?;
            if query_id.parse::<u64>().is_err() {
                return Err(invalid_data());
            }
            (optional_text(row, "sample")?.unwrap_or_default(), None, vec![query_id])
        }
        FamilySource::PlanCache => (text(row, "sample")?, Some(integer(row, "cached_bytes")? as f64), vec![]),
    };
    Ok(QueryFamily {
        key,
        sample,
        variants: integer(row, "variants")? as f64,
        executions: integer(row, "executions")? as f64,
        total_time_ms: required_nonnegative_number(row, "total_ms")?,
        cached_bytes,
        query_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `body_sql`'s PATINDEX finds: the first `)` followed by neither `,` nor `)`.
    fn body(text: &str) -> &str {
        if !text.starts_with("(@") {
            return text;
        }
        let bytes = text.as_bytes();
        (0..bytes.len().saturating_sub(1))
            .find(|&i| bytes[i] == b')' && !matches!(bytes[i + 1], b',' | b')'))
            .map(|i| &text[i + 1..])
            .unwrap_or(text)
    }

    #[test]
    fn families_count_texts_that_differ_past_their_parameter_declarations() {
        // ADO.NET sizes each nvarchar parameter to its value: one statement, many declarations, no literal.
        assert_eq!(body("(@p0 nvarchar(7))SELECT * FROM t WHERE a = @p0"), "SELECT * FROM t WHERE a = @p0");
        assert_eq!(body("(@p0 nvarchar(9))SELECT * FROM t WHERE a = @p0"), "SELECT * FROM t WHERE a = @p0");
        assert_eq!(
            body("(@p0 nvarchar(7),@p1 int)UPDATE t SET b = @p1 WHERE a = @p0"),
            "UPDATE t SET b = @p1 WHERE a = @p0"
        );
        assert_eq!(body("(@p0 decimal(10,2))SELECT @p0"), "SELECT @p0");
        assert_eq!(body("SELECT * FROM t WHERE a = 'x'"), "SELECT * FROM t WHERE a = 'x'");

        let sql = crate::query_store::families::sql();
        assert!(sql.contains("COUNT(DISTINCT body_hash) AS variants"), "{sql}");
        assert!(sql.contains("PATINDEX(N'%)[^,)]%', qt.query_sql_text)"));
        assert!(sql.contains(&crate::statement_origin::application_candidates_sql("qt.query_sql_text")));
        let cache = cache_families_sql();
        assert!(cache.contains("pa.attribute = 'dbid' AND CAST(pa.value AS int) = DB_ID()"), "{cache}");
        assert!(!cache.contains("st.dbid IS NULL"), "another database's ad-hoc statements aren't this one's");
        assert!(cache.contains(&crate::statement_origin::application_candidates_sql("st.text")));
        assert!(is_monitoring_text("SELECT session_id FROM sys.dm_exec_requests WHERE x = 1"));
        assert!(!is_monitoring_text("SELECT * FROM shop.orders WHERE id = 1"));
    }
    fn stored_family() -> Rec {
        use crate::value::Val;
        Rec::of(&[
            ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
            ("query_id", Val::Text("3".into())),
            ("sample", Val::Text("SELECT 1 FROM dbo.orders".into())),
            ("variants", Val::Int(12)),
            ("executions", Val::Int(20)),
            ("total_ms", Val::Float(100.0)),
        ])
    }
    #[tokio::test]
    async fn unreadable_cache_preserves_stored_families_with_explicit_partial_evidence() {
        use crate::{read::ReadError, resources::test_support::FakeRead};
        use culprit_core::{Error, ErrorCode};
        let error = Error::new(ErrorCode::PermissionDenied, "fixture refusal");
        let mut cache = FakeRead::new(vec![Err(ReadError::Source(error.clone()))]);
        let mut stored = crate::query_store::Reader::new(FakeRead::new(vec![Ok(vec![stored_family()])]));
        let read = query_families(&mut cache, 60, Some(&mut stored)).await.unwrap();
        assert_eq!(read.families.len(), 1);
        assert_eq!(read.families[0].query_ids, ["3"]);
        assert_eq!(read.families[0].cached_bytes, None);
        assert_eq!(read.cache_failure.unwrap().code, error.code);
        assert_eq!(cache.calls.len(), 1);
        let mut cache = FakeRead::new(vec![Err(ReadError::Source(error))]);
        assert!(query_families(&mut cache, 60, None::<&mut crate::query_store::Reader<FakeRead>>).await.is_err());
    }
    #[tokio::test]
    async fn uncertain_cache_cancellation_is_never_a_successful_partial_read() {
        use crate::{read::ReadError, resources::test_support::FakeRead};
        use culprit_core::telemetry::CollectionStop;
        let mut cache = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
        let mut stored = crate::query_store::Reader::new(FakeRead::new(vec![Ok(vec![stored_family()])]));
        let error = query_families(&mut cache, 60, Some(&mut stored)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        assert_eq!(cache.calls.len(), 1);
    }
    #[test]
    fn missing_or_invalid_family_statistics_never_become_zero_cost_advice() {
        use crate::{resources::test_support::replace, value::Val};
        let row = stored_family();
        assert!(decode_family(&row, FamilySource::QueryStore).is_ok());
        for (column, value) in [
            ("executions", Val::Null),
            ("variants", Val::Int(-1)),
            ("total_ms", Val::Float(f64::NAN)),
            ("query_id", Val::Text("bad".into())),
        ] {
            assert!(decode_family(&replace(&row, &[(column, value)]), FamilySource::QueryStore).is_err());
        }
    }
}
