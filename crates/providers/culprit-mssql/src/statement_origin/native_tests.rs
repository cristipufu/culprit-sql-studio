//! Predicate qualification executes fixed SQL over bound text values, never the SQL supplied as fixture data.
use super::*;
use crate::{
    conn::Pool,
    read::{Parameter, ReadError, ReadSession},
    test_support::config,
};
use culprit_core::telemetry::{CollectionBudget, SourceShutdown};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_candidate_filter_preserves_literals_comments_unknowns_and_uses_only_proven_markers() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    // A supplementary-character collation must not change the prefix's native UTF-16 coordinate system.
    let predicate = application_candidates_sql("(@P1 COLLATE Latin1_General_100_CI_AS_SC)");
    let sql = format!("SELECT CASE WHEN {predicate} THEN 1 ELSE 0 END AS candidate OPTION(MAXDOP 1)");
    let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
    let user =
        Rec::of(&[("engine_internal", crate::value::Val::Int(0)), ("cloud_internal", crate::value::Val::Int(0))]);
    for (text, expected) in [
        ("SELECT 1", true),
        ("SELECT N'/* culprit */' AS label", true),
        ("SELECT N'go''/* culprit */' AS label", true),
        ("SELECT [/* culprit */] FROM t", true),
        ("SELECT \"/* culprit */\" FROM t", true),
        ("SELECT 1 -- /* culprit */", true),
        ("/* outside /* culprit */ */ SELECT 1", true),
        ("/* heading */ SELECT /* culprit */ 1", true),
        ("SELECT N'prior' /* culprit */", true),
        ("SELECT N'🌟/* culprit */' AS label", true),
        ("SELECT α /* culprit */ FROM t", true),
        ("SELECT\u{00a0}/* culprit */ 1", true),
        ("SELECT /* CULPRIT */ 1", true),
        ("SELECT /* culprit */ 1", false),
        ("select /* culprit */ 1", false),
        ("(@P1 nvarchar(7),@P2 int)SELECT /* culprit */ @P1", false),
        ("SELECT /* culprit */ N'cut", false),
    ] {
        let rows =
            reader.query(&sql, vec![Parameter::Text(text.into())], 1).await.map_err(ReadError::into_error).unwrap();
        let candidate = integer(&rows[0], "candidate").unwrap() == 1;
        assert_eq!(candidate, expected, "{text}");
        if !candidate {
            assert!(
                read_prefix(&user, Some(text), false).origin.is_culprit(),
                "native pushdown must not invent positive ownership: {text}"
            );
        }
    }
    for text in [
        monitoring_batch("WITH x AS(SELECT 1 AS n) SELECT n FROM x"),
        monitoring_batch("SELECT @P1"),
        requested_batch("KILL 57"),
    ] {
        let rows = reader.query(&sql, vec![Parameter::Text(text)], 1).await.map_err(ReadError::into_error).unwrap();
        assert_eq!(integer(&rows[0], "candidate").unwrap(), 0);
    }
    let beyond_prefix = format!("{}SELECT /* culprit */ 1", " ".repeat(PROVENANCE_PREFIX_CHARACTERS));
    let rows =
        reader.query(&sql, vec![Parameter::Text(beyond_prefix)], 1).await.map_err(ReadError::into_error).unwrap();
    assert_eq!(integer(&rows[0], "candidate").unwrap(), 1, "an uninspected suffix remains eligible");
    let unknown = application_candidates_sql("CAST(NULL AS nvarchar(max))");
    let rows = reader
        .query(&format!("SELECT CASE WHEN {unknown} THEN 1 ELSE 0 END AS candidate"), vec![], 1)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    assert_eq!(integer(&rows[0], "candidate").unwrap(), 1, "restricted text cannot erase measured work");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
