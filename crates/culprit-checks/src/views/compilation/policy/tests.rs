use super::*;
use crate::views::compilation::{
    explain,
    tests::{native, observation},
};
use culprit_core::{
    SourceOutcome, SourceState,
    telemetry::{OperationalObservation, PayloadUnavailable},
};

pub(crate) fn measured_recompile() -> OperationalObservation {
    let mut row = observation("12");
    let fact = native(&mut row);
    fact.average_duration_us = 1_000_000.0;
    fact.origin = StatementOrigin::Application;
    fact.statement =
        DefinitionPayload::sql("SELECT id FROM dbo.orders WHERE customer_id = 91234 OPTION (RECOMPILE)", Engine::Mssql);
    row
}

#[test]
fn expensive_explicit_statement_policy_uses_its_own_lifetime_and_keeps_cause_limits() {
    let mut row = measured_recompile();
    native(&mut row).seconds_since_first_compile = 90 * 24 * 60 * 60;
    let case = repeated_recompile(native(&mut row)).unwrap();
    assert_eq!(case.total_milliseconds, 12_000.0);
    assert!(case.statement.contains("OPTION (RECOMPILE)"));
    assert!(!case.statement.contains("91234"));
    let source =
        SourceOutcome { source: row.source.clone(), state: SourceState::Partial, reason: None, reason_message: None };
    let report = explain(&[&row], Some(&source)).unwrap();
    assert!(report.entries[0].title.contains("OPTION (RECOMPILE)"));
    assert!(report.entries[0].explanation.contains(&culprit_core::format::duration(12_000.0)));
    assert!(report.entries[0].investigation.contains("keep the hint if"));
    assert!(report.limitations.contains(&culprit_core::t!("view.compilation.limit.policy")));
    assert!(report.limitations.contains(&culprit_core::t!("view.compilation.limit.source_partial")));
    assert!(!report.limitations.contains(&culprit_core::t!("view.compilation.limit.causes")));
    assert!(report.entries[0].metrics.iter().all(|metric| metric.unit != culprit_core::Unit::Ratio));
}

#[test]
fn single_cheap_or_unbounded_measurements_do_not_become_expensive_repeated_policy() {
    for (count, mean) in [
        ("1", 10_000_000.0),
        ("100000", 100.0),
        ("2", 500_000.0),
        ("12", f64::NAN),
        ("12", f64::INFINITY),
        ("18446744073709551615", f64::MAX),
    ] {
        let mut row = measured_recompile();
        let fact = native(&mut row);
        fact.compilations = count.into();
        fact.average_duration_us = mean;
        assert!(repeated_recompile(fact).is_none(), "{count}/{mean}");
    }
}

#[test]
fn comments_literals_another_statement_and_module_directives_cannot_supply_this_policy() {
    for sql in [
        "SELECT id FROM dbo.orders /* OPTION (RECOMPILE) */",
        "SELECT N'OPTION (RECOMPILE)' FROM dbo.orders",
        "SELECT id FROM dbo.orders; SELECT id FROM dbo.other_orders OPTION (RECOMPILE)",
        "CREATE PROCEDURE dbo.p WITH RECOMPILE AS SELECT id FROM dbo.orders",
        "SELECT id FROM dbo.orders",
    ] {
        let mut row = measured_recompile();
        native(&mut row).statement = DefinitionPayload::sql(sql, Engine::Mssql);
        assert!(repeated_recompile(native(&mut row)).is_none(), "{sql}");
    }
}

#[test]
fn missing_protected_or_non_application_sql_preserves_measurements_without_policy() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::NotCollected,
    ] {
        let mut row = measured_recompile();
        native(&mut row).statement = DefinitionPayload::Unavailable { reason };
        assert!(repeated_recompile(native(&mut row)).is_none());
        let report = explain(&[&row], None).unwrap();
        assert_eq!(report.entries[0].identity, native(&mut row).identity);
        assert!(report.entries[0].metrics.iter().any(|metric| metric.is("view.compilation.metric.count")));
    }
    for origin in [
        StatementOrigin::Unknown,
        StatementOrigin::Culprit,
        StatementOrigin::CulpritMonitoring,
        StatementOrigin::EngineInternal,
    ] {
        let mut row = measured_recompile();
        native(&mut row).origin = origin;
        assert!(repeated_recompile(native(&mut row)).is_none());
    }
}
