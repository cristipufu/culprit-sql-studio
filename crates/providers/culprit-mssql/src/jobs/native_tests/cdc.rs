//! Explicit Agent-owned fixture only. Production CDC collection never enables CDC, invokes scans or writes data.
use crate::{
    conn::{MsClient, SessionKind},
    read::{integer, invalid_data},
    test_support::{
        OwnedSourceFixture, config, connect,
        owned_captures::{Captured, OwnedCaptures},
        owned_case::finish_case,
        receipt_directory, save_cost_receipt, verify_owned_source,
    },
    value::Rec,
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::*};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::time::Instant;

mod capture_job;
mod control;
mod stack_tests;
use control::{Control, ControlFailure};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
// Creating the first capture instance also creates native Agent jobs. The observed timeout after 5s is
// fixture setup work; all ordinary control commands and source deadlines retain their existing bounds.
const FIRST_TABLE_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const CASE_TIMEOUT: Duration = Duration::from_secs(90);
const CONTROL_ROWS: usize = 4;
const CONTROL_BYTES: usize = 8 * 1024;
const SCAN: &str =
    "/* culprit fixture scan only */ EXEC sys.sp_cdc_scan @maxtrans=8,@maxscans=4,@continuous=0,@pollinginterval=0;";
const PRIVATE_PAYLOAD: i64 = 2_147_483_648;

fn control_config(port: u16, database: &str, password: &str) -> tiberius::Config {
    let mut options = config(port, database, password, "owned-cdc-control");
    // This socket-wide per-response ceiling must permit the longest explicit setup deadline.
    // Each ordinary operation still has its 5s outer deadline in Control::run.
    options.command_timeout(Some(FIRST_TABLE_SETUP_TIMEOUT));
    options
}

struct Fixture {
    owner: OwnedSourceFixture,
    cancellation: CollectionCancellation,
    cleaning: bool,
    client: Option<Box<MsClient>>,
    control: Control,
    control_queries: u32,
    control_connect_attempts: u32,
    control_session_setups: u32,
    capture_job: Option<Value>,
    owns_cdc: bool,
    owns_table: bool,
    owns_user: bool,
    stages: Vec<Value>,
}
impl Fixture {
    async fn open_control(&mut self) -> Result<()> {
        Box::pin(async {
            self.control.require_available()?;
            if self.client.is_some() {
                return Ok(());
            }
            let options = control_config(self.owner.port, &self.owner.database, self.owner.password());
            self.control_connect_attempts += 1;
            let client = self
                .control
                .run(
                    "connect-control",
                    Box::pin(async {
                        Box::pin(connect(&options, SessionKind::User)).await.map(Box::new).map_err(ControlFailure::from)
                    }),
                )
                .await?;
            self.control_session_setups += 1;
            self.client = Some(client);
            self.control_queries += 1;
            let client = self.client.as_mut().ok_or_else(invalid_data)?;
            let result = self
                .control
                .run(
                    "verify-owned-control",
                    Box::pin(async { Box::pin(verify_owned_source(client)).await.map_err(ControlFailure::from) }),
                )
                .await;
            if result.is_err() {
                // Retirement is recorded separately and cannot replace the original verification failure.
                let _ = self.retire().await;
            }
            result
        })
        .await
    }
    async fn retire(&mut self) -> Result<()> {
        Box::pin(async {
            if let Some(client) = self.client.take() {
                self.control.retire(Box::pin((*client).close())).await?;
            }
            Ok(())
        })
        .await
    }
    async fn query(&mut self, stage: &'static str, sql: &str) -> Result<Vec<Rec>> {
        Box::pin(self.query_with_timeout(stage, CONTROL_TIMEOUT, sql)).await
    }
    async fn query_with_timeout(&mut self, stage: &'static str, timeout: Duration, sql: &str) -> Result<Vec<Rec>> {
        Box::pin(self.query_checked(stage, timeout, sql, Ok)).await
    }
    async fn query_checked<T>(
        &mut self,
        stage: &'static str,
        timeout: Duration,
        sql: &str,
        check: impl FnOnce(Vec<Rec>) -> Result<T>,
    ) -> Result<T> {
        Box::pin(async {
            self.control.require_available()?;
            if !self.cleaning && self.cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            self.open_control().await?;
            self.control_queries += 1;
            let client = self.client.as_mut().ok_or_else(invalid_data)?;
            let result = self
                .control
                .run_with_timeout(
                    stage,
                    timeout,
                    Box::pin(async {
                        let response = Box::pin(client.simple_query(sql)).await.map_err(ControlFailure::from)?;
                        let results = Box::pin(response.into_results()).await.map_err(ControlFailure::from)?;
                        let rows = results.into_iter().flatten().map(|row| Rec::from_row(&row)).collect::<Vec<_>>();
                        if rows.len() > CONTROL_ROWS
                            || rows.iter().map(crate::read::value_bytes).sum::<usize>() > CONTROL_BYTES
                        {
                            return Err(ControlFailure::from(invalid_data()));
                        }
                        check(rows).map_err(ControlFailure::from)
                    }),
                )
                .await;
            if result.is_err() {
                // A failed/timed-out result leaves the command fenced even when its socket closes successfully.
                let _ = self.retire().await;
            }
            result
        })
        .await
    }
    async fn exercise(&mut self, cancel: &CollectionCancellation) -> Result<Value> {
        self.open_control().await?;
        let rows=self.query("preconditions", "SELECT /* culprit fixture preconditions */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,DB_ID() AS database_id,DB_NAME() AS current_database,CASE WHEN EXISTS(SELECT 1 FROM sys.dm_server_services WHERE servicename LIKE N'SQL Server Agent (%' AND status=4) THEN 1 ELSE 0 END AS agent_running,OBJECT_ID(N'dbo.culprit_cdc_fixture') AS prior_table,USER_ID(N'culprit_cdc_denied') AS prior_user").await?;
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        assert!(integer(row, "major")? >= 16, "this exact native permission control requires SQL Server2022+");
        assert!(integer(row, "database_id")? > 4);
        assert_eq!(crate::read::text(row, "current_database")?, self.owner.database);
        assert_eq!(integer(row, "agent_running")?, 1, "use the existing explicit mssql-agent owner");
        assert!(crate::read::optional_integer(row, "prior_table")?.is_none());
        assert!(crate::read::optional_integer(row, "prior_user")?.is_none());
        let mut captures = OwnedCaptures::from_pool(self.owner.pool.clone(), self.owner.port)?;
        let disabled = self.capture(&mut captures, "original-disabled", cancel).await?;
        assert_eq!(status(&disabled).enabled, Some(false), "never disable another fixture's preexisting CDC");
        self.owns_cdc = true;
        self.query("enable-database", "/* culprit fixture CDC setup only */ EXEC sys.sp_cdc_enable_db;").await?;
        self.owns_table = true;
        self.query("create-table", "/* culprit fixture table setup only */ CREATE TABLE dbo.culprit_cdc_fixture(id int NOT NULL PRIMARY KEY,payload bigint NOT NULL);").await?;
        self.query("seed-table", "/* culprit fixture seed only */ INSERT dbo.culprit_cdc_fixture VALUES(1,1);").await?;
        // First-table enablement creates native Agent capture/cleanup jobs. Each lifecycle operation
        // retains its own outcome; a timeout cannot be hidden behind a later reconnect or cleanup batch.
        self.query_with_timeout("enable-table", FIRST_TABLE_SETUP_TIMEOUT, "/* culprit fixture table CDC setup only */ EXEC sys.sp_cdc_enable_table @source_schema=N'dbo',@source_name=N'culprit_cdc_fixture',@capture_instance=N'culprit_cdc_fixture',@role_name=NULL,@supports_net_changes=0;").await?;
        self.disable_capture_job().await?;
        self.query("insert-positive", "/* culprit fixture positive work only */ INSERT dbo.culprit_cdc_fixture VALUES(2,2);IF @@ROWCOUNT<>1 THROW 51000,'owned CDC positive quota incomplete',1;").await?;
        // The direct documented scan refuses if a competing capture still owns the native scan operation.
        // There is no retry, continuous polling or success skip if the controlled operation cannot settle.
        self.query("scan-positive", SCAN).await?;
        let enabled = self.capture(&mut captures, "original-enabled-progress", cancel).await?;
        assert_eq!(status(&enabled).enabled, Some(true));
        assert_eq!(status(&enabled).permission, MssqlCdcPermission::ViewDatabasePerformanceState);
        assert_eq!(status(&enabled).has_permission, Some(true));
        assert!(status(&enabled).scan_layout_available && status(&enabled).error_layout_available);
        assert!(enabled.records.iter().any(|row|matches!(&row.record,TelemetryRecord::Operational(row) if matches!(&row.fact,OperationalFact::MssqlCdc(point) if matches!(&point.data,MssqlCdcData::ScanSession(_))))),"require genuine native scan rows, not enabled metadata alone");
        assert!(
            has_processed_scan(&enabled),
            "require genuine native processed transaction/log work after the exact owned insert"
        );
        let processed=self.query("verify-processed-row", "SELECT /* culprit fixture exact owned change-table quota */ COUNT_BIG(*) AS rows FROM cdc.culprit_cdc_fixture_CT WHERE __$operation=2 AND id=2").await?;
        let [processed] = processed.as_slice() else {
            return Err(invalid_data());
        };
        assert_eq!(integer(processed, "rows")?, 1);
        self.owns_user = true;
        self.query("create-denied-user", "/* culprit fixture permission setup only */ CREATE USER culprit_cdc_denied WITHOUT LOGIN;DENY VIEW DATABASE PERFORMANCE STATE TO culprit_cdc_denied;").await?;
        let denied=self.query("verify-permission-denial", "/* culprit fixture native denial only */ DECLARE @permission int,@error int;EXECUTE AS USER=N'culprit_cdc_denied';SET @permission=HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE');BEGIN TRY SELECT TOP(1) session_id FROM sys.dm_cdc_errors;SET @error=0;END TRY BEGIN CATCH SET @error=ERROR_NUMBER();END CATCH;REVERT;SELECT @permission AS has_permission,@error AS denied_error_number;").await?;
        let [row] = denied.as_slice() else {
            return Err(invalid_data());
        };
        assert_eq!(integer(row, "has_permission")?, 0);
        let denied_error = integer(row, "denied_error_number")?;
        assert!(
            matches!(denied_error, 229 | 297 | 300),
            "native permission denial must be genuine, not arbitrary procedure failure"
        );
        // The current rows fit int, while committed pending-log changes retain a bigint outside its
        // range. Narrowing before capture exercises the documented unsupported bigint-to-int case.
        // https://learn.microsoft.com/sql/relational-databases/track-changes/known-issues-and-errors-change-data-capture
        let work = format!(
            "/* culprit fixture pending-log type-conversion failure only */ BEGIN TRANSACTION;INSERT dbo.culprit_cdc_fixture VALUES(3,{PRIVATE_PAYLOAD});IF @@ROWCOUNT<>1 THROW 51000,'owned CDC overflow insert quota incomplete',1;DELETE dbo.culprit_cdc_fixture WHERE id=3;IF @@ROWCOUNT<>1 THROW 51000,'owned CDC overflow delete quota incomplete',1;ALTER TABLE dbo.culprit_cdc_fixture ALTER COLUMN payload int NOT NULL;COMMIT TRANSACTION;"
        );
        self.query("stage-documented-type-conversion", &work).await?;
        // Catch only inside this fixture batch so even an expected native failure is fully drained.
        // Driver errors/timeout still fence all later fixture SQL; physical close cannot establish completion.
        let scan = format!(
            "DECLARE @error int=0;BEGIN TRY {SCAN} END TRY BEGIN CATCH SET @error=ERROR_NUMBER();END CATCH;SELECT /* culprit fixture numeric scan outcome only */ @error AS scan_error_number;"
        );
        let scan = self.query("scan-documented-type-conversion", &scan).await?;
        let [scan] = scan.as_slice() else {
            return Err(invalid_data());
        };
        let scan_error_number = integer(scan, "scan_error_number")?;
        let scan_failed = scan_error_number != 0;
        let failed = self.capture(&mut captures, "original-retained-error", cancel).await?;
        let errors = failed
            .records
            .iter()
            .filter_map(|row| match &row.record {
                TelemetryRecord::Operational(row) => match &row.fact {
                    OperationalFact::MssqlCdc(point) => match &point.data {
                        MssqlCdcData::Error(error) => Some(error),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        let conversion = errors
            .iter()
            .find(|error| matches!(error.error_number, 8114 | 8115))
            .expect("require a genuine documented native conversion error, not a skipped or synthesized positive");
        assert!(conversion.session_id > 0 && conversion.phase_number.is_some_and(|phase| phase > 0));
        for lsn in [&conversion.start_lsn, &conversion.sequence_value] {
            assert!(
                lsn.as_ref().is_some_and(|value| value.bytes().any(|byte| byte.is_ascii_hexdigit() && byte != b'0')),
                "the controlled failure needs an actual nonzero native log address"
            );
        }
        assert!(
            errors.iter().any(|error| error.error_number == 22859
                && error.session_id == conversion.session_id
                && error.phase_number == conversion.phase_number
                && error.start_lsn == conversion.start_lsn
                && error.sequence_value == conversion.sequence_value),
            "the original native processing-failure wrapper must identify the same controlled scan and log record"
        );
        assert!(
            scan_sessions(&failed).any(|scan| scan.session_id == conversion.session_id
                && positive_counter(&scan.error_count)
                && positive_counter(&scan.failed_sessions)),
            "the same original scan must report genuine failed processing"
        );
        assert!(!serde_json::to_string(&failed.records).unwrap().contains(&PRIVATE_PAYLOAD.to_string()));
        assert_eq!(disabled.source()?.target, enabled.source()?.target);
        assert_eq!(enabled.source()?.target, failed.source()?.target);
        assert_eq!(disabled.source()?.partition, enabled.source()?.partition);
        assert_eq!(enabled.source()?.partition, failed.source()?.partition);
        assert_eq!(source_session(&disabled), source_session(&enabled));
        assert_eq!(source_session(&enabled), source_session(&failed));
        for original in [&disabled, &enabled, &failed] {
            captures.verify_retained(original).await?;
        }
        Ok(
            json!({"native_denied_error_number":denied_error,"documented_scan_returned_error":scan_failed,"documented_scan_error_number":scan_error_number,"genuine_processing_error":true,"cdc_source_dispatches":[disabled.manifest.source_queries,enabled.manifest.source_queries,failed.manifest.source_queries],"source_stats_session_setup_dispatches":1,"source_stats_session_reused":true,"native_session_work_is_partial_self_observation":true,"login_and_logon_trigger_work_measured":false,"new_error_count_calculated":false,"counter_continuity_qualified":false,"native_local_times_converted_to_utc":false,"cost_qualified":false}),
        )
    }
    async fn capture(
        &mut self,
        captures: &mut OwnedCaptures,
        stage: &str,
        cancel: &CollectionCancellation,
    ) -> Result<Captured> {
        Box::pin(async {
            if cancel.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            let captured = Box::pin(
                captures
                    .capture_with_cancellation(CollectionSelection::Snapshot { dataset: SnapshotDataset::Cdc }, cancel),
            )
            .await?;
            self.stages.push(json!({"stage":stage,"manifest":captured.manifest,"records":captured.records}));
            assert_eq!(captured.manifest.status, CollectionStatus::Complete);
            assert_eq!(captured.source()?.continuity, SourceContinuity::Unverified);
            let recipe = crate::telemetry::recipes()
                .into_iter()
                .find(|recipe| recipe.id == "mssql.cdc")
                .expect("production CDC recipe");
            assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
            assert!(captured.manifest.source_queries <= recipe.budget.max_queries);
            assert!(captured.source()?.partition.is_qualified());
            assert_eq!(captured.source()?.target.database().unwrap().incarnation, None);
            for row in &captured.records {
                let TelemetryRecord::Operational(row) = &row.record else {
                    return Err(invalid_data());
                };
                let OperationalFact::MssqlCdc(point) = &row.fact else {
                    return Err(invalid_data());
                };
                assert!(point.fits_source(captured.source()?));
                assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
            }
            Ok(captured)
        })
        .await
    }
    async fn cleanup(&mut self) -> Result<()> {
        Box::pin(async {
            self.cleaning = true;
            let mut first_error = self.retire().await.err();
            if !self.control.is_available() {
                // Neither a new nonce verification nor DROP/disable is safe after an unconfirmed command.
                // The existing nonce-labelled supervisor owns destruction of this disposable database/container.
                return Err(first_error.unwrap_or_else(|| CollectionStop::CancellationUnconfirmed.error()));
            }
            let cleanup=async {
                self.open_control().await?;
                if self.owns_cdc{self.query("cleanup-disable-cdc", "/* culprit fixture cleanup only */ IF EXISTS(SELECT 1 FROM sys.databases WHERE database_id=DB_ID() AND is_cdc_enabled=1) EXEC sys.sp_cdc_disable_db;").await?;}
                if self.owns_table{self.query("cleanup-drop-table", "/* culprit fixture cleanup only */ IF OBJECT_ID(N'dbo.culprit_cdc_fixture') IS NOT NULL DROP TABLE dbo.culprit_cdc_fixture;").await?;}
                if self.owns_user{self.query("cleanup-drop-user", "/* culprit fixture cleanup only */ IF USER_ID(N'culprit_cdc_denied') IS NOT NULL DROP USER culprit_cdc_denied;").await?;}
                Ok::<_,culprit_core::Error>(())
            }.await;
            if let Err(error) = cleanup {
                first_error.get_or_insert(error);
            }
            if let Err(error) = self.retire().await {
                first_error.get_or_insert(error);
            }
            first_error.map_or(Ok(()), Err)
        }).await
    }
}
fn positive_counter(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
}
fn scan_sessions(captured: &Captured) -> impl Iterator<Item = &MssqlCdcScanSession> {
    captured.records.iter().filter_map(|row| {
        let TelemetryRecord::Operational(row) = &row.record else {
            return None;
        };
        let OperationalFact::MssqlCdc(point) = &row.fact else {
            return None;
        };
        let MssqlCdcData::ScanSession(scan) = &point.data else {
            return None;
        };
        Some(scan.as_ref())
    })
}
fn has_processed_scan(captured: &Captured) -> bool {
    scan_sessions(captured).any(|scan| positive_counter(&scan.transactions) && positive_counter(&scan.log_records))
}
fn source_session(captured: &Captured) -> &NativeActivityIdentity {
    let metrics = captured.manifest.read_metrics.as_ref().expect("original native reader accounting");
    let NativeSessionReadWork::MssqlSession(work) =
        metrics.native_session_work.as_deref().expect("original native source session");
    assert_eq!(work.settlement, NativeReadSettlement::Quiescent);
    let interval = work.interval.as_ref().expect("native self-observation interval");
    assert_eq!(interval.initial.query_ordinal, 3);
    assert_eq!(interval.latest.as_ref().unwrap().query_ordinal, captured.manifest.source_queries);
    &interval.session
}
fn status(captured: &Captured) -> &MssqlCdcStatus {
    captured
        .records
        .iter()
        .find_map(|row| match &row.record {
            TelemetryRecord::Operational(row) => match &row.fact {
                OperationalFact::MssqlCdc(point) => match &point.data {
                    MssqlCdcData::Status(status) => Some(status),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .expect("genuine original native CDC status")
}
#[tokio::test]
#[ignore = "requires explicit mssql-agent nonce-owned SQLServer2022+ fixture; CDC/table/user/scan work is fixture-only, never production collection"]
async fn owned_cdc_status_permission_progress_and_retained_error_are_original_bounded_points() {
    Box::pin(run_owned()).await;
}
async fn run_owned() {
    let owner = Box::pin(OwnedSourceFixture::open("culprit")).await.unwrap();
    let cancel = CollectionCancellation::default();
    let mut fixture = Box::new(Fixture {
        owner,
        cancellation: cancel.clone(),
        cleaning: false,
        client: None,
        control: Control::new(),
        control_queries: 0,
        control_connect_attempts: 0,
        control_session_setups: 0,
        capture_job: None,
        owns_cdc: false,
        owns_table: false,
        owns_user: false,
        stages: Vec::new(),
    });
    let (expired, result) = finish_case(
        Instant::now() + CASE_TIMEOUT,
        &cancel,
        AssertUnwindSafe(Box::pin(fixture.exercise(&cancel))).catch_unwind(),
    )
    .await;
    let cleanup = AssertUnwindSafe(Box::pin(fixture.cleanup())).catch_unwind().await;
    save_cost_receipt(&receipt_directory(),"cdc-owned-failure-progress",&json!({"successful":!expired&&matches!(&result,Ok(Ok(_)))&&matches!(&cleanup,Ok(Ok(()))),"expired":expired,"fixture_driver_response_timeout_ms":FIRST_TABLE_SETUP_TIMEOUT.as_millis(),"fixture_ordinary_control_deadline_ms":CONTROL_TIMEOUT.as_millis(),"fixture_first_table_setup_deadline_ms":FIRST_TABLE_SETUP_TIMEOUT.as_millis(),"fixture_control_query_attempts":fixture.control_queries,"fixture_connection_attempts":fixture.control_connect_attempts,"confirmed_fixture_session_setup_dispatches":fixture.control_session_setups,"fixture_setup_uncertain":fixture.control_connect_attempts!=fixture.control_session_setups||!fixture.control.is_available(),"fixture_control_fenced":!fixture.control.is_available(),"supervisor_destruction_required":!fixture.control.is_available(),"fixture_control_stages":fixture.control.events(),"owned_capture_job":fixture.capture_job,"owned_capture_job_limits":capture_job::limits(),"initial_owned_admission_queries":1,"initial_owned_session_setup_dispatches":1,"native_source_and_fixture_work_separate":true,"stages":fixture.stages,"result":match&result{Ok(Ok(value))=>Some(value),_=>None},"case_error":match&result{Ok(Err(error))=>Some(error.code),_=>None},"case_error_detail":match&result{Ok(Err(error))=>error.detail,_=>None},"cleanup_error":match&cleanup{Ok(Err(error))=>Some(error.code),_=>None},"cleanup_error_detail":match&cleanup{Ok(Err(error))=>error.detail,_=>None},"cost_qualified":false})).unwrap();
    match result {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => {
            result.unwrap();
        }
    }
    match cleanup {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => {
            result.unwrap();
        }
    }
    assert!(!expired, "original native work must settle inside its owned case bound");
}
