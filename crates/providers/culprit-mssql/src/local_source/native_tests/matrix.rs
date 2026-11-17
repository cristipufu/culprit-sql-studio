use super::*;
use crate::{
    read::{integer, optional_counter, text},
    test_support::assert_no_execution_grant,
};
use std::path::Path;

pub(super) async fn run(
    fixture: &OwnedSourceFixture,
    population: &mut population::IdleSessions,
    observer: &mut ReadSession,
    directory: &Path,
) -> Result<()> {
    for stage in ["baseline", "idle-32", "released-baseline"] {
        match stage {
            "idle-32" => population.populate(fixture, observer).await?,
            "released-baseline" => {
                population.release();
                population.confirm_released(observer).await?;
            }
            _ => population.confirm_idle(observer).await?,
        }
        for (name, detail) in [("authority", Detail::Authority), ("page-lookup", Detail::PageLookup)] {
            capture(fixture, population, detail, directory, &format!("context-{stage}-{name}")).await?;
        }
        if stage == "idle-32" {
            for (name, detail) in [("authority", Detail::Authority), ("page-lookup", Detail::PageLookup)] {
                let receipt = grant_control(fixture, detail).await?;
                save_cost_receipt(directory, &format!("context-actual-plan-{name}"), &receipt)?;
            }
            population.confirm_idle(observer).await?;
        }
    }
    Ok(())
}

async fn capture(
    fixture: &OwnedSourceFixture,
    population: &population::IdleSessions,
    detail: Detail,
    directory: &Path,
    name: &str,
) -> Result<()> {
    let full_started = Instant::now();
    let mut reader = measured(fixture, source_budget()).await?;
    let experiment = experiment(reader.scope());
    let source_started = Instant::now();
    let result = read_pair(&mut reader, fixture, detail).await;
    let source_elapsed_ms = source_started.elapsed().as_secs_f64() * 1000.0;
    // Always settle before inspecting/asserting the captured result or writing its receipt.
    let shutdown = reader.close().await;
    let quiet = reader.is_quiet().await;
    let metrics = reader.read_metrics();
    let accounting = reader.report();
    let receipt = json!({
        "experiment":experiment,
        "tested_budget":source_budget(),
        "unrelated_idle_master_sessions":population.len(),
        "projection_template_id":digest(query(IdentityDetail::Endpoint,detail).as_bytes()),
        "source_queries":reader.queries(),
        "expected_context_projection_rows_per_read":1,
        "error":result.as_ref().err().map(|error|error.code),
        "shutdown":crate::read::measurement::shutdown_state(shutdown),
        "independently_observed_quiet":quiet.as_ref().ok(),
        "source_with_observer_client_elapsed_ms":source_elapsed_ms,
        "full_profile_including_preparation_and_observer_client_elapsed_ms":full_started.elapsed().as_secs_f64()*1000.0,
        "accounting":accounting,
        "native_cpu_io_cost_qualified":false,
    });
    save_cost_receipt(directory, name, &receipt)?;
    let (context, first, last) = result?;
    if shutdown != SourceShutdown::Quiescent || !quiet? {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    assert_eq!(reader.queries(), SOURCE_QUERIES, "unchanged endpoint authority path is three reads at each bound");
    assert_eq!(context.target.database().unwrap().database, fixture.database);
    assert!(context.has_qualified_local_owner() && context.database_writable.is_some());
    assert_native_row(&first, detail);
    assert_native_row(&last, detail);
    let metrics = metrics.expect("actual native reader accounting");
    assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
    let id = digest(query(IdentityDetail::Endpoint, detail).as_bytes());
    let projection = metrics
        .templates
        .iter()
        .find(|metric| metric.template_id == id)
        .expect("unchanged actual projection was executed");
    assert_eq!(projection.completed_tasks, 2);
    assert_eq!(projection.rows_decoded, "2", "one context row at each native bound, independent of unrelated sessions");
    assert_eq!(projection.failures, 0);
    assert_eq!(metrics.templates.len(), 3, "no workload/population query enters the source window");
    assert_eq!(accounting["measurement_complete"], true, "missing native counters cannot qualify a numeric sample");
    let NativeSessionReadWork::MssqlSession(work) =
        metrics.native_session_work.as_deref().expect("native self-session observations");
    assert_eq!(work.settlement, NativeReadSettlement::Quiescent);
    assert_eq!(work.issue, None);
    let interval = work.interval.as_ref().expect("original native owner interval");
    assert_eq!(interval.session.id, accounting["preparation"]["session_id"].as_i64().unwrap().to_string());
    assert_eq!(
        interval.session.generation,
        crate::sql_time::native_time(accounting["preparation"]["login_time"].as_str().unwrap())?
    );
    assert_eq!(interval.context, context);
    assert_eq!(interval.initial.query_ordinal, 3);
    assert_eq!(interval.latest.as_ref().unwrap().query_ordinal, 6);
    assert!(!population.contains(&interval.session), "an unrelated actor is never the collector's native session");
    Ok(())
}

async fn read_pair(
    reader: &mut MeasuredRead,
    fixture: &OwnedSourceFixture,
    detail: Detail,
) -> Result<(NativeReadContext, Rec, Rec)> {
    let (before, first) = read_initial(reader, "127.0.0.1", fixture.port, IdentityDetail::Endpoint, detail).await?;
    let (after, last) = read_again(reader, "127.0.0.1", fixture.port, &before, detail).await?;
    if !before.is_qualified() || !after.is_qualified() || before.activity.edition == 5 {
        return Err(crate::read::invalid_data());
    }
    Ok((before.completed(&after)?, first, last))
}

fn assert_native_row(row: &Rec, detail: Detail) {
    let fields = [
        "collector_session_id",
        "collector_login_without_offset",
        "collector_cpu_ms",
        "collector_reads",
        "collector_logical_reads",
        "collector_dirty_marks",
        "collector_memory_pages",
    ];
    for field in fields {
        assert_eq!(row.columns().filter(|(name, _)| *name == field).count(), 1, "one native {field} column");
    }
    assert!(integer(row, "collector_session_id").is_ok_and(|id| (1..=i16::MAX as i64).contains(&id)));
    assert!(crate::sql_time::native_time(&text(row, "collector_login_without_offset").unwrap()).is_ok());
    for field in [
        "collector_cpu_ms",
        "collector_reads",
        "collector_logical_reads",
        "collector_dirty_marks",
        "collector_memory_pages",
    ] {
        assert!(
            optional_counter(row, field).unwrap().is_some(),
            "the owned self-session native {field} value must actually exist"
        );
    }
    if matches!(detail, Detail::PageLookup) {
        for field in ["page_info_available", "page_permission"] {
            assert!(integer(row, field).is_ok_and(|value| matches!(value, 0 | 1)));
        }
    }
}

async fn grant_control(fixture: &OwnedSourceFixture, detail: Detail) -> Result<Value> {
    let sql = query(IdentityDetail::Endpoint, detail);
    let sent_sql = crate::statement_origin::monitoring_batch(&sql);
    let started = Instant::now();
    // A panic/timeout in the shared capture helper retires this separately owned actor on unwind. No cleanup
    // SQL uses it afterwards; it neither shares the source owner nor changes the production query.
    let mut actor = connect(
        &config(fixture.port, &fixture.database, fixture.password(), "culprit-context-plan-control"),
        SessionKind::Stats,
    )
    .await?;
    let plans = assert_no_execution_grant(&mut actor, &sent_sql, vec![Parameter::Integer(1)]).await;
    drop(actor);
    let mut runtime_plans = 0;
    let mut maximum_dop: Option<u32> = None;
    let plan_receipts = plans
        .iter()
        .map(|xml| {
            culprit_core::xml::read(xml, |document| {
                let document = document.expect("actual native metadata plan XML");
                let executed = document.descendants().any(|node| {
                    node.has_tag_name("RunTimeCountersPerThread")
                        && node
                            .attribute("ActualExecutions")
                            .and_then(|value| value.parse::<u64>().ok())
                            .is_some_and(|value| value > 0)
                });
                if executed {
                    runtime_plans += 1;
                }
                for plan in document.descendants().filter(|node| node.has_tag_name("QueryPlan")) {
                    if let Some(dop) = plan.attribute("DegreeOfParallelism").and_then(|value| value.parse::<u32>().ok())
                    {
                        maximum_dop = Some(maximum_dop.map_or(dop, |before| before.max(dop)));
                    }
                }
                json!({"native_plan_sha256":digest(xml.as_bytes()),"runtime_execution_observed":executed})
            })
        })
        .collect::<Vec<_>>();
    assert!(runtime_plans > 0, "estimated-only plans do not establish native execution or grant behavior");
    assert!(
        maximum_dop.is_some_and(|dop| dop <= 1),
        "serial execution must be reported by the actual native plan, not an absent-field zero"
    );
    Ok(json!({
        "template_id":digest(sql.as_bytes()),"native_batch_sha256":digest(sent_sql.as_bytes()),"parameters":[1],"plans":plan_receipts,
        "runtime_plans":runtime_plans,"maximum_reported_degree_of_parallelism":maximum_dop,
        "execution_memory_grant_observed":false,
        "client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "outside_source_measurement":true,"instrumentation":"owned actor SET STATISTICS XML only; unchanged template",
        "actor_session_setup_batches":1,"statistics_xml_control_batches":2,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
    }))
}
