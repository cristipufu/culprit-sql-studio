//! One unchanged production read at each population point; no comparison query supplies replacement counters.
use super::*;
use crate::{
    conn::SessionKind,
    read::{integer, optional_text, text, value_bytes},
    test_support::{actual_plans, config, connect},
};

const POPULATION_STAGES: [&str; 3] = ["baseline", "idle-32", "released-baseline"];

pub(super) async fn run(
    fixture: &OwnedSourceFixture,
    population: &mut IdleSessions,
    observer: &mut crate::read::ReadSession,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    for stage in POPULATION_STAGES {
        if cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        match stage {
            "idle-32" => population.populate(fixture, observer).await?,
            "released-baseline" => {
                population.release();
                population.confirm_released(observer).await?;
            }
            _ => population.confirm_idle(observer).await?,
        }
        capture(fixture, population, directory, stage, cancellation).await?;
        if stage == "idle-32" {
            plan_control(fixture, directory, cancellation).await?;
            population.confirm_idle(observer).await?;
        }
    }
    Ok(())
}

async fn capture(
    fixture: &OwnedSourceFixture,
    population: &IdleSessions,
    directory: &Path,
    stage: &str,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let started = Instant::now();
    let mut reader = measured(fixture, source_budget(), cancellation).await?;
    reader.trace_result(&digest(sql().as_bytes()))?;
    let scope = reader.scope().clone();
    let result = until_cancelled(cancellation, read(&mut reader, MAX_COUNTERS)).await;
    let settlement = settle(&mut reader).await;
    let raw_rows = reader.take_result();
    let mut receipt = settlement.receipt();
    receipt["template_id"] = json!(digest(sql().as_bytes()));
    receipt["tested_budget"] = json!(source_budget());
    receipt["source_scope"] = json!(scope);
    receipt["stage"] = json!(stage);
    receipt["unrelated_idle_master_sessions"] = json!(population.len());
    receipt["source_result"] = error_receipt(&result);
    receipt["full_profile_including_preparation_and_observer_client_elapsed_ms"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    if let Ok(read) = &result {
        receipt["native_input_rows"] = json!(read.native_rows);
        receipt["aggregate_observations"] = json!(observations(read.clone())?.len());
        if let Ok(Some(rows)) = &raw_rows {
            receipt["original_native_metadata"] = metadata_receipt(rows)?;
        }
        receipt["native_counter_set"] = json!(read.sample);
    }
    save_cost_receipt(directory, &format!("performance-{stage}"), &receipt)?;
    let read = result?;
    let raw_rows = raw_rows?.ok_or_else(invalid_data)?;
    settlement.confirm_quiet()?;
    assert_eq!(settlement.shutdown, SourceShutdown::Quiescent);
    assert_eq!(settlement.queries, 1, "no summary, rescue or secondary counter query");
    assert!(scope.has_qualified_local_owner() && scope.database_writable.is_some());
    assert_eq!(scope.target.database().unwrap().database, fixture.database);
    let metrics = settlement.metrics.as_ref().expect("actual source metrics");
    verify_input(&read, &raw_rows, metrics, &settlement.accounting);
    assert!(!population.contains(&read_session(&settlement.accounting)?));
    assert_eq!(settlement.accounting["measurement_complete"], true);
    Ok(())
}

fn read_session(accounting: &Value) -> Result<NativeActivityIdentity> {
    let preparation = &accounting["preparation"];
    let id = preparation["session_id"].as_i64().ok_or_else(invalid_data)?;
    let login = preparation["login_time"].as_str().ok_or_else(invalid_data)?;
    Ok(NativeActivityIdentity { id: id.to_string(), generation: crate::sql_time::native_time(login)? })
}

fn verify_input(read: &PerformanceRead, raw_rows: &[crate::value::Rec], metrics: &ReadWorkMetrics, accounting: &Value) {
    assert!(read.sample.is_valid());
    assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
    assert_eq!(metrics.untracked_tasks, 0);
    assert_eq!(read.native_rows, raw_rows.len());
    assert_eq!(read.native_rows, read.sample.counters.len() + read.sample.ratio_gaps.len());
    assert!(read.native_rows <= MAX_COUNTERS);
    let ordinary = SPECS.iter().filter(|spec| spec.family.is_some()).collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 6, "the existing independent six-counter projection");
    for spec in &ordinary {
        let counter = global(&read.sample.counters, spec).expect("owned SQL2022 exposes the existing counter");
        let mut originals = raw_rows.iter().filter(|row| {
            text(row, "native_name").ok().as_deref() == Some(counter.native_name.as_str())
                && text(row, "native_object").ok().as_deref() == Some(counter.native_object.as_str())
                && optional_text(row, "native_instance").ok().flatten().as_deref() == Some("")
        });
        let original = originals.next().expect("the decoded existing counter retains its exact native row");
        assert!(originals.next().is_none());
        assert_eq!(counter.value.integer(), Some(integer(original, "value").unwrap() as u64));
        assert_eq!(
            counter.native_type.as_deref(),
            Some(integer(original, "native_type").unwrap().to_string().as_str())
        );
        assert_eq!(counter.scope, MeasurementScope::Server);
        assert_eq!(counter.reset, ResetMarker::Unknown);
        assert!(application(&read.sample.counters).contains_key(spec.name));
    }
    let exported = observations(read.clone()).unwrap();
    assert_eq!(exported.len(), 1, "native inputs are retained in one canonical aggregate observation");
    assert!(exported[0].is_valid());
    let metrics = metrics.templates.as_slice();
    let [template] = metrics else { panic!("one measured production template") };
    assert_eq!(template.template_id, digest(sql().as_bytes()));
    assert_eq!(template.completed_tasks, 1);
    assert_eq!(template.failures, 0);
    assert_eq!(template.rows_decoded, read.native_rows.to_string());
    let bytes = raw_rows.iter().map(value_bytes).sum::<usize>();
    assert_eq!(template.charged_value_bytes, bytes.to_string());
    assert!(bytes <= SOURCE_VALUE_BYTES as usize);
    assert!(
        raw_rows
            .iter()
            .all(|row| { integer(row, "engine_edition").ok() == accounting["preparation"]["native_edition"].as_i64() })
    );
    assert!(
        raw_rows.iter().all(|row| {
            crate::deployment::NativeDeployment::decode(row).is_ok_and(|profile| profile.conventional())
        })
    );
    assert!(
        read.sample.ratios.iter().all(|ratio| ratio.support == CounterRatioSupport::Unsupported),
        "native RBPEX names on a boxed server cannot authorize Hyperscale ratio interpretation"
    );
}

fn metadata_receipt(raw_rows: &[crate::value::Rec]) -> Result<Value> {
    let first = raw_rows.first().ok_or_else(invalid_data)?;
    let at = text(first, "counter_observed_at")?;
    canonical_time(&at)?;
    let edition = integer(first, "engine_edition")?;
    let database_edition = optional_text(first, "database_edition")?;
    let replica = optional_text(first, "replica_id")?;
    for row in raw_rows {
        if text(row, "counter_observed_at")? != at
            || integer(row, "engine_edition")? != edition
            || optional_text(row, "database_edition")? != database_edition
            || optional_text(row, "replica_id")? != replica
        {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    Ok(json!({
        "native_observed_at":at, "engine_edition":edition,
        "database_edition":database_edition, "replica_id":replica,
        "authority":"columns from each original row of the unchanged fully drained performance statement",
    }))
}

async fn plan_control(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let sql = sql();
    let sent = crate::statement_origin::monitoring_batch(&sql);
    let mut actor = connect(
        &config(fixture.port, &fixture.database, fixture.password(), "culprit-performance-plan-control"),
        SessionKind::Stats,
    )
    .await?;
    // This owned actor is never reused on capture failure. The shared helper drains or fails and retires it.
    let plans = actual_plans(&mut actor, &sent, vec![]).await;
    drop(actor);
    let receipts = plans.iter().map(|xml| plan_receipt(xml)).collect::<Result<Vec<_>>>()?;
    save_cost_receipt(
        directory,
        "performance-actual-plan",
        &json!({
            "template_id":digest(sql.as_bytes()), "native_batch_sha256":digest(sent.as_bytes()),
            "plans":receipts, "source_template":true, "outside_source_measurement":true,
            "instrumentation":"separate owned actor SET STATISTICS XML; unchanged production statement and no native-query hints added",
            "grants":"only actual native MemoryGrantInfo is reported; absent fields remain unknown and sampled absence proves no peak",
            "production_admission":"unqualified", "native_cpu_io_cost_qualified":false,
        }),
    )?;
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    assert!(receipts.iter().any(|receipt| receipt["runtime_observed"] == true));
    Ok(())
}

fn plan_receipt(xml: &str) -> Result<Value> {
    culprit_core::xml::read(xml, |document| {
        let document = document.map_err(|_| invalid_data())?;
        let runtime = document.descendants().any(|node| {
            node.has_tag_name("RunTimeCountersPerThread")
                && node.attribute("ActualExecutions").and_then(|value| value.parse::<u64>().ok()).is_some_and(|n| n > 0)
        });
        let grants = document
            .descendants()
            .filter(|node| node.has_tag_name("MemoryGrantInfo"))
            .map(|node| {
                let fields = ["RequestedMemory", "GrantedMemory", "RequiredMemory", "DesiredMemory", "MaxUsedMemory"];
                let mut values = serde_json::Map::new();
                for field in fields {
                    let value = node
                        .attribute(field)
                        .map(|value| {
                            let number = value.parse::<u64>().map_err(|_| invalid_data())?;
                            if number.to_string() != value {
                                return Err(invalid_data());
                            }
                            Ok(Value::String(value.into()))
                        })
                        .transpose()?;
                    values.insert(field.into(), value.unwrap_or(Value::Null));
                }
                Ok(Value::Object(values))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"native_plan_sha256":digest(xml.as_bytes()), "runtime_observed":runtime, "memory_grant_info":grants}))
    })
}
