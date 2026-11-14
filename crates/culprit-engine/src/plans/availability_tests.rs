use super::*;

fn portable_plan() -> String {
    let scan: culprit_core::Snapshot =
        serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let plan = scan.workload.as_ref().unwrap().queries.iter().find_map(|query| query.plan.as_ref()).unwrap();
    serde_json::to_string(plan).unwrap()
}

#[test]
fn declared_format_availability_matches_actual_dispatch_for_every_native_format() {
    let portable = portable_plan();
    let inputs = [
        (PlanFormat::ShowplanXml, include_str!("../../tests/plans/mssql-seek.sqlplan"), cfg!(feature = "mssql")),
        (PlanFormat::PostgresJson, include_str!("../../tests/plans/pg-index.json"), cfg!(feature = "postgres")),
        (
            PlanFormat::YugabyteJson,
            include_str!("../../../providers/culprit-yugabyte/tests/data/loops.json"),
            cfg!(feature = "yugabyte"),
        ),
        (PlanFormat::PostgresText, include_str!("../../tests/plans/pg-analyze.txt"), cfg!(feature = "postgres")),
        (PlanFormat::MysqlJson, include_str!("../../tests/plans/mysql-v2.json"), cfg!(feature = "mysql")),
        (PlanFormat::MysqlTree, include_str!("../../tests/plans/mysql-tree-g.txt"), cfg!(feature = "mysql")),
        (PlanFormat::SqliteText, "QUERY PLAN\n|--SCAN o\n`--USE TEMP B-TREE FOR ORDER BY\n", cfg!(feature = "sqlite")),
        (PlanFormat::Culprit, portable.as_str(), true),
    ];
    for (format, text, enabled) in inputs {
        assert_eq!(format.is_available(), enabled, "{format:?}");
        // SQLite owns its shell-format detector with its parser; a provider-free build includes neither.
        let detected = if format == PlanFormat::SqliteText && !enabled { None } else { Some(format) };
        assert_eq!(detect(text), detected, "{format:?}");
        assert_eq!(parse_plan(text).is_ok(), format.is_available(), "{format:?}");
    }
}

#[test]
fn native_yugabyte_context_selects_its_parser_for_an_unmarked_qpm_plan() {
    let text = include_str!("../../../providers/culprit-yugabyte/tests/data/qpm.json");
    assert_eq!(detect(text), Some(PlanFormat::PostgresJson));
    assert_eq!(detect_native(text, Engine::Yugabyte), Some(PlanFormat::YugabyteJson));
    let parsed = parse_native_plan(text, Engine::Yugabyte);
    assert_eq!(parsed.is_ok(), PlanFormat::YugabyteJson.is_available());
    if let Ok(plan) = parsed {
        assert_eq!(plan.engine, Engine::Yugabyte);
        assert!(plan.facts().estimated_cost_unavailable);
        assert!(plan.nodes().iter().all(|node| node.facts().estimated_rows_unavailable));
        assert!(plan.root.act_rows.is_none() && plan.facts().elapsed_ms.is_none());
    }
    if PlanFormat::PostgresJson.is_available() {
        assert_eq!(parse_plan(text).unwrap().engine, Engine::Postgres, "pasted-plan detection is unchanged");
    }
}

#[test]
fn native_context_cannot_relabel_a_serialized_foreign_plan() {
    let portable = portable_plan();
    assert_eq!(parse_plan(&portable).unwrap().engine, Engine::Mssql);
    assert!(parse_native_plan(&portable, Engine::Mysql).is_err());
    assert_eq!(parse_native_plan(&portable, Engine::Mssql).unwrap().engine, Engine::Mssql);
}
