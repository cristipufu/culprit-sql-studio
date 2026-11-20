//! An owned intervention separates native logged generation from a read-only snapshot's protection.
//! It never estimates per-transaction version bytes or waits for background byte reclamation.
use super::*;
use crate::{
    conn::{MsClient, SessionKind},
    read::{invalid_data, text},
    test_support::{
        OwnedSourceFixture, config,
        owned_captures::{Captured, OwnedCaptures},
        owned_case::finish_case,
        receipt_directory, save_cost_receipt, verify_owned_source,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{CleanupReport, SourceOutcome, source_id};
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::time::Instant;

const ROWS: u32 = 256;
const UPDATED_ROWS: u32 = ROWS - 2;
const CASE_TIMEOUT: Duration = Duration::from_secs(45);
const FIXTURE_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
const ACTOR_IDENTITY: &str = r#"SELECT /* culprit fixture */
 CONVERT(varchar(16),s.session_id) AS session_id,
 CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 CONVERT(varchar(20),tx.transaction_id) AS transaction_id,
 CONVERT(varchar(40),CAST(tx.transaction_begin_time AS datetime2),126) AS transaction_generation
 FROM sys.dm_exec_sessions s
 JOIN sys.dm_tran_session_transactions st ON st.session_id=s.session_id
 JOIN sys.dm_tran_active_transactions tx ON tx.transaction_id=st.transaction_id
 WHERE s.session_id=@@SPID"#;

struct Actor {
    session: NativeActivityIdentity,
    transaction: NativeActivityIdentity,
}
impl Actor {
    async fn read(client: &mut MsClient) -> culprit_core::Result<Self> {
        let rows = crate::conn::query(client, ACTOR_IDENTITY).await.map_err(crate::conn::map_error)?;
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        let actor = Self {
            session: NativeActivityIdentity {
                id: text(row, "session_id")?,
                generation: crate::sql_time::native_time(&text(row, "session_generation")?)?,
            },
            transaction: NativeActivityIdentity {
                id: text(row, "transaction_id")?,
                generation: crate::sql_time::native_time(&text(row, "transaction_generation")?)?,
            },
        };
        if !actor.session.is_valid() || !actor.transaction.is_valid() {
            return Err(invalid_data());
        }
        Ok(actor)
    }
    fn matches(&self, row: &MssqlCleanupTransaction) -> bool {
        self.session == row.session && self.transaction == row.transaction
    }
}

struct Fixture {
    admission: OwnedSourceFixture,
    database: String,
    admin: Option<MsClient>,
    generator: Option<MsClient>,
    protector: Option<MsClient>,
    fixture_queries: u32,
    fixture_session_setups: u32,
    fixture_connect_attempts: u32,
    original_evidence: Vec<serde_json::Value>,
}
impl Fixture {
    async fn connect(&mut self, database: &str, label: &str) -> culprit_core::Result<MsClient> {
        let mut options = config(self.admission.port, database, self.admission.password(), label);
        options.command_timeout(Some(FIXTURE_STATEMENT_TIMEOUT));
        self.fixture_connect_attempts += 1;
        let client = crate::test_support::connect(&options, SessionKind::User).await?;
        self.fixture_session_setups += 1;
        Ok(client)
    }
    async fn setup(&mut self) -> culprit_core::Result<()> {
        self.admin = Some(self.connect("master", "cleanup-intervention-setup").await?);
        self.fixture_queries += 1;
        verify_owned_source(self.admin.as_mut().ok_or_else(invalid_data)?).await?;
        let sql = format!(
            "CREATE DATABASE [{db}];ALTER DATABASE [{db}] SET ALLOW_SNAPSHOT_ISOLATION ON;ALTER DATABASE [{db}] SET ACCELERATED_DATABASE_RECOVERY=OFF",
            db = self.database,
        );
        self.admin_batch(&sql).await?;
        let sql = format!(
            r#"USE [{db}];SET LOCK_TIMEOUT 1000;
 CREATE TABLE dbo.cleanup_fixture(id int NOT NULL PRIMARY KEY,n int NOT NULL,pad char(512) NOT NULL);
 WITH n(i) AS (SELECT i FROM (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15)) v(i))
 INSERT dbo.cleanup_fixture SELECT a.i*16+b.i,0,REPLICATE('x',512) FROM n a CROSS JOIN n b;
 IF @@ROWCOUNT<>{rows} THROW 51000,'owned seed quota incomplete',1"#,
            db = self.database,
            rows = ROWS
        );
        self.admin_batch(&sql).await?;
        let database = self.database.clone();
        self.generator = Some(self.connect(&database, "cleanup-generator").await?);
        self.protector = Some(self.connect(&database, "cleanup-protector").await?);
        Ok(())
    }
    async fn admin_batch(&mut self, sql: &str) -> culprit_core::Result<()> {
        self.fixture_queries += 1;
        batch(self.admin.as_mut().ok_or_else(invalid_data)?, sql).await
    }
    async fn generator_batch(&mut self, sql: &str) -> culprit_core::Result<()> {
        self.fixture_queries += 1;
        batch(self.generator.as_mut().ok_or_else(invalid_data)?, sql).await
    }
    async fn protector_batch(&mut self, sql: &str) -> culprit_core::Result<()> {
        self.fixture_queries += 1;
        batch(self.protector.as_mut().ok_or_else(invalid_data)?, sql).await
    }
    async fn exercise(&mut self, cancellation: &CollectionCancellation) -> culprit_core::Result<serde_json::Value> {
        check_cancellation(cancellation)?;
        self.setup().await?;
        check_cancellation(cancellation)?;
        // Establish the logged generator before the read-only snapshot. Its first update leaves a separate
        // untouched row available for the snapshot to establish its original transaction without blocking.
        self.generator_batch("SET LOCK_TIMEOUT 1000;BEGIN TRAN;UPDATE dbo.cleanup_fixture SET n=1 WHERE id=0;IF @@ROWCOUNT<>1 THROW 51000,'generator quota incomplete',1").await?;
        check_cancellation(cancellation)?;
        self.protector_batch("SET LOCK_TIMEOUT 1000;SET TRANSACTION ISOLATION LEVEL SNAPSHOT;BEGIN TRAN;SELECT n FROM dbo.cleanup_fixture WHERE id=255").await?;
        check_cancellation(cancellation)?;
        self.fixture_queries += 1;
        let generator = Actor::read(self.generator.as_mut().ok_or_else(invalid_data)?).await?;
        self.fixture_queries += 1;
        let protector = Actor::read(self.protector.as_mut().ok_or_else(invalid_data)?).await?;
        assert_ne!(generator.session, protector.session);
        assert_ne!(generator.transaction, protector.transaction);
        check_cancellation(cancellation)?;
        self.generator_batch(&format!("UPDATE dbo.cleanup_fixture SET n=1,pad=REPLICATE('y',512) WHERE id BETWEEN 1 AND 254;IF @@ROWCOUNT<>{UPDATED_ROWS} THROW 51000,'generator quota incomplete',1")).await?;
        // A fresh pool has no preparation dispatches. Its one Stats-session setup is separate from the
        // source query counter; the same returned native session must serve every retained capture.
        let pool = Arc::new(Pool::new(
            config(self.admission.port, &self.database, self.admission.password(), "culprit"),
            None,
        ));
        let mut captures = OwnedCaptures::from_pool(pool, self.admission.port)?;
        let original = capture(&mut captures, cancellation).await?;
        self.retain_stage("both-alive", &original);
        let original_horizon = horizon(&original);
        let original_generator = original_horizon.oldest_confirmed.as_ref().expect("live logged generator");
        assert!(generator.matches(original_generator));
        assert!(
            original_generator
                .log_record_count
                .as_deref()
                .is_some_and(|count| count.parse::<u64>().is_ok_and(|count| count > 0))
        );
        assert_eq!(original_generator.snapshot_isolation, Some(false));
        assert!(original_generator.row_version_participant);
        self.generator_batch("COMMIT").await?;
        // This is the same previously established snapshot, not a replacement transaction or a new input.
        self.protector_batch("IF EXISTS(SELECT 1 FROM dbo.cleanup_fixture WHERE id BETWEEN 1 AND 254 AND n<>0) THROW 51000,'original snapshot lost its earlier row versions',1").await?;
        let protected = capture(&mut captures, cancellation).await?;
        self.retain_stage("original-protector", &protected);
        let protected_horizon = horizon(&protected);
        let original_protector = protected_horizon.oldest_confirmed.as_ref().expect("original protecting snapshot");
        assert!(protector.matches(original_protector));
        assert_eq!(original_protector.snapshot_isolation, Some(true));
        assert_eq!(original_protector.log_record_count.as_deref(), Some("0"));
        assert!(original_protector.row_version_participant);
        assert!(
            version_kibibytes(&protected) > 0,
            "the native database gauge must observe retained versions; no byte attribution is inferred"
        );
        self.protector_batch("COMMIT").await?;
        let recovered = capture(&mut captures, cancellation).await?;
        self.retain_stage("protection-ended", &recovered);
        assert!(horizon(&recovered).oldest_confirmed.is_none());
        assert!(horizon(&recovered).oldest_unattributed_versioning.is_none());
        for (before, after) in [(&original, &protected), (&protected, &recovered)] {
            assert_eq!(before.source()?.target, after.source()?.target);
            assert_eq!(before.source()?.partition, after.source()?.partition);
            assert!(
                ObservationBounds::new(
                    &before.source()?.observation.snapshot().unwrap().observed_until,
                    &after.source()?.observation.snapshot().unwrap().observed_from,
                )
                .is_some()
            );
        }
        let original_source_session = source_session(&original);
        assert_eq!(source_session(&protected), original_source_session);
        assert_eq!(source_session(&recovered), original_source_session);
        for captured in [&original, &protected, &recovered] {
            captures.verify_retained(captured).await?;
        }
        let reports = [report(&original)?, report(&protected)?, report(&recovered)?];
        assert!(
            reports[0].entries[0]
                .explanation
                .contains(&culprit_core::t!("view.cleanup.transaction.context.logged_versioning"))
        );
        assert!(
            reports[1].entries[0]
                .explanation
                .contains(&culprit_core::t!("view.cleanup.transaction.context.snapshot_zero_log"))
        );
        assert!(reports[2].entries.iter().all(|entry| entry.mechanisms.is_empty()));
        Ok(json!({
            "reports":reports,
            "updated_rows":UPDATED_ROWS+1,
            "source_dispatches":33,
            "source_session_setup_dispatches":1,
            "source_session_reused":true,
            "native_session_work_is_partial_self_observation":true,
            "login_and_logon_trigger_cost_measured":false,
            "version_bytes_attributed_to_participant":false,
            "immediate_byte_reclamation_required":false,
            "cost_qualified":false,
        }))
    }
    fn retain_stage(&mut self, stage: &str, captured: &Captured) {
        self.original_evidence.push(json!({
            "stage": stage,
            "manifest": &captured.manifest,
            "records": &captured.records,
        }));
    }
    async fn cleanup(&mut self) -> culprit_core::Result<()> {
        // Attempt every original actor close even if one close fails. Only this nonce-owned database
        // may then be removed; an actor/close error still fails the native case.
        let mut first_error = None;
        for client in [self.generator.take(), self.protector.take(), self.admin.take()].into_iter().flatten() {
            if let Err(error) = client.close().await {
                first_error.get_or_insert_with(|| crate::conn::map_error(error));
            }
        }
        let operation = async {
            self.admin = Some(self.connect("master", "cleanup-intervention-teardown").await?);
            self.fixture_queries += 1;
            verify_owned_source(self.admin.as_mut().ok_or_else(invalid_data)?).await?;
            let sql = format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{db}];END",
                db = self.database
            );
            self.admin_batch(&sql).await
        }.await;
        if let Err(error) = operation {
            first_error.get_or_insert(error);
        }
        // Verification/drop errors never bypass settlement of this same retained teardown admin.
        if let Some(admin) = self.admin.take()
            && let Err(error) = admin.close().await
        {
            first_error.get_or_insert_with(|| crate::conn::map_error(error));
        }
        first_error.map_or(Ok(()), Err)
    }
}
fn check_cancellation(cancellation: &CollectionCancellation) -> culprit_core::Result<()> {
    if cancellation.is_cancelled() { Err(CollectionStop::Cancelled.error()) } else { Ok(()) }
}

async fn batch(client: &mut MsClient, sql: &str) -> culprit_core::Result<()> {
    client
        .simple_query(sql)
        .await
        .map_err(crate::conn::map_error)?
        .into_results()
        .await
        .map_err(crate::conn::map_error)?;
    Ok(())
}
async fn capture(captures: &mut OwnedCaptures, cancel: &CollectionCancellation) -> culprit_core::Result<Captured> {
    let captured = captures
        .capture_with_cancellation(CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup }, cancel)
        .await?;
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    assert_eq!(captured.manifest.source_queries, 11);
    let native = captured.source()?.observation.snapshot().ok_or_else(invalid_data)?;
    assert!(native.sources.iter().all(|source| matches!(
        source.state,
        culprit_core::SourceState::Collected | culprit_core::SourceState::NotApplicable
    )));
    assert!(native.sources.iter().any(|source| source.source == source_id::PERSISTENT_VERSION_STORE
        && source.state == culprit_core::SourceState::NotApplicable));
    Ok(captured)
}
fn horizon(captured: &Captured) -> &MssqlCleanupHorizon {
    captured
        .records
        .iter()
        .find_map(|row| match &row.record {
            TelemetryRecord::Operational(row) => match &row.fact {
                OperationalFact::MssqlCleanupHorizon(horizon) => Some(horizon),
                _ => None,
            },
            _ => None,
        })
        .expect("original native horizon")
}
fn version_kibibytes(captured: &Captured) -> u64 {
    captured
        .records
        .iter()
        .find_map(|row| match &row.record {
            TelemetryRecord::Operational(row) if row.source == source_id::VERSION_STORE => match &row.fact {
                OperationalFact::Counter(counter) => counter.value.integer(),
                _ => None,
            },
            _ => None,
        })
        .expect("measured native version-store gauge")
}
fn source_session(captured: &Captured) -> &NativeActivityIdentity {
    let metrics = captured.manifest.read_metrics.as_ref().expect("original source metrics");
    let NativeSessionReadWork::MssqlSession(work) =
        metrics.native_session_work.as_deref().expect("native source-session facts");
    assert_eq!(work.settlement, NativeReadSettlement::Quiescent);
    let interval = work.interval.as_ref().expect("native self-observation interval");
    assert_eq!(interval.initial.query_ordinal, 3);
    assert_eq!(interval.latest.as_ref().unwrap().query_ordinal, 11);
    &interval.session
}
fn report(captured: &Captured) -> culprit_core::Result<CleanupReport> {
    let native = captured
        .records
        .iter()
        .filter_map(|row| match &row.record {
            TelemetryRecord::Operational(row)
                if matches!(
                    row.fact,
                    OperationalFact::MssqlCleanupHorizon(_) | OperationalFact::MssqlPersistentVersionStore(_)
                ) =>
            {
                Some(row)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let sources = captured
        .source()?
        .observation
        .snapshot()
        .ok_or_else(invalid_data)?
        .sources
        .iter()
        .filter(|source| source.source != source_id::VERSION_STORE)
        .map(|source| SourceOutcome {
            source: source.source.clone(),
            state: source.state,
            reason: None,
            reason_message: None,
        })
        .collect::<Vec<_>>();
    culprit_checks::cleanup::explain(
        culprit_checks::cleanup::CleanupEvidence { retention: None, native: &native },
        &sources,
    )
}

#[tokio::test]
#[ignore = "requires guarded nonce-owned loopback SQL Server with CULPRIT_SMOKE_OWNER, CULPRIT_SMOKE_DATABASE and private receipt directory"]
async fn owned_generator_and_original_snapshot_protector_have_distinct_cleanup_roles_until_protection_ends() {
    let directory = receipt_directory();
    let admission = OwnedSourceFixture::open("cleanup-intervention-admission").await.unwrap();
    let database = format!("{}_cleanup_{}", admission.database, std::process::id());
    let mut fixture = Fixture {
        admission,
        database,
        admin: None,
        generator: None,
        protector: None,
        fixture_queries: 0,
        fixture_session_setups: 0,
        fixture_connect_attempts: 0,
        original_evidence: Vec::new(),
    };
    let cancellation = CollectionCancellation::default();
    let (expired, body) = finish_case(
        Instant::now() + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(Box::pin(fixture.exercise(&cancellation))).catch_unwind(),
    )
    .await;
    let cleanup = AssertUnwindSafe(Box::pin(fixture.cleanup())).catch_unwind().await;
    let successful = matches!(&body, Ok(Ok(_))) && matches!(&cleanup, Ok(Ok(()))) && !expired;
    let details = match &body {
        Ok(Ok(value)) => Some(value),
        _ => None,
    };
    save_cost_receipt(
        &directory,
        "cleanup-generator-protector",
        &json!({
            "successful":successful,"expired":expired,
            "fixture_query_attempts":fixture.fixture_queries,
            "confirmed_fixture_session_setup_dispatches":fixture.fixture_session_setups,
            "fixture_connect_attempts":fixture.fixture_connect_attempts,
            "fixture_connection_setup_uncertain":fixture.fixture_connect_attempts!=fixture.fixture_session_setups,
            "initial_admission_queries":1,"initial_admission_session_setup_dispatches":1,
            "setup_and_workload_separate_from_source":true,
            "successful_case_expected_fixture_queries":13,
            "successful_case_expected_fixture_session_setups":4,
            "successful_case_expected_total_sql_dispatches_including_admission_and_source":53,
            "owned_database_removed":matches!(&cleanup,Ok(Ok(()))),
            "case_error":match &body{Ok(Err(error))=>Some(error.code),_=>None},
            "cleanup_error":match &cleanup{Ok(Err(error))=>Some(error.code),_=>None},
            "original_evidence":&fixture.original_evidence,
            "evidence":details,"cost_qualified":false,
        }),
    )
    .unwrap();
    match body {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => {
            result.unwrap();
        }
    }
    match cleanup {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.unwrap(),
    }
    assert_eq!(fixture.fixture_queries, 13);
    assert_eq!(fixture.fixture_session_setups, 4);
    assert_eq!(fixture.fixture_connect_attempts, 4);
    assert!(!expired, "owned original source and actors settled after the case deadline");
}
