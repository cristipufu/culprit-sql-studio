use super::*;
use crate::{
    progress::test_support::{stage as stage_row, statement as statement_row},
    read::test_support::Read,
};

fn request() -> MysqlCurrentRequestWork {
    MysqlCurrentRequestWork {
        thread_id: "107".into(),
        statement_event_id: "3".into(),
        timer_start_ps: "4".into(),
        timer_wait_ps: None,
        rows_examined: None,
        rows_sent: None,
        created_tmp_tables: None,
        created_tmp_disk_tables: None,
        sort_merge_passes: None,
        stage: MysqlStageObservation::NotObserved,
    }
}

#[tokio::test]
async fn current_statement_counters_use_the_shared_native_actor_projection_and_preserve_exact_unknown_and_zero() {
    let mut io = Read::with(vec![Ok(vec![statement_row(&[])])]);
    let StatementRead::Observed(point) = super::statement(&mut io, 7, "shop", true, 16 * 1024).await.unwrap() else {
        panic!()
    };
    assert_eq!(point.observation.actor.session.generation, "107");
    assert_eq!(point.observation.native.mysql().unwrap().rows_examined.as_deref(), Some("9007199254740993"));
    assert_eq!(point.observation.native.mysql().unwrap().rows_sent.as_deref(), Some("0"));
    assert!(point.observation.native.mysql().unwrap().sort_merge_passes.is_none());
    assert_eq!(
        point.observation.statement_identity,
        culprit_core::engines::mysql_activity_statement_identity(
            &point.observation.actor.session,
            point.observation.actor.request.as_ref().unwrap()
        )
        .unwrap()
    );
    let (sql, parameters, limit) = &io.queries[0];
    assert!(sql.contains("e.EVENT_ID AS event_id") && sql.contains("e.TIMER_START AS timer_start"));
    assert!(sql.contains("t.PROCESSLIST_ID=?") && sql.contains("t.PROCESSLIST_DB=?"));
    assert!(sql.contains("NOT LIKE '%/* culprit:%'"));
    assert!(!sql.contains("ORDER BY") && !sql.contains("GROUP BY"));
    assert_eq!(*limit, 2);
    assert_eq!(parameters.len(), 3);
    assert!(io.explains.is_empty());
}

#[tokio::test]
async fn current_stage_requires_the_native_statement_parent_not_a_matching_thread_or_name() {
    for changes in [
        vec![("nesting_event_id", 99u64.into())],
        vec![("thread_id", 108u64.into())],
        vec![("nesting_event_type", "STAGE".into())],
        vec![("nesting_event_id", Value::NULL)],
    ] {
        let mut io = Read::with(vec![Ok(vec![stage_row(&changes)])]);
        assert_eq!(
            super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.unwrap(),
            MysqlStageObservation::Unlinked
        );
    }
    let mut io = Read::with(vec![Ok(vec![stage_row(&[("end_event_id", 5u64.into())])])]);
    assert_eq!(
        super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.unwrap(),
        MysqlStageObservation::NotObserved
    );
    let mut io = Read::with(vec![Ok(vec![])]);
    assert_eq!(
        super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.unwrap(),
        MysqlStageObservation::NotObserved
    );
}

#[tokio::test]
async fn untimed_native_stage_and_zero_work_estimate_are_preserved_without_a_completion_or_timer_order_claim() {
    let mut io = Read::with(vec![Ok(vec![stage_row(&[
        ("timer_wait_ps", Value::NULL),
        ("work_completed", 9u64.into()),
        ("work_estimated", 0u64.into()),
    ])])]);
    let MysqlStageObservation::Observed { event } =
        super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.unwrap()
    else {
        panic!()
    };
    assert!(event.timer_start_ps.is_none() && event.timer_wait_ps.is_none());
    assert_eq!(event.work_completed.as_deref(), Some("9"));
    assert_eq!(event.work_estimated.as_deref(), Some("0"));
    let mut io = Read::with(vec![Ok(vec![stage_row(&[("timer_start_ps", 1u64.into())])])]);
    assert!(matches!(
        super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.unwrap(),
        MysqlStageObservation::Observed { .. }
    ));
}

#[tokio::test]
async fn unsupported_stage_work_columns_are_never_queried_or_substituted_by_mariadb_percentages() {
    for (flavor, version, available) in
        [(Flavor::Mysql, "5.7.4", false), (Flavor::Mysql, "5.7.5", true), (Flavor::Mariadb, "11.8.9-MariaDB", false)]
    {
        let mut io = Read::with(vec![Ok(vec![])]);
        super::stage(&mut io, &request(), flavor, version).await.unwrap();
        let sql = &io.queries[0].0;
        assert_eq!(sql.contains("WORK_COMPLETED AS work_completed"), available);
        assert_eq!(sql.contains("NULL AS work_completed"), !available);
        assert!(!sql.contains("PROCESSLIST") && !sql.contains("UPDATE"));
        assert!(sql.contains("NESTING_EVENT_TYPE AS nesting_event_type"));
        assert_eq!(io.queries.len(), 1);
    }
}

#[tokio::test]
async fn malformed_counters_and_duplicate_points_fail_without_scalar_or_zero_fallbacks() {
    for row in [
        statement_row(&[("created_tmp_tables", Value::Double(1.0))]),
        statement_row(&[("created_tmp_disk_tables", Value::Int(-1))]),
        statement_row(&[("rows_examined", "1.5".into())]),
    ] {
        let mut io = Read::with(vec![Ok(vec![row])]);
        assert!(super::statement(&mut io, 7, "shop", true, 16 * 1024).await.is_err());
        assert_eq!(io.queries.len(), 1);
    }
    let mut io = Read::with(vec![Ok(vec![statement_row(&[]), statement_row(&[])])]);
    assert!(super::statement(&mut io, 7, "shop", true, 16 * 1024).await.is_err());
    let mut io = Read::with(vec![Ok(vec![stage_row(&[]), stage_row(&[])])]);
    assert!(super::stage(&mut io, &request(), Flavor::Mysql, "8.4.0").await.is_err());
}

#[tokio::test]
async fn foreign_schema_is_unavailable_and_oversized_sql_does_not_discard_native_work_or_request_identity() {
    let mut io = Read::with(vec![Ok(vec![statement_row(&[("request_db", "foreign".into())])])]);
    assert!(matches!(super::statement(&mut io, 7, "shop", true, 16 * 1024).await.unwrap(), StatementRead::Foreign));
    let mut io =
        Read::with(vec![Ok(vec![statement_row(&[("sql_text", Value::NULL), ("sql_text_bytes", 1000u64.into())])])]);
    let StatementRead::Observed(point) = super::statement(&mut io, 7, "shop", true, 100).await.unwrap() else {
        panic!()
    };
    assert_eq!(point.observation.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized });
    assert_eq!(point.observation.native.mysql().unwrap().created_tmp_tables.as_deref(), Some("1"));
}

#[tokio::test]
async fn zero_native_statement_timer_origin_does_not_require_a_duration_or_become_a_utc_clock() {
    let mut io =
        Read::with(vec![Ok(vec![statement_row(&[("timer_start", 0u64.into()), ("elapsed_ps", Value::NULL)])])]);
    let StatementRead::Observed(point) = super::statement(&mut io, 7, "shop", true, 16 * 1024).await.unwrap() else {
        panic!()
    };
    assert_eq!(point.observation.native.mysql().unwrap().timer_start_ps, "0");
    assert!(point.observation.native.mysql().unwrap().timer_wait_ps.is_none());
    assert_eq!(point.observation.actor.request.as_ref().unwrap().generation, "0");
    assert_eq!(point.observation.observed_at, canonical_time(crate::progress::test_support::POINT).unwrap());
    assert!(point.observation.is_valid());
}
