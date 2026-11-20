//! Shared SQL/decoder acceptance in a nonce-owned database. The Azure-only indexes factory is unchanged.
use super::*;
use crate::{
    conn::{MsClient, Pool, SessionKind},
    read::ReadSession,
    test_support::{OwnedSourceFixture, config, connect, verify_owned_source},
};
use futures_util::FutureExt;
use std::{
    panic::{AssertUnwindSafe, resume_unwind},
    sync::Arc,
    time::Duration,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
struct Fixture<'a> {
    owned: &'a OwnedSourceFixture,
    table: String,
    user: String,
    owns: bool,
    control_available: bool,
}
impl Fixture<'_> {
    async fn control<T>(&mut self, work: impl AsyncFnOnce(&mut MsClient) -> T) -> T {
        assert!(self.control_available, "an unconfirmed original command forbids further fixture SQL");
        // A failed acquisition/verification cannot be followed by a new actor either.
        self.control_available = false;
        let mut actor = connect(
            &config(self.owned.port, &self.owned.database, self.owned.password(), "culprit-columnstore-control"),
            SessionKind::User,
        )
        .await
        .unwrap();
        let operation = AssertUnwindSafe(tokio::time::timeout(COMMAND_TIMEOUT, async {
            verify_owned_source(&mut actor).await.unwrap();
            work(&mut actor).await
        }))
        .catch_unwind()
        .await;
        let retired = retire_control(operation, actor.close()).await;
        self.control_available = retired.permits_sql();
        retired.finish()
    }
    async fn command(&mut self, sql: &str) {
        self.control(async |actor| crate::conn::execute(actor, sql).await.unwrap()).await;
    }
    async fn initialize(&mut self) {
        let sql = format!(
            "SELECT /* culprit fixture */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,OBJECT_ID(N'dbo.{}',N'U') AS table_id,USER_ID(N'{}') AS user_id",
            self.table, self.user
        );
        let row = self.control(async |actor| crate::conn::query_one(actor, &sql).await.unwrap()).await;
        assert!(integer(&row, "major").unwrap() >= 16, "the ordered-columnstore positive requires SQL Server 2022+");
        assert!(
            crate::read::optional_integer(&row, "table_id").unwrap().is_none()
                && crate::read::optional_integer(&row, "user_id").unwrap().is_none(),
            "no existing object can be claimed"
        );
        self.owns = true;
        self.command(&format!("CREATE TABLE dbo.[{}](id int NOT NULL,payload int NULL); INSERT dbo.[{}] VALUES(1,10),(2,20); CREATE CLUSTERED COLUMNSTORE INDEX cci_owned ON dbo.[{}] ORDER(id) WITH(COMPRESSION_DELAY=15,MAXDOP=1); CREATE USER [{}] WITHOUT LOGIN; DENY VIEW DEFINITION TO [{}]; GRANT SELECT ON OBJECT::dbo.[{}] TO [{}];",self.table,self.table,self.table,self.user,self.user,self.table,self.user)).await;
    }
    async fn cleanup(&mut self) {
        if self.owns && !self.control_available {
            // Physical retirement is not an Attention acknowledgement. The positively nonce-owned
            // supervisor destroys the disposable fixture; this uncertain client starts no replacement SQL.
            eprintln!("owned columnstore command unconfirmed; fixture SQL stopped; supervisor destruction required");
            return;
        }
        if self.owns {
            self.command(&format!("IF OBJECT_ID(N'dbo.{}',N'U') IS NOT NULL DROP TABLE dbo.[{}]; IF USER_ID(N'{}') IS NOT NULL DROP USER [{}];",self.table,self.table,self.user,self.user)).await;
            self.owns = false;
        }
    }
}

type BoundedControl<T> = std::thread::Result<std::result::Result<T, tokio::time::error::Elapsed>>;

/// A drained result and socket retirement are separate facts. Only both succeeding reopen this fixture gate.
struct RetiredControl<T> {
    operation: BoundedControl<T>,
    retirement: BoundedControl<tiberius::Result<()>>,
}
async fn retire_control<T>(
    operation: BoundedControl<T>,
    retirement: impl std::future::Future<Output = tiberius::Result<()>>,
) -> RetiredControl<T> {
    let retirement = AssertUnwindSafe(tokio::time::timeout(COMMAND_TIMEOUT, retirement)).catch_unwind().await;
    RetiredControl { operation, retirement }
}
impl<T> RetiredControl<T> {
    fn permits_sql(&self) -> bool {
        matches!(self.operation, Ok(Ok(_))) && matches!(self.retirement, Ok(Ok(Ok(()))))
    }
    fn finish(self) -> T {
        // Retirement was already awaited. Its failure can never replace the original operation failure.
        let value = match self.operation {
            Err(panic) => resume_unwind(panic),
            Ok(Err(timeout)) => panic!("bounded owned catalog control timed out: {timeout}"),
            Ok(Ok(value)) => value,
        };
        match self.retirement {
            Err(panic) => resume_unwind(panic),
            Ok(Err(timeout)) => panic!("bounded original fixture client retirement timed out: {timeout}"),
            Ok(Ok(result)) => result.expect("original fixture client retirement"),
        }
        value
    }
}

#[tokio::test]
#[ignore = "requires the nonce-owned loopback SQL2022+ fixture; no production endpoints or rowgroup walk"]
async fn owned_columnstore_configuration_matches_scan_and_stream_and_optional_permission_refusal() {
    let owned = OwnedSourceFixture::open("culprit-columnstore-fixture").await.unwrap();
    let suffix = format!("{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let mut fixture = Fixture {
        owned: &owned,
        table: format!("culprit_columnstore_{suffix}"),
        user: format!("culprit_columnstore_reader_{suffix}"),
        owns: false,
        control_available: true,
    };
    let body = AssertUnwindSafe(async {
        fixture.initialize().await;
        let initial = compare(&fixture, "initial", None).await.expect("SA initial native configuration");
        assert_eq!(initial.compression_delay_availability, MssqlCatalogAvailability::Available);
        assert_eq!(initial.compression_delay_minutes, Some(15));
        assert_eq!(initial.column_store_order_availability, MssqlCatalogAvailability::Available);
        assert_eq!(
            initial.columns.iter().find(|column| column.name == "id").unwrap().column_store_order_ordinal,
            Some(1)
        );
        assert_eq!(
            initial.columns.iter().find(|column| column.name == "payload").unwrap().column_store_order_ordinal,
            Some(0)
        );
        fixture.command(&format!("ALTER INDEX cci_owned ON dbo.[{}] SET(COMPRESSION_DELAY=30)", fixture.table)).await;
        let changed = compare(&fixture, "changed", None).await.expect("SA changed native configuration");
        assert_eq!(changed.table, initial.table);
        assert_eq!(changed.index_id, initial.index_id);
        assert_eq!(
            changed.compression_delay_minutes,
            Some(30),
            "the reader must observe changed persisted configuration"
        );
        if let Some(denied) = compare(&fixture, "denied", Some(&initial.table)).await {
            assert_eq!(denied.compression_delay_availability, MssqlCatalogAvailability::PermissionDenied);
            assert!(denied.compression_delay_minutes.is_none());
            assert_eq!(denied.column_store_order_availability, MssqlCatalogAvailability::PermissionDenied);
            assert_eq!(denied.data_clustering_availability, MssqlCatalogAvailability::PermissionDenied);
            assert!(denied.columns.iter().all(|column| {
                column.column_store_order_ordinal.is_none() && column.data_clustering_ordinal.is_none()
            }));
            assert_eq!(denied.table, initial.table);
            assert_eq!(denied.index_id, initial.index_id);
        }
    })
    .catch_unwind()
    .await;
    let cleanup = AssertUnwindSafe(fixture.cleanup()).catch_unwind().await;
    if let Err(panic) = body {
        resume_unwind(panic);
    }
    cleanup.expect("exact owned objects cleaned up");
}

async fn compare(
    fixture: &Fixture<'_>,
    stage: &str,
    denied: Option<&MssqlObjectIdentity>,
) -> Option<Box<MssqlColumnstoreConfiguration>> {
    let scan = read(fixture, stage, denied, false).await;
    let stream = read(fixture, stage, denied, true).await;
    assert_eq!(scan, stream, "one shared native projection and decoder");
    scan
}
async fn read(
    fixture: &Fixture<'_>,
    stage: &str,
    denied: Option<&MssqlObjectIdentity>,
    stream: bool,
) -> Option<Box<MssqlColumnstoreConfiguration>> {
    let pool = Arc::new(Pool::new(
        config(fixture.owned.port, &fixture.owned.database, fixture.owned.password(), "culprit-columnstore-source"),
        None,
    ));
    let mut reader = ReadSession::application(pool.clone());
    let work = AssertUnwindSafe(async {
        eprintln!("owned columnstore phase: {}", serde_json::json!({"stage":stage,"stream":stream}));
        if denied.is_some() {
            reader
                .query(&format!("EXECUTE AS USER=N'{}'", fixture.user), vec![], 1)
                .await
                .map_err(ReadError::into_error)?;
        }
        let visibility = match denied {
            Some(original) => Some(denied_visibility(&mut reader, fixture, original).await?),
            None => None,
        };
        let before = reader.queries();
        let metadata = if stream {
            let layout = Layout { with_dmvs: false, expression_limit: MAX_CATALOG_EXPRESSION_BYTES };
            reader
                .begin_stream(
                    &select_sql(&layout),
                    vec![Parameter::Integer((APPLICATION_INDEXES + 1) as i64)],
                    APPLICATION_INDEXES + 1,
                )
                .await
                .map_err(ReadError::into_error)?;
            let mut selected = None;
            let mut rows = 0;
            loop {
                let batch = reader.next_batch().await.map_err(ReadError::into_error)?;
                rows += batch.rows.len();
                assert!(rows <= APPLICATION_INDEXES, "an overflow is not a complete prefix");
                for row in batch.rows {
                    let index = decode(&row, &layout)?.snapshot;
                    if index.schema == "dbo" && index.table_name == fixture.table && index.name == "cci_owned" {
                        assert!(selected.is_none(), "one row per exact native index");
                        assert!(index.is_valid());
                        selected = index.columnstore;
                    }
                }
                if batch.complete {
                    break;
                }
            }
            selected
        } else {
            let (indexes, usage) = application(&mut reader, false).await?;
            assert!(!usage);
            let selected = indexes
                .iter()
                .filter(|index| index.schema == "dbo" && index.table == fixture.table && index.name == "cci_owned")
                .collect::<Vec<_>>();
            match selected.as_slice() {
                [] => None,
                [index] => Some(index.mssql_columnstore.clone().expect("canonical scan metadata")),
                _ => panic!("one owned current index"),
            }
        };
        assert_eq!(reader.queries() - before, 1, "optional metadata adds no native source dispatch or fallback");
        assert_eq!(metadata.is_some(), visibility.unwrap_or(true), "only native visibility can qualify a missing row");
        if let Some(original) = denied {
            let after = denied_visibility(&mut reader, fixture, original).await?;
            assert_eq!(visibility, Some(after), "original denied metadata visibility must remain stable");
            reader.query("REVERT", vec![], 1).await.map_err(ReadError::into_error)?;
        }
        assert_eq!(reader.queries(), if denied.is_some() { 5 } else { 1 }, "fixture context and visibility probes are separately counted");
        eprintln!("owned columnstore phase complete: {}", serde_json::json!({"stage":stage,"stream":stream,"native_row_visible":metadata.is_some(),"denied_visibility_qualified":visibility.is_some()}));
        Ok::<_, culprit_core::Error>(metadata)
    })
    .catch_unwind()
    .await;
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics();
    drop(reader);
    drop(pool);
    let metadata = match work {
        Err(panic) => resume_unwind(panic),
        Ok(result) => result.expect("shared native index source; no skipped positive"),
    };
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(!metrics.unwrap().has_unconfirmed_work());
    metadata
}

/// Fixture-only observations on the same impersonated reader; no source query or availability value is changed.
async fn denied_visibility(
    reader: &mut ReadSession,
    fixture: &Fixture<'_>,
    original: &MssqlObjectIdentity,
) -> Result<bool> {
    let sql = format!(
        "SELECT /* culprit fixture denied metadata witness */ DB_ID() AS database_id,USER_NAME() AS database_user,HAS_PERMS_BY_NAME(DB_NAME(),N'DATABASE',N'VIEW DEFINITION') AS view_definition,OBJECT_ID(N'dbo.{}',N'U') AS visible_object_id,(SELECT COUNT_BIG(*) FROM dbo.[{}]) AS owned_rows,CASE WHEN EXISTS(SELECT 1 FROM sys.indexes i JOIN sys.tables t ON t.object_id=i.object_id JOIN sys.schemas s ON s.schema_id=t.schema_id WHERE s.name=N'dbo' AND t.name=N'{}' AND i.name=N'cci_owned') THEN 1 ELSE 0 END AS visible_index",
        fixture.table, fixture.table, fixture.table
    );
    let rows = reader.query(&sql, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    validate_denied_visibility(row, &fixture.user, original)
}

fn validate_denied_visibility(row: &Rec, user: &str, original: &MssqlObjectIdentity) -> Result<bool> {
    let visible = crate::read::boolean(row, "visible_index")?;
    let object_id = crate::read::optional_integer(row, "visible_object_id")?;
    if integer(row, "database_id")? != i64::from(original.database_id)
        || text(row, "database_user")? != user
        || optional_boolean(row, "view_definition")? != Some(false)
        || integer(row, "owned_rows")? != 2
        || object_id.is_some_and(|object_id| object_id != i64::from(original.object_id))
        || visible && object_id.is_none()
    {
        return Err(invalid_data());
    }
    Ok(visible)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn original_table() -> MssqlObjectIdentity {
        MssqlObjectIdentity { database_id: 7, object_id: 100, created_without_offset: "2026-01-02T03:04:05.000".into() }
    }
    fn visibility_row(visible: bool) -> Rec {
        use crate::value::Val;
        Rec::of(&[
            ("database_id", Val::Int(7)),
            ("database_user", Val::Text("owned-denied-reader".into())),
            ("view_definition", Val::Int(0)),
            ("owned_rows", Val::Int(2)),
            ("visible_object_id", if visible { Val::Int(100) } else { Val::Null }),
            ("visible_index", Val::Int(i64::from(visible))),
        ])
    }

    #[test]
    fn denied_visibility_keeps_native_visible_and_hidden_catalogs_distinct() {
        for visible in [false, true] {
            assert_eq!(
                validate_denied_visibility(&visibility_row(visible), "owned-denied-reader", &original_table()).unwrap(),
                visible
            );
        }
    }

    #[test]
    fn absent_unknown_or_foreign_permission_and_visibility_do_not_qualify_an_empty_source() {
        use crate::value::Val;
        for (name, value) in [
            ("database_id", Val::Int(8)),
            ("database_user", Val::Text("foreign-reader".into())),
            ("view_definition", Val::Null),
            ("view_definition", Val::Int(1)),
            ("owned_rows", Val::Int(1)),
            ("visible_object_id", Val::Int(101)),
            ("visible_index", Val::Int(1)),
        ] {
            let row = crate::resources::test_support::replace(&visibility_row(false), &[(name, value)]);
            assert!(validate_denied_visibility(&row, "owned-denied-reader", &original_table()).is_err(), "{name}");
        }
        assert!(validate_denied_visibility(&Rec::default(), "owned-denied-reader", &original_table()).is_err());
    }

    #[tokio::test]
    async fn a_completed_operation_still_awaits_its_original_client_retirement() {
        let (started, retiring) = tokio::sync::oneshot::channel();
        let (release, allowed) = tokio::sync::oneshot::channel();
        let returned = AtomicBool::new(false);
        let close = async {
            let result = retire_control(Ok(Ok(7)), async {
                started.send(()).unwrap();
                allowed.await.unwrap();
                Ok(())
            })
            .await;
            returned.store(true, Ordering::Release);
            assert!(result.permits_sql());
            result.finish()
        };
        let observer = async {
            retiring.await.unwrap();
            assert!(!returned.load(Ordering::Acquire), "the first result cannot bypass same-client retirement");
            release.send(()).unwrap();
        };
        let (value, ()) = tokio::join!(close, observer);
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn verification_command_and_initial_query_failures_keep_their_first_cause_after_retirement() {
        for message in ["nonce verification", "DDL response", "initial catalog query"] {
            let closed = AtomicBool::new(false);
            let operation: BoundedControl<()> = Err(Box::new(message));
            let result = retire_control(operation, async {
                closed.store(true, Ordering::Release);
                Err(tiberius::error::Error::Protocol("retirement failure".into()))
            })
            .await;
            assert!(closed.load(Ordering::Acquire));
            assert!(!result.permits_sql());
            let first = std::panic::catch_unwind(AssertUnwindSafe(|| result.finish())).unwrap_err();
            assert_eq!(first.downcast_ref::<&str>(), Some(&message));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn successful_socket_retirement_cannot_confirm_a_timed_out_native_operation() {
        let operation =
            AssertUnwindSafe(tokio::time::timeout(COMMAND_TIMEOUT, std::future::pending::<()>())).catch_unwind().await;
        let result = retire_control(operation, async { Ok(()) }).await;
        assert!(!result.permits_sql(), "an unconfirmed DDL must leave all further fixture SQL fenced");
        assert!(std::panic::catch_unwind(AssertUnwindSafe(|| result.finish())).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn failed_panicking_or_timed_out_retirement_cannot_reopen_the_fixture_gate() {
        for failure in ["error", "panic", "timeout"] {
            let result = retire_control(Ok(Ok(())), async {
                match failure {
                    "error" => Err(tiberius::error::Error::Protocol("retirement failure".into())),
                    "panic" => panic!("retirement panic"),
                    _ => std::future::pending::<tiberius::Result<()>>().await,
                }
            })
            .await;
            assert!(!result.permits_sql(), "{failure}");
            assert!(std::panic::catch_unwind(AssertUnwindSafe(|| result.finish())).is_err(), "{failure}");
        }
    }
}
