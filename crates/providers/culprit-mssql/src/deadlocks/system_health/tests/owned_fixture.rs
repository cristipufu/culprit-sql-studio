//! One nonce-owned XE target and producer owner, shared by native scan/collector acceptance and the cost experiment.
use crate::{
    conn::{MsClient, SessionKind, execute, map_error, query},
    read::{integer, invalid_data, text},
    test_support::{OwnedSourceFixture, config, connect, receipt_directory, save_cost_receipt, verify_owned_source},
};
use culprit_core::{Result, telemetry::CollectionStop};
use futures_util::{FutureExt, future::BoxFuture};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, time::Duration};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const TARGET_FIELDS: &str = "SELECT TOP(5) f.name,CONVERT(nvarchar(4000),f.value) AS value FROM sys.server_event_sessions c JOIN sys.server_event_session_targets t ON t.event_session_id=c.event_session_id JOIN sys.server_event_session_fields f ON f.event_session_id=c.event_session_id AND f.object_id=t.target_id WHERE c.name=N'system_health' AND t.name=N'event_file' AND t.package=N'package0'";
const NATIVE_SETUP: &str = "SELECT CONVERT(nvarchar(4000),SERVERPROPERTY('ErrorLogFileName')) AS error_log,CASE WHEN EXISTS(SELECT 1 FROM sys.dm_xe_sessions WHERE name=N'system_health') THEN 1 ELSE 0 END AS running";
const STOP: &str = "IF EXISTS(SELECT 1 FROM sys.dm_xe_sessions WHERE name=N'system_health') ALTER EVENT SESSION [system_health] ON SERVER STATE=STOP;";
const DROP_TARGET: &str = "IF EXISTS(SELECT 1 FROM sys.server_event_sessions c JOIN sys.server_event_session_targets t ON t.event_session_id=c.event_session_id WHERE c.name=N'system_health' AND t.name=N'event_file') ALTER EVENT SESSION [system_health] ON SERVER DROP TARGET package0.event_file;";
const START: &str = "ALTER EVENT SESSION [system_health] ON SERVER STATE=START;";
const TABLE: &str = "dbo.culprit_configured_deadlock_fixture";

#[derive(PartialEq, Eq)]
struct TargetOptions {
    filename: String,
    max_file_size: Option<u32>,
    max_rollover_files: Option<u32>,
}
impl TargetOptions {
    fn from_rows(rows: Vec<crate::value::Rec>) -> Result<Self> {
        let mut fields = BTreeMap::new();
        for row in rows {
            if fields.insert(text(&row, "name")?, text(&row, "value")?).is_some() {
                return Err(invalid_data());
            }
        }
        let filename = fields.remove("filename").ok_or_else(invalid_data)?;
        let max_file_size =
            fields.remove("max_file_size").map(|value| value.parse::<u32>()).transpose().map_err(|_| invalid_data())?;
        let max_rollover_files = fields
            .remove("max_rollover_files")
            .map(|value| value.parse::<u32>())
            .transpose()
            .map_err(|_| invalid_data())?;
        if !fields.is_empty() {
            return Err(invalid_data());
        }
        Ok(Self { filename, max_file_size, max_rollover_files })
    }

    fn with_filename(&self, filename: &str) -> String {
        let mut fields = vec![format!("filename=N'{}'", quote(filename))];
        if let Some(size) = self.max_file_size {
            fields.push(format!("max_file_size=({size})"));
        }
        if let Some(files) = self.max_rollover_files {
            fields.push(format!("max_rollover_files=({files})"));
        }
        fields.join(",")
    }
}

#[derive(PartialEq, Eq)]
struct OriginalTarget {
    options: TargetOptions,
    directory: String,
    running: bool,
}
impl OriginalTarget {
    async fn read(fixture: &OwnedSourceFixture) -> Result<Self> {
        tokio::time::timeout(CONTROL_TIMEOUT, Self::read_native(fixture))
            .await
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
    }

    async fn read_native(fixture: &OwnedSourceFixture) -> Result<Self> {
        let mut admin = administrative_client(fixture).await?;
        let options = TargetOptions::from_rows(query(&mut admin, TARGET_FIELDS).await.map_err(map_error)?)?;
        let metadata = query(&mut admin, NATIVE_SETUP).await.map_err(map_error)?;
        let [metadata] = metadata.as_slice() else { return Err(invalid_data()) };
        let log = text(metadata, "error_log")?;
        let separator = log.rfind(['/', '\\']).ok_or_else(invalid_data)?;
        let directory = log[..=separator].to_owned();
        let running = match integer(metadata, "running")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        Ok(Self { options, directory, running })
    }

    fn install_sql(&self, path: &str) -> String {
        format!(
            "{STOP}{DROP_TARGET}ALTER EVENT SESSION [system_health] ON SERVER ADD TARGET package0.event_file(SET {});{START}{STOP}{START}",
            self.options.with_filename(path)
        )
    }

    fn restore_sql(&self) -> String {
        let resume = if self.running { START } else { "" };
        format!(
            "{STOP}{DROP_TARGET}ALTER EVENT SESSION [system_health] ON SERVER ADD TARGET package0.event_file(SET {});{resume}DROP TABLE IF EXISTS {TABLE};",
            self.options.with_filename(&self.options.filename)
        )
    }
}

pub(crate) fn quote(value: &str) -> String {
    value.replace('\'', "''")
}

async fn administrative_client(fixture: &OwnedSourceFixture) -> Result<MsClient> {
    let mut client = connect(
        &config(fixture.port, &fixture.database, fixture.password(), "culprit-owned-xe-fixture"),
        SessionKind::User,
    )
    .await?;
    // The same held administrative socket proves the full native nonce before every mutation or restoration.
    verify_owned_source(&mut client).await?;
    Ok(client)
}

async fn owned_batch(fixture: &OwnedSourceFixture, sql: &str) -> Result<()> {
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        let mut client = administrative_client(fixture).await?;
        execute(&mut client, sql).await.map_err(map_error)
    })
    .await
    .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
}

async fn plant_deadlock(fixture: &OwnedSourceFixture) -> Result<()> {
    owned_batch(
        fixture,
        &format!("CREATE TABLE {TABLE}(id int NOT NULL PRIMARY KEY,n int);INSERT {TABLE} VALUES(1,0),(2,0);"),
    )
    .await?;
    let mut first = administrative_client(fixture).await?;
    let mut second = administrative_client(fixture).await?;
    execute(&mut first, &format!("SET LOCK_TIMEOUT 10000;BEGIN TRAN;UPDATE {TABLE} SET n=1 WHERE id=1"))
        .await
        .map_err(map_error)?;
    execute(&mut second, &format!("SET LOCK_TIMEOUT 10000;BEGIN TRAN;UPDATE {TABLE} SET n=2 WHERE id=2"))
        .await
        .map_err(map_error)?;
    let first_sql = format!("UPDATE {TABLE} SET n=1 WHERE id=2;COMMIT;");
    let second_sql = format!("UPDATE {TABLE} SET n=2 WHERE id=1;COMMIT;");
    // No spawned/detached actors: both original transactions settle before fixture flushing or source capture.
    let (first_result, second_result) =
        tokio::join!(execute(&mut first, &first_sql), execute(&mut second, &second_sql));
    let victim = |result: &std::result::Result<(), tiberius::error::Error>| matches!(result, Err(tiberius::error::Error::Server(error)) if error.code() == 1205);
    let completed =
        (first_result.is_ok() && victim(&second_result)) || (second_result.is_ok() && victim(&first_result));
    drop((first, second));
    if !completed {
        return Err(invalid_data());
    }
    // Only this disposable fixture's target is flushed. The configured-file collector never reads target_data or does DDL.
    owned_batch(fixture, &format!("{STOP}{START}")).await
}

pub(crate) struct Fixture {
    pub(crate) admitted: OwnedSourceFixture,
    pub(crate) from: String,
    pub(crate) file_stem: String,
    additional_sessions: Vec<String>,
}
impl Fixture {
    pub(crate) async fn admin(&self) -> Result<MsClient> {
        administrative_client(&self.admitted).await
    }

    pub(crate) async fn batch(&self, sql: &str) -> Result<()> {
        owned_batch(&self.admitted, sql).await
    }

    pub(crate) async fn cycle_and_stop_emitter(&self) -> Result<()> {
        self.batch(&format!("{STOP}{START}{STOP}")).await
    }

    pub(crate) async fn native_time(&self) -> Result<String> {
        tokio::time::timeout(CONTROL_TIMEOUT, async {
            let mut admin = self.admin().await?;
            let rows = query(&mut admin, "SELECT CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at")
                .await
                .map_err(map_error)?;
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            culprit_core::telemetry::canonical_time(&text(row, "observed_at")?)
        })
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
    }

    pub(crate) fn additional_session(&mut self, suffix: &str) -> Result<String> {
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err(invalid_data());
        }
        let session = format!("{}_{}", self.admitted.database, suffix);
        if session.len() > 128 || self.additional_sessions.contains(&session) {
            return Err(invalid_data());
        }
        self.additional_sessions.push(session.clone());
        Ok(session)
    }
}

pub(crate) async fn run<T>(
    name: &str,
    absolute: bool,
    work: impl for<'a> FnOnce(&'a mut Fixture) -> BoxFuture<'a, Result<T>>,
) -> Result<T> {
    let directory = receipt_directory();
    let admitted = OwnedSourceFixture::open("culprit").await?;
    let original = OriginalTarget::read(&admitted).await?;
    let stem = format!("{}_configured_deadlock", admitted.database);
    let file_stem = format!("{}{stem}", original.directory);
    let path = if absolute { format!("{file_stem}.xel") } else { format!("{stem}.xel") };
    let mut fixture = Fixture { admitted, file_stem, from: String::new(), additional_sessions: vec![] };
    fixture.from = fixture.native_time().await?;
    let outcome = AssertUnwindSafe(async {
        fixture.batch(&original.install_sql(&path)).await?;
        if absolute {
            plant_deadlock(&fixture.admitted).await?;
        }
        work(&mut fixture).await
    })
    .catch_unwind()
    .await;
    let mut restore = String::new();
    for session in &fixture.additional_sessions {
        restore.push_str(&format!(
            "IF EXISTS(SELECT 1 FROM sys.server_event_sessions WHERE name=N'{session}') BEGIN IF EXISTS(SELECT 1 FROM sys.dm_xe_sessions WHERE name=N'{session}') ALTER EVENT SESSION [{session}] ON SERVER STATE=STOP; DROP EVENT SESSION [{session}] ON SERVER; END;"
        ));
    }
    restore.push_str(&original.restore_sql());
    let restored = async {
        fixture.batch(&restore).await?;
        if OriginalTarget::read(&fixture.admitted).await? != original {
            return Err(invalid_data());
        }
        Ok(())
    }
    .await;
    let written = save_cost_receipt(
        &directory,
        &format!("{name}-fixture"),
        &serde_json::json!({
            "case":name,"restored_original_target":restored.is_ok(),"additional_session_count":fixture.additional_sessions.len(),
            "case_error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "body_panicked":outcome.is_err(),"owned_file_cleanup":"disposable fixture container teardown",
            "original_file_target_parameters":{"max_file_size":original.options.max_file_size,"max_rollover_files":original.options.max_rollover_files},
            "administrative_fixture_setup_in_source_metrics":false,"production_admission":"unqualified",
            "native_cpu_io_cost_qualified":false,"server_file_scan_work_bounded":false,
        }),
    );
    restored?;
    written?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Rec, Val};

    fn field(name: &str, value: &str) -> Rec {
        Rec::of(&[("name", Val::Text(name.into())), ("value", Val::Text(value.into()))])
    }

    #[test]
    fn changing_the_owned_filename_preserves_native_parameters_and_exact_original_state() {
        let original = OriginalTarget {
            options: TargetOptions::from_rows(vec![
                field("filename", r"C:\SQL logs\original's.xel"),
                field("max_file_size", "16"),
                field("max_rollover_files", "0"),
            ])
            .unwrap(),
            directory: r"C:\SQL logs\".into(),
            running: false,
        };
        let installed = original.install_sql("/native/nonce.xel");
        assert!(installed.contains("SET filename=N'/native/nonce.xel',max_file_size=(16),max_rollover_files=(0)"));
        let restored = original.restore_sql();
        assert!(
            restored.contains(r"SET filename=N'C:\SQL logs\original''s.xel',max_file_size=(16),max_rollover_files=(0)")
        );
        assert!(!restored.contains("STATE=START"));
        let running = OriginalTarget { running: true, ..original };
        assert!(running.restore_sql().contains("STATE=START"));
    }

    #[test]
    fn omitted_and_zero_native_parameters_remain_distinct_and_unrestorable_fields_are_refused() {
        let absent = TargetOptions::from_rows(vec![field("filename", "original.xel")]).unwrap();
        assert_eq!(absent.with_filename("owned.xel"), "filename=N'owned.xel'");
        let zero =
            TargetOptions::from_rows(vec![field("filename", "original.xel"), field("max_file_size", "0")]).unwrap();
        assert_eq!(zero.with_filename("owned.xel"), "filename=N'owned.xel',max_file_size=(0)");
        for rows in [
            vec![],
            vec![field("filename", "a.xel"), field("filename", "b.xel")],
            vec![field("filename", "a.xel"), field("unknown_target_parameter", "4")],
            vec![field("filename", "a.xel"), field("max_rollover_files", "not-an-integer")],
        ] {
            assert!(TargetOptions::from_rows(rows).is_err());
        }
    }
}
