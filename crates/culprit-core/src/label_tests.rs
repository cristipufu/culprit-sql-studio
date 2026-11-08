//! The labels core gives people, word for word in English: what a scan can read, what a finding asks of them, the
//! dashboard's tiles and a plan's warnings.

use crate::dashboard::TileKey;
use crate::finding::Actionability;
use crate::model::{Capability, Engine};
use crate::plan::PlanWarning;

#[test]
fn capabilities_are_said_in_each_engines_words() {
    let all = [
        (Capability::QueryStore, "Query Store"),
        (Capability::PlanCache, "Plan cache statistics"),
        (Capability::PgStatStatements, "pg_stat_statements"),
        (Capability::PerformanceSchema, "performance_schema statement digests"),
        (Capability::QueryPlans, "Query plans"),
        (Capability::GenericPlans, "Generic plans (PostgreSQL 12+)"),
        (Capability::TableStats, "Table statistics"),
        (Capability::IndexUsage, "Index usage statistics"),
        (Capability::MissingIndexes, "Missing-index suggestions"),
        (Capability::Statistics, "Optimizer statistics"),
        (Capability::WaitStats, "Wait statistics"),
        (Capability::ResourceUsage, "Resource usage"),
        (Capability::Sessions, "Sessions and blocking"),
        (Capability::Showplan, "Estimated plans (SHOWPLAN)"),
        (Capability::SlowLog, "Slow query log table"),
        (Capability::QueryLog, "Application query log"),
    ];
    for (capability, label) in all {
        assert_eq!(capability.label(), label);
        for engine in [Engine::Mssql, Engine::Yugabyte] {
            if (capability, engine) != (Capability::Showplan, Engine::Yugabyte) {
                assert_eq!(capability.label_for(engine), label, "{engine:?}");
            }
        }
    }
    assert_eq!(Capability::Showplan.label_for(Engine::Yugabyte), "Estimated plans (EXPLAIN)");
    assert_eq!(Capability::Showplan.label_for(Engine::Postgres), "Estimated plans (EXPLAIN)");
    assert_eq!(Capability::Showplan.label_for(Engine::Mysql), "Estimated plans (EXPLAIN)");
    assert_eq!(Capability::Showplan.label_for(Engine::Sqlite), "Estimated plans (EXPLAIN QUERY PLAN)");
    assert_eq!(Capability::QueryStore.label_for(Engine::Sqlite), "Query Store");
}

#[test]
fn what_a_finding_asks_tiles_and_plan_warnings_are_labelled() {
    for (a, label) in [
        (Actionability::NeedsAttention, "Needs attention"),
        (Actionability::OptionalImprovement, "Optional improvement"),
        (Actionability::Information, "Information"),
        (Actionability::History, "History"),
    ] {
        assert_eq!(a.label(), label);
    }
    for (key, label) in [
        (TileKey::DbTime, "Database time"),
        (TileKey::Findings, "Findings"),
        (TileKey::TopQueries, "Top queries"),
        (TileKey::Waits, "Where time goes"),
        (TileKey::Now, "Right now"),
        (TileKey::Health, "Health"),
        (TileKey::Deadlocks, "Deadlocks"),
        (TileKey::Indexes, "Indexes"),
        (TileKey::Plans, "Plans"),
        (TileKey::Statistics, "Query statistics"),
        (TileKey::Storage, "Storage"),
        (TileKey::Connections, "Connections"),
        (TileKey::Replication, "Replication"),
        (TileKey::Failures, "Failing queries"),
    ] {
        assert_eq!(key.label(), label);
    }
    for (w, label) in [
        (PlanWarning::ImplicitConversion, "Implicit conversion"),
        (PlanWarning::Spill, "Spills to disk"),
        (PlanWarning::NoJoinPredicate, "No join predicate"),
        (PlanWarning::MissingStatistics, "Missing statistics"),
        (PlanWarning::MemoryGrant, "Memory grant issue"),
        (PlanWarning::Misestimate, "Row estimate off"),
        (PlanWarning::ResidualPredicate, "Filters after reading"),
    ] {
        assert_eq!(w.label(), label);
    }
}
