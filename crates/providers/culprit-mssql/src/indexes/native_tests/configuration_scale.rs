//! Paid scale observations of the actual application readers; empty disposable catalog objects only.
use super::*;
use crate::{
    read::measurement::{MeasuredRead, assert_no_followup, shutdown_state},
    test_support::{
        measured_read,
        owned_case::{AdmittedRead, finish_case, require_active, until_cancelled},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use serde_json::json;
use std::{future::Future, path::Path, time::Duration};
use tokio::time::Instant;

const SMALL_PARENTS: usize = 2;
const MEDIUM_PARENTS: usize = 32;
const FIXTURE_PARTITIONS: usize = MAX_MSSQL_CONFIGURATION_PARTITIONS + 1;
const MAX_CONTROL_COMMANDS: usize = 2 * MEDIUM_PARENTS + 4;
const CASE_TIMEOUT: Duration = Duration::from_secs(90);

struct Catalog<'a> {
    control: Fixture<'a>,
    tables: Vec<String>,
    function: String,
    scheme: String,
    owns_partitioning: bool,
    control_attempts: usize,
    control_elapsed_ms: f64,
}
impl<'a> Catalog<'a> {
    fn new(owned: &'a OwnedSourceFixture, suffix: &str) -> Self {
        Self {
            control: Fixture { owned, table: String::new(), user: String::new(), owns: false, control_available: true },
            tables: vec![],
            function: format!("culprit_cfg_pf_{suffix}"),
            scheme: format!("culprit_cfg_ps_{suffix}"),
            owns_partitioning: false,
            control_attempts: 0,
            control_elapsed_ms: 0.0,
        }
    }
    async fn command(&mut self, sql: &str, cancellation: Option<&CollectionCancellation>) {
        if let Some(cancellation) = cancellation {
            require_active(cancellation).unwrap();
        }
        assert!(self.control_attempts < MAX_CONTROL_COMMANDS, "fixed fixture command envelope");
        self.control_attempts += 1;
        let started = Instant::now();
        // The original Fixture retains and retires this exact actor on error, panic or timeout.
        Box::pin(self.control.command(sql)).await;
        self.control_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
    }
    async fn initialize(&mut self, cancellation: &CollectionCancellation) {
        require_active(cancellation).unwrap();
        self.control_attempts += 1;
        let started = Instant::now();
        let sql = format!(
            "SELECT /* culprit fixture */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,(SELECT COUNT_BIG(*) FROM sys.tables WHERE is_ms_shipped=0) AS tables,(SELECT COUNT(*) FROM sys.partition_functions WHERE name=N'{}') AS functions,(SELECT COUNT(*) FROM sys.partition_schemes WHERE name=N'{}') AS schemes",
            self.function, self.scheme
        );
        let row =
            Box::pin(self.control.control(async |actor| crate::conn::query_one(actor, &sql).await.unwrap())).await;
        self.control_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        assert!(integer(&row, "major").unwrap() >= 16, "owned SQL2022+ layout");
        for key in ["tables", "functions", "schemes"] {
            assert_eq!(integer(&row, key).unwrap(), 0, "only a genuinely empty nonce catalog is admitted");
        }
        self.owns_partitioning = true; // Retain cleanup ownership before DDL can be polled.
        let boundaries = (1..FIXTURE_PARTITIONS).map(|value| value.to_string()).collect::<Vec<_>>().join(",");
        self.command(&format!("CREATE PARTITION FUNCTION [{}](int) AS RANGE RIGHT FOR VALUES({boundaries}); CREATE PARTITION SCHEME [{}] AS PARTITION [{}] ALL TO ([PRIMARY]);",self.function,self.scheme,self.function),Some(cancellation)).await;
    }
    async fn grow(&mut self, parents: usize, cancellation: &CollectionCancellation) {
        assert!(parents <= MEDIUM_PARENTS && self.tables.len() <= parents);
        while self.tables.len() < parents {
            let table = format!("{}_t{}", self.function, self.tables.len());
            self.tables.push(table.clone()); // Exact nonce name is owned before its first native dispatch.
            self.command(&format!("CREATE TABLE dbo.[{table}](id int NOT NULL,payload int NULL) ON [{}](id); CREATE NONCLUSTERED INDEX ix_owned ON dbo.[{table}](id) WITH(FILLFACTOR=80,PAD_INDEX=ON,ALLOW_ROW_LOCKS=OFF,ALLOW_PAGE_LOCKS=ON,DATA_COMPRESSION=PAGE) ON [{}](id);",self.scheme,self.scheme),Some(cancellation)).await;
        }
    }
    async fn cleanup(&mut self) {
        if !self.control.control_available {
            return;
        } // Supervisor destroys an uncertain disposable owner.
        while let Some(table) = self.tables.last().cloned() {
            self.command(&format!("DROP TABLE IF EXISTS dbo.[{table}]"), None).await;
            self.tables.pop();
        }
        if self.owns_partitioning {
            self.command(
                &format!("DROP PARTITION SCHEME [{}]; DROP PARTITION FUNCTION [{}];", self.scheme, self.function),
                None,
            )
            .await;
            self.owns_partitioning = false;
        }
    }
}

#[derive(Clone, Copy)]
enum Family {
    Indexes,
    Heaps,
}
impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::Indexes => "indexes",
            Self::Heaps => "heaps",
        }
    }
    fn queries(self) -> u32 {
        match self {
            Self::Indexes => 1,
            Self::Heaps => 2,
        }
    }
}

#[tokio::test]
#[ignore = "requires the positively nonce-owned empty SQL2022+ fixture and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; catalog work measurement only, no production-cost approval"]
async fn owned_index_and_heap_partition_projection_records_small_and_medium_catalog_work() {
    let directory = receipt_directory();
    let owned = Box::pin(OwnedSourceFixture::open("culprit-index-configuration-cost")).await.unwrap();
    let suffix = format!("{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let mut catalog = Catalog::new(&owned, &suffix);
    let cancellation = CollectionCancellation::default();
    let (expired, outcome) = finish_case(
        Instant::now() + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(Box::pin(run(&mut catalog, &directory, &cancellation))).catch_unwind(),
    )
    .await;
    if owned.pool.source_shutdown() != SourceShutdown::Quiescent {
        catalog.control.control_available = false;
    }
    let cleanup = AssertUnwindSafe(Box::pin(catalog.cleanup())).catch_unwind().await;
    save_cost_receipt(&directory,"index-configuration-scale-outcome",&json!({
        "measured_source_queries_expected":12,"completed":!expired&&outcome.is_ok()&&cleanup.is_ok(),"case_deadline_reached":expired,
        "source_workload":"two fixed empty catalog sizes; unchanged shipped application queries and native decoder",
        "fixture_tables":MEDIUM_PARENTS,"partitions_per_heap_and_index":FIXTURE_PARTITIONS,
        "control_attempts":catalog.control_attempts,"max_control_attempts":MAX_CONTROL_COMMANDS,
        "control_client_elapsed_ms":catalog.control_elapsed_ms,"control_timeout_ms":COMMAND_TIMEOUT.as_millis(),
        "control_accounting":"each admitted control owns one native identity check plus one command; login/admission and source observer/preparation remain separate",
        "fixture_admission_client_elapsed_ms":owned.admission_elapsed_ms,
        "cleanup_confirmed":catalog.tables.is_empty()&&!catalog.owns_partitioning,
        "supervisor_destruction_required":!catalog.control.control_available,
        "source_shutdown":shutdown_state(owned.pool.source_shutdown()),
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "limits":"these two points do not qualify the ordinary 10,000-parent ceiling; TOP bounds the returned prefix, not internal reads or sorts"
    })).unwrap();
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
    cleanup.expect("all known owned DDL settled and cleaned up");
    assert!(!expired, "deadline cancels admission and awaits the same native owner");
    assert!(
        catalog.tables.is_empty() && !catalog.owns_partitioning,
        "uncertain cleanup is not a successful experiment"
    );
}
async fn run(catalog: &mut Catalog<'_>, directory: &Path, cancellation: &CollectionCancellation) {
    Box::pin(catalog.initialize(cancellation)).await;
    for parents in [SMALL_PARENTS, MEDIUM_PARENTS] {
        Box::pin(catalog.grow(parents, cancellation)).await;
        for native_statistics in [false, true] {
            for family in [Family::Indexes, Family::Heaps] {
                Box::pin(observe(catalog, directory, parents, family, native_statistics, cancellation)).await;
            }
        }
    }
}
async fn observe(
    catalog: &mut Catalog<'_>,
    directory: &Path,
    parents: usize,
    family: Family,
    native_statistics: bool,
    cancellation: &CollectionCancellation,
) {
    require_active(cancellation).unwrap();
    let budget = CollectionBudget { max_queries: family.queries(), ..Default::default() };
    let owned = catalog.control.owned;
    let mut reader = Box::new(
        Box::pin(measured_read(owned.pool.clone(), owned.port, owned.password(), &owned.database, budget.clone()))
            .await
            .unwrap(),
    );
    let scope = reader.scope().clone();
    let started = Instant::now();
    let work = AssertUnwindSafe(until_cancelled(
        cancellation,
        Box::pin(read(&mut reader, family, native_statistics, cancellation)),
    ))
    .catch_unwind()
    .await;
    let shutdown = Box::pin(reader.close()).await;
    let quiet = Box::pin(reader.is_quiet()).await;
    let metrics = reader.read_metrics();
    let accounting = reader.report();
    Box::pin(assert_no_followup(&mut reader)).await;
    if shutdown != SourceShutdown::Quiescent || quiet.as_ref().ok() != Some(&true) {
        catalog.control.control_available = false;
    }
    let facts = match &work {
        Ok(Ok(facts)) => Some(facts),
        _ => None,
    };
    let receipt = json!({"family":family.name(),"native_statistics":native_statistics,"fixture_parents":parents,"fixture_partitions_per_parent":FIXTURE_PARTITIONS,
        "maximum_projected_partition_rows":parents*FIXTURE_PARTITIONS,"retained_partition_rows":facts.map(|facts|facts.iter().map(|fact|fact.partitions.len()).sum::<usize>()),
        "decoded_parents":facts.map(Vec::len),"source_scope":scope,"budget":budget,"source_queries":reader.queries(),
        "source_read_metrics":metrics,"accounting":accounting,"source_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
        "source_error":match &work { Ok(Err(error))=>Some(error.code),_=>None },"source_panic":work.is_err(),
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "interpretation":"actual original reader work including the per-parent correlated partition/JSON projection; not only dispatch count; no business rows or physical health claim"});
    save_cost_receipt(
        directory,
        &format!("index-configuration-{}-{native_statistics}-{parents}", family.name()),
        &receipt,
    )
    .unwrap();
    let facts = match work {
        Err(panic) => resume_unwind(panic),
        Ok(result) => result.expect("actual unchanged application reader"),
    };
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(quiet.unwrap());
    assert_eq!(reader.queries(), family.queries(), "no rescue or measurement SQL on the source owner");
    assert!(scope.has_qualified_local_owner());
    assert_eq!(scope.target.database().unwrap().database, owned.database);
    assert_eq!(accounting["measurement_complete"], true);
    assert!(metrics.as_ref().is_some_and(|metrics| metrics.is_valid() && !metrics.has_unconfirmed_work()));
    verify_facts(&facts, &catalog.tables, family);
    assert_eq!(facts.len(), parents);
}
async fn read(
    reader: &mut MeasuredRead,
    family: Family,
    native_statistics: bool,
    cancellation: &CollectionCancellation,
) -> Result<Vec<MssqlIndexConfiguration>> {
    let mut admitted = AdmittedRead::new(reader, cancellation);
    match family {
        Family::Indexes => Box::pin(application(&mut admitted, native_statistics))
            .await?
            .table_indexes
            .into_iter()
            .map(|index| index.mssql_configuration.map(|configuration| *configuration).ok_or_else(invalid_data))
            .collect(),
        Family::Heaps => Box::pin(crate::tables::application(&mut admitted, native_statistics))
            .await?
            .tables
            .into_iter()
            .map(|table| table.mssql_heap_configuration.map(|configuration| *configuration).ok_or_else(invalid_data))
            .collect(),
    }
}
fn verify_facts(facts: &[MssqlIndexConfiguration], tables: &[String], family: Family) {
    assert_eq!(facts.len(), tables.len());
    for table in tables {
        let matches =
            facts.iter().filter(|fact| fact.parent_schema == "dbo" && &fact.parent_name == table).collect::<Vec<_>>();
        let [fact] = matches.as_slice() else {
            panic!("one exact owned native parent per table");
        };
        assert!(fact.is_valid());
        assert_eq!(fact.partitions.len(), MAX_MSSQL_CONFIGURATION_PARTITIONS);
        assert!(!fact.partitions_complete, "the actual sixty-fifth native partition is lookahead, never complete");
        assert!(
            fact.partitions
                .iter()
                .enumerate()
                .all(|(ordinal, partition)| partition.partition_number == ordinal as u32 + 1)
        );
        match family {
            Family::Indexes => {
                assert_eq!(fact.name.as_deref(), Some("ix_owned"));
                assert!(fact.index_id > 0);
                assert_eq!(fact.options.as_ref().unwrap().fill_factor, Some(80));
                assert!(fact.partitions.iter().all(|partition| partition.data_compression == Some(2)));
            }
            Family::Heaps => {
                assert_eq!(fact.index_id, 0);
                assert!(fact.name.is_none() && fact.options.is_none());
                assert!(fact.partitions.iter().all(|partition| partition.data_compression == Some(0)));
            }
        }
    }
}

#[test]
fn paid_scale_future_types_keep_owned_native_phases_off_the_inline_stack() {
    fn check<A, F: Future>(phase: &str, _factory: impl FnOnce(A) -> F) {
        let bytes = std::mem::size_of::<F>();
        assert!(bytes <= 4096, "{phase} retains {bytes} inline bytes; native phases must stay on the heap");
    }
    check(
        "whole case",
        |(catalog, directory, cancellation): (
            &'static mut Catalog<'static>,
            &'static Path,
            &'static CollectionCancellation,
        )| run(catalog, directory, cancellation),
    );
    check(
        "one measured reader",
        |(catalog, directory, cancellation): (
            &'static mut Catalog<'static>,
            &'static Path,
            &'static CollectionCancellation,
        )| observe(catalog, directory, MEDIUM_PARENTS, Family::Indexes, true, cancellation),
    );
    check("source dispatch", |(reader, cancellation): (&'static mut MeasuredRead, &'static CollectionCancellation)| {
        read(reader, Family::Heaps, true, cancellation)
    });
}
