//! One owned alternative-context control over the same original scalar episode. A native statistics
//! refresh is observed separately; it never turns same-input work differences into an exclusive cause.
use super::*;
use culprit_core::{plan_compare::NativeRuntimeInputRelation, telemetry::MssqlStatisticsSnapshot};
use serde_json::json;

// The owned table has two indexed columns and one other column: at most three native statistics.
const MAX_FIXTURE_STATISTICS: usize = 3;

async fn current_statistics(probe: &mut Probe) -> MssqlStatisticsSnapshot {
    // Use the existing native SQL and decoder, on the same original actor; no collector or session is added.
    let sql = format!("/* culprit */ {}", crate::statistics::select_sql());
    let mut query = tiberius::Query::new(sql);
    query.bind((MAX_FIXTURE_STATISTICS + 1) as i32);
    let sets = query.query(&mut probe.application).await.unwrap().into_results().await.unwrap();
    let [rows] = sets.as_slice() else { panic!("one original native statistics result") };
    assert!(rows.len() <= MAX_FIXTURE_STATISTICS, "the exact statistic must be inside the complete owned roster");
    let matching = rows
        .iter()
        .map(crate::value::Rec::from_row)
        .map(|row| crate::statistics::decode(&row).unwrap())
        .filter(|row| row.schema == "dbo" && row.table_name == "scalar_rows" && row.name == "ix_scalar_bucket")
        .collect::<Vec<_>>();
    let [row] = matching.as_slice() else { panic!("one native bucket statistic") };
    assert!(row.properties_available && row.last_updated_without_offset.is_some());
    assert_eq!(row.rows.as_deref(), Some("32768"));
    assert_eq!(row.rows_sampled.as_deref(), Some("32768"));
    row.clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsedStatistic {
    database: String,
    schema: String,
    table: String,
    statistic: String,
    updated_without_offset: String,
}
fn used_statistic(plan: &Plan) -> UsedStatistic {
    // The caller first established the pair through the original-native adapter, which requires one
    // unambiguous native statement/QueryPlan. Do not introduce a second statement-selection policy.
    culprit_core::xml::read(plan.raw.as_deref().unwrap(), |document| {
        let document = document.unwrap();
        let matching = document
            .descendants()
            .filter(|node| node.tag_name().name() == "StatisticsInfo")
            .filter(|node| {
                node.attribute("Schema") == Some("[dbo]")
                    && node.attribute("Table") == Some("[scalar_rows]")
                    && node.attribute("Statistics") == Some("[ix_scalar_bucket]")
            })
            .collect::<Vec<_>>();
        let [native] = matching.as_slice() else { panic!("one used native bucket statistic") };
        UsedStatistic {
            database: native.attribute("Database").unwrap().into(),
            schema: native.attribute("Schema").unwrap().into(),
            table: native.attribute("Table").unwrap().into(),
            statistic: native.attribute("Statistics").unwrap().into(),
            updated_without_offset: crate::sql_time::native_time(native.attribute("LastUpdate").unwrap()).unwrap(),
        }
    })
}
fn native_instant(value: &str) -> chrono::NaiveDateTime {
    // These are native offset-free statistic timestamps, never source UTC observation bounds. Exact
    // parsed instants may differ in fractional formatting; no rounding/tolerance or UTC is invented.
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").unwrap()
}

async fn changed_statistics(fixture: &OwnedSourceFixture, database: &str) {
    let directory = crate::test_support::receipt_directory();
    let mut probe = Probe::counted(fixture, database, IndexCoverage::LookupRequired).await;
    let initial = probe.record(2, CacheExpectation::New).await;
    let original = probe.record_native(1, CacheExpectation::Reused(&initial.context)).await;
    assert_context(&original, ParameterValues::Different);
    let before = current_statistics(&mut probe).await;
    assert_eq!(before.table.database_id, original.context.database_id);
    let mut retained = Original::capture(&probe, &original).await;
    verify_actor_present(fixture, database, &probe.actor()).await;
    // Refresh only this nonce-owned statistic, then explicitly prepare the controlled counterpart.
    // FULLSCAN alone need not replace a cache incarnation. The separately paid sp_recompile is not
    // inferred from a handle or native cache flag; neither intervention isolates the work difference.
    // Application rows, statement text, index definitions and runtime inputs remain unchanged.
    batch(&mut probe.application, "UPDATE STATISTICS dbo.scalar_rows ix_scalar_bucket WITH FULLSCAN").await;
    batch(&mut probe.application, "EXEC sys.sp_recompile N'dbo.scalar_probe'").await;
    let counterpart = probe.record_native(1, CacheExpectation::New).await;
    let after = current_statistics(&mut probe).await;
    assert_eq!(before.table, after.table);
    assert_eq!(before.statistics_id, after.statistics_id);
    assert_eq!(before.name, after.name);
    assert_eq!(original.context.database_id, counterpart.context.database_id);
    assert_ne!(
        native_instant(before.last_updated_without_offset.as_ref().unwrap()),
        native_instant(after.last_updated_without_offset.as_ref().unwrap())
    );
    assert_eq!(original.result, counterpart.result);
    let pair = crate::plan_xml::runtime_input_pair(&original.plan, &counterpart.plan);
    assert_eq!(pair.relation(), NativeRuntimeInputRelation::Same);
    let used_before = used_statistic(&original.plan);
    let used_after = used_statistic(&counterpart.plan);
    assert_eq!(
        (&used_before.database, &used_before.schema, &used_before.table, &used_before.statistic),
        (&used_after.database, &used_after.schema, &used_after.table, &used_after.statistic)
    );
    // Showplan and the current statistics DMF expose independent timestamps. Keep both original
    // surfaces and require each to change; neither an exact match nor one statistics-blob version
    // follows from matching database/table/statistic labels. Do not round them into agreement.
    assert_ne!(native_instant(&used_before.updated_without_offset), native_instant(&used_after.updated_without_offset));
    let lookup = original.operator("Key Lookup");
    let scan = counterpart.operator("Clustered Index Scan");
    let lookup_reads = original.logical_reads("Key Lookup");
    let scan_reads = counterpart.logical_reads("Clustered Index Scan");
    assert!(lookup_reads >= 1000.0 && scan_reads < lookup_reads);
    let counterpart_name = "After statistics and explicit recompile";
    let comparison =
        culprit_core::plan_compare::compare_native_pair(pair, "Original", counterpart_name, &Default::default());
    let limit = culprit_core::t!("core.plan_compare.scalar_reuse.limit");
    let scenario = comparison
        .scenarios
        .iter()
        .find(|scenario| {
            scenario.kind == culprit_core::ScenarioKind::Parameters
                && scenario.a == Some(lookup.id)
                && scenario.b == Some(scan.id)
        })
        .expect("same-input measured work survives the observed statistics refresh");
    assert_eq!((scenario.a, scenario.b), (Some(lookup.id), Some(scan.id)));
    assert_eq!(
        scenario.detail.as_deref(),
        Some(
            format!(
                "{} {}",
                culprit_core::t!(
                    "core.plan_compare.scalar_reuse.detail",
                    reused = "Original",
                    scan = counterpart_name,
                    executions = culprit_core::format::count(lookup.act_executions.unwrap()),
                    lookup_reads = culprit_core::format::count(lookup_reads),
                    scan_reads = culprit_core::format::count(scan_reads)
                ),
                limit
            )
            .as_str()
        )
    );
    let actor = probe.actor();
    batch(&mut probe.application, "DROP PROCEDURE dbo.scalar_probe").await;
    probe.application.close().await.unwrap();
    retained.verify_after_actor_disappears(fixture, database, actor).await;
    let replayed = retained.explain();
    assert_eq!(
        crate::plan_xml::runtime_input_pair(&replayed, &counterpart.plan).relation(),
        NativeRuntimeInputRelation::Unavailable,
        "recovered/redacted facts cannot reconstruct the original runtime tuple"
    );
    let record = &retained.capture.records[retained.position];
    let TelemetryRecord::ObjectDefinition(record) = &record.record else { panic!("original retained source") };
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &record.fact else { panic!("original retained body") };
    let DefinitionPayload::Available { text, .. } = &native.batch_plan.payload else { panic!("original native body") };
    assert!(text.contains("OptimizerStatsUsage"));
    crate::test_support::save_cost_receipt(
        &directory,
        "scalar-original-statistics-control",
        &json!({
            "successful":true,
            "original_manifest":retained.capture.manifest,
            "original_records":retained.capture.records,
            "before_statistics":before,"after_statistics":after,
            "used_statistic_before_last_update_without_offset":used_before.updated_without_offset,
            "used_statistic_after_last_update_without_offset":used_after.updated_without_offset,
            "comparison":comparison,
            "lookup_node":lookup.id,"lookup_logical_reads":lookup_reads,
            "scan_node":scan.id,"scan_logical_reads":scan_reads,
            "statistics_refresh_dispatches":1,"statistics_metadata_oracle_dispatches":2,
            "explicit_procedure_recompile_dispatches":1,
            "original_cache_identity":&original.context.identity,
            "counterpart_cache_identity":&counterpart.context.identity,
            "observed_cache_identity_changed":original.context.identity.key().unwrap()!=counterpart.context.identity.key().unwrap(),
            "original_native_retrieved_from_cache":original.plan.facts().parameter_context.as_ref().and_then(|context|context.retrieved_from_cache),
            "counterpart_native_retrieved_from_cache":counterpart.plan.facts().parameter_context.as_ref().and_then(|context|context.retrieved_from_cache),
            "owned_application_executions":3,"owned_actual_plan_capture_dispatches":9,
            "live_actor_generation_verification_dispatches":2,
            "live_actor_generation_verifier_session_setup_dispatches":1,
            "fixture_sql_dispatch_upper_bound_including_setup_and_original_source":59,
            "histogram_change_or_exclusive_parameter_cause_established":false,
            "statistics_refresh_alone_isolated_as_cause":false,
            "fresh_compilation_inferred_from_cache_flag":false,
            "cross_surface_statistics_timestamp_equality_established":false,
            "source_cost_qualified":false,
        }),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned scalar fixture and private receipt directory; statistics refresh is owned setup only"]
async fn owned_changed_statistics_keep_original_work_and_limits_after_recovery_without_exclusive_parameter_blame() {
    super::super::super::fixture::run("scalar_stats", changed_statistics).await;
}
