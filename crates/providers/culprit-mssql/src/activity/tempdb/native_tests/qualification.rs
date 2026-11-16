//! Empirical owned-environment costs. This does not issue a production qualification receipt.
use super::*;
use crate::read::measurement::{MeasuredRead, lifecycle_cases, shutdown_state};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Instant,
};

mod fixture;
mod matrix;
use fixture::CostFixture;

const ACTOR_COUNTS: [usize; 3] = [1, 16, 64];
const LOAD_WRITERS: usize = 16;
const RECEIPT_SCHEMA_VERSION: u32 = 1;
static SOURCE_RECEIPT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[tokio::test]
#[ignore = "requires an owned nonce-labelled loopback SQL Server and CULPRIT_NATIVE_COST_RECEIPT_DIR"]
async fn owned_task_space_cost_matrix_preserves_native_work_and_settlement() {
    let directory = receipt_directory();
    for actors in ACTOR_COUNTS {
        let experiment_started = Instant::now();
        let mut fixture = tokio::time::timeout(Duration::from_secs(10), CostFixture::open(actors))
            .await
            .expect("absolute fixture admission deadline")
            .expect("owned fixture admission");
        let run = tokio::time::timeout(Duration::from_secs(35),AssertUnwindSafe(async {
            fixture.initialize().await?;
            let actor_started=Instant::now();
            fixture.start_actors(actors).await?;
            save(&directory,&format!("actors-{actors}-setup"),&json!({
                "actors":actors,
                "schema_setup_client_elapsed_ms":fixture.setup_elapsed_ms,
                "schema_setup_batches":2,
                "actor_setup_client_elapsed_ms":actor_started.elapsed().as_secs_f64()*1000.0,
                "actor_identity_reads":actors,
                "readiness_poll_queries":fixture.readiness_queries,
                "outside_source_measurement":true,
                "actor_input_rows":64,
                "actor_input_value_bytes":2000,
                "actor_native_wait_limit_seconds":120,
                "user_database_data_limit_mib":64,
                "user_database_log_limit_mib":64,
            }))?;
            for load in [false, true] {
                let warmup = capture(&fixture).await?;
                save(&directory, &format!("actors-{actors}-load-{load}-warmup"), &json!({"schema_version":RECEIPT_SCHEMA_VERSION,"case":"unchanged-source-warmup","source_work":warmup}))?;
                let rounds = matrix::abba(&fixture, load).await?;
                save(&directory, &format!("actors-{actors}-load-{load}"), &json!({
                    "schema_version":RECEIPT_SCHEMA_VERSION,
                    "recipe":recipe(),
                    "actors":actors,
                    "load_writers":if load {LOAD_WRITERS}else{0},
                    "round_order":["A-no-source","B-source","B-source","A-no-source"],
                    "rounds":rounds,
                    "cache_state":"uncontrolled; repeatable warm cases only",
                    "cold_qualification":"unsupported-in-this-run",
                    "production_admission":"unqualified",
                    "component":{"name":"culprit-mssql","version":env!("CARGO_PKG_VERSION")},
                    "native_cpu_io_cost_qualified":false,
                }))?;
            }
            let limited = capture_with_budget(&fixture, CollectionBudget { max_snapshot_rows:1,page_rows:1,..crate::task_space::bounded_budget(&CollectionBudget::default()) }).await?;
            save(&directory, &format!("actors-{actors}-row-cap"), &json!({"case":"strict-caller-row-cap","source_work":limited,"native_cpu_io_cost_qualified":false}))?;
            if actors == *ACTOR_COUNTS.last().unwrap() {
                let lifecycle = lifecycle_cases(fixture.pool.clone(), fixture.port, &fixture.password, &fixture.database).await?;
                save(&directory, "lifecycle", &json!({"schema_version":RECEIPT_SCHEMA_VERSION,"cases":lifecycle,"production_admission":"unqualified"}))?;
            }
            Ok::<_, culprit_core::Error>(())
        }).catch_unwind()).await;
        let cleanup_started = Instant::now();
        let cleanup = tokio::time::timeout(Duration::from_secs(8), fixture.close()).await;
        save(
            &directory,
            &format!("actors-{actors}-cleanup"),
            &json!({
                "client_elapsed_ms":cleanup_started.elapsed().as_secs_f64()*1000.0,
                "completed":cleanup.as_ref().is_ok_and(|result|result.is_ok()),
                "outside_source_measurement":true,
                "full_cardinality_experiment_client_elapsed_ms":experiment_started.elapsed().as_secs_f64()*1000.0,
            }),
        )
        .expect("cleanup receipt");
        match run {
            Ok(Ok(result)) => result.expect("empirical source measurement completed"),
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => panic!("absolute 35-second cardinality scenario deadline"),
        }
        cleanup.expect("absolute fixture cleanup deadline").expect("owned actors and nonce database removed");
    }
}

async fn capture(fixture: &CostFixture) -> culprit_core::Result<Value> {
    capture_with_budget(fixture, crate::task_space::bounded_budget(&CollectionBudget::default())).await
}

async fn capture_with_budget(fixture: &CostFixture, budget: CollectionBudget) -> culprit_core::Result<Value> {
    let full_started = Instant::now();
    let io =
        MeasuredRead::open(fixture.pool.clone(), fixture.port, &fixture.password, &fixture.database, budget.clone())
            .await?;
    let scope = io.scope().clone();
    let mut reader = ActivityReader::new(
        io,
        ReadOptions {
            budget: budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        },
        "127.0.0.1".into(),
        fixture.port,
    );
    let source_started = Instant::now();
    let captured = reader.capture_task_space().await;
    let verification = match &captured {
        Ok(captured) => reader.verify_task_space_context(&captured.context).await,
        Err(_) => Ok(false),
    };
    let source_elapsed_ms = source_started.elapsed().as_secs_f64() * 1000.0;
    let shutdown = reader.close().await;
    let accounting = reader.io.report();
    let outcome = json!({
        "scope":scope,
        "recipe":recipe(),
        "tested_budget":budget,
        "source_profile_including_preparation_and_observer_client_elapsed_ms":full_started.elapsed().as_secs_f64()*1000.0,
        "source_with_observer_client_elapsed_ms":source_elapsed_ms,
        "source_queries":reader.query_count(),
        "accounting":accounting,
        "shutdown":shutdown_state(shutdown),
        "error":captured.as_ref().err().map(|error|error.code),
        "verified":verification.as_ref().ok(),
        "snapshot":captured.as_ref().ok().map(|capture|&capture.snapshot),
        "captured_rows":captured.as_ref().ok().map(|capture|capture.snapshot.observations.len()),
        "limited":captured.as_ref().ok().map(|capture|capture.limited),
        "context_complete":captured.as_ref().ok().map(|capture|capture.context_complete),
        "native_cpu_io_cost_qualified":false,
    });
    let serial = SOURCE_RECEIPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(serial < 32);
    save(&receipt_directory(), &format!("source-work-{serial:02}"), &outcome)?;
    match &captured {
        Err(error) => return Err(error.clone()),
        Ok(captured) => {
            assert!(captured.snapshot.is_valid());
            assert!(captured.snapshot.observations.iter().all(|row| row.is_valid()));
            assert!(captured.context.database == *scope.target.database().expect("database scope"));
            let observed = captured
                .snapshot
                .observations
                .iter()
                .filter(|row| fixture.actor_ids.contains(&row.task.session_id))
                .map(|row| &row.task.session_id)
                .collect::<BTreeSet<_>>();
            if !captured.limited {
                assert_eq!(
                    observed.len(),
                    fixture.actor_ids.len(),
                    "all owned allocating actors must be observed before claiming the cardinality case"
                );
            }
            let strict_row_cap = budget.max_snapshot_rows < crate::activity::tempdb::MAX_NATIVE_ROWS as u32;
            if strict_row_cap {
                assert!(captured.limited, "the strict caller cap must retain its native overflow witness");
                assert!(
                    (1..=7).contains(&reader.query_count()),
                    "a limited capture may short-circuit but cannot exceed the unchanged source budget"
                );
            } else {
                assert!(!captured.limited, "a capped population cannot qualify the complete cardinality case");
                assert_eq!(reader.query_count(), 7, "the complete measured source keeps its original seven phases");
            }
            assert!(verification?);
            assert_eq!(shutdown, SourceShutdown::Quiescent);
            assert!(
                accounting["measurement_complete"].as_bool().unwrap_or(false),
                "missing native counters cannot pass a cost sample: {accounting}"
            );
        }
    }
    Ok(outcome)
}

fn recipe() -> RecipeDescriptor {
    crate::telemetry::recipes()
        .into_iter()
        .find(|recipe| recipe.id == "mssql.task-space")
        .expect("the source-owned recipe")
}
fn receipt_directory() -> PathBuf {
    let path =
        PathBuf::from(std::env::var("CULPRIT_NATIVE_COST_RECEIPT_DIR").expect("explicit private receipt directory"));
    assert!(path.is_absolute() && path.is_dir() && !path.symlink_metadata().unwrap().file_type().is_symlink());
    path
}
fn save(directory: &Path, name: &str, value: &Value) -> culprit_core::Result<()> {
    use std::io::Write;
    let path = directory.join(format!("{name}.json"));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path).map_err(|_| crate::read::invalid_data())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| crate::read::invalid_data())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(crate::read::invalid_data());
    }
    output.write_all(&bytes).and_then(|_| output.sync_all()).map_err(|_| crate::read::invalid_data())
}
