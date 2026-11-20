//! A fixture oracle selects only its exact original Query Store identity; it is not a new source or binder.
use super::*;
use crate::{
    query_store::{catalog_sql, decode},
    read::Parameter,
};

const SELECTED_ROWS: usize = 2;

pub(super) async fn read(
    probe: &Probe,
    executed: &RecordedRun,
    expected: Option<&NativePlanIdentity>,
) -> NativePlanIdentity {
    let plan_hash = crate::plan_xml::original_plan_hash(&executed.plan).expect("original native selected plan hash");
    assert!(crate::cached_statements::valid_hash(&plan_hash));
    let query_hash = executed.context.query_hash.as_ref().unwrap();
    let sql = executed.plan.statement.as_ref().unwrap();
    let selected_id = if expected.is_some() { "AND q.query_id=@P4 AND p.plan_id=@P5" } else { "" };
    let query = format!(
        "SELECT TOP(@P1) /* culprit fixture exact original Query Store identity */ {columns},DB_ID() AS database_id, \
         CASE WHEN DATALENGTH(qt.query_sql_text)<=4096 THEN qt.query_sql_text ELSE CAST(NULL AS nvarchar(max)) END AS native_statement \
         FROM sys.query_store_query q JOIN sys.query_store_plan p ON p.query_id=q.query_id \
         JOIN sys.query_store_query_text qt ON qt.query_text_id=q.query_text_id \
         WHERE q.object_id=OBJECT_ID(N'dbo.scalar_probe') AND q.query_hash=CONVERT(binary(8),@P2,1) \
         AND p.query_plan_hash=CONVERT(binary(8),@P3,1) {selected_id} OPTION(MAXDOP 1)",
        columns = concat!(
            catalog_sql::query_identity_columns!("q.query_id"),
            ",",
            catalog_sql::plan_identity_columns!("p", "")
        ),
    );
    let mut parameters =
        vec![Parameter::Integer(SELECTED_ROWS as i64), Parameter::Text(query_hash.clone()), Parameter::Text(plan_hash)];
    if let Some(expected) = expected {
        parameters.extend([
            Parameter::Integer(expected.query.query_id.parse().unwrap()),
            Parameter::Integer(expected.plan_id.parse().unwrap()),
        ]);
    }
    let mut reader = ReadSession::new(
        probe.pool.clone(),
        CollectionBudget {
            max_queries: 1,
            max_snapshot_rows: SELECTED_ROWS as u32,
            page_rows: SELECTED_ROWS as u32,
            page_bytes: 8192,
            total_bytes: 8192,
            statement_timeout_ms: 1000,
            total_timeout_ms: 5000,
            ..Default::default()
        },
    );
    let observed = reader.query(&query, parameters, SELECTED_ROWS).await;
    let queries = reader.queries();
    let closed = reader.close().await;
    let rows = observed.expect("the bounded original Query Store identity query must complete");
    assert_eq!(closed, SourceShutdown::Quiescent);
    assert_eq!(queries, 1, "the selected metadata oracle has no fallback or polling");
    let [row] = rows.as_slice() else { panic!("one exact original Query Store identity, got {}", rows.len()) };
    assert_eq!(
        crate::read::integer(row, "database_id").unwrap(),
        i64::from(executed.context.database_id),
        "the original execution and selected Query Store identity must belong to the same native database"
    );
    assert_eq!(
        crate::read::optional_text(row, "native_statement")
            .unwrap()
            .as_deref()
            .map(|text| culprit_core::sql::without_parameter_list(text).trim()),
        Some(sql.trim()),
        "the selected Query Store body must match the original statement after its native parameter prefix"
    );
    let identity = decode::plan_identity(row).unwrap().expect("one complete native Query Store identity");
    assert!(identity.is_valid());
    if let Some(expected) = expected {
        assert_eq!(&identity, expected, "the complete original native Query Store identity must remain unchanged");
    }
    identity
}
