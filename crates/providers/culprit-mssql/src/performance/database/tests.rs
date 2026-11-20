use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    read::ReadResult,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) struct Reader {
    pub inner: FakeRead,
    pub generation: Option<u64>,
    pub change_at: Option<u32>,
}
impl Reader {
    pub fn new(replies: Vec<ReadResult<Vec<Rec>>>) -> Self {
        Self { inner: FakeRead::new(replies), generation: Some(1), change_at: None }
    }
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation.map(|before| before + u64::from(self.change_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
pub(crate) fn authority(at: &str) -> Rec {
    replace(&crate::activity::database_context::tests::row(3), &[("observed_at", Val::Text(at.into()))])
}
pub(crate) fn frame(name: &str) -> Rec {
    let row = replace(
        &super::super::tests::row(),
        &[
            ("counter_observed_at", Val::Text("2020-01-01T06:00:00.5000000Z".into())),
            ("native_object", Val::Text("SQLServer:Databases".into())),
            ("native_name", Val::Text(name.into())),
            ("native_instance", Val::Text("shop".into())),
        ],
    );
    let mut fields = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("selected_database_id", Val::Int(7)),
        ("selected_database", Val::Text("shop".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("catalog_visible", Val::Int(1)),
        ("catalog_matches", Val::Int(1)),
        ("database_created", Val::Text("2019-01-01T04:00:00.3066667".into())),
        ("database_guid", Val::Text("native-database-guid".into())),
        ("recovery_fork_guid", Val::Text("native-recovery-fork".into())),
        ("database_file_guid", Val::Text("native-file-guid".into())),
    ]);
    Rec::of(&fields)
}
pub(crate) fn frames() -> Vec<Rec> {
    ["Log Bytes Flushed/sec", "Log Flushes/sec", "Log Flush Wait Time"].map(frame).to_vec()
}
async fn owner() -> Context {
    let mut reader =
        Reader::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![authority("2020-01-01T06:00:00Z")])]);
    Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap()
}
#[tokio::test]
async fn selected_database_reuses_exact_decoder_and_local_native_identity_key_without_shared_totals() {
    let owner = owner().await;
    let mut reader = Reader::new(vec![Ok(frames())]);
    let read = read_current(&mut reader, 3, &owner).await.unwrap();
    assert_eq!(read.native_rows, 3);
    assert_eq!(read.sample.identity, owner.native_identity().unwrap());
    assert_eq!(read.sample.key().unwrap(), owner.capture_context().unwrap().epoch.unwrap());
    assert_eq!(read.sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert!(read.sample.counters.iter().all(|row| row.scope == MeasurementScope::Database
        && row.reset == ResetMarker::Unknown
        && row.native_instance.as_deref() == Some("shop")));
    assert!(read.sample.ratios.is_empty());
    assert_eq!(reader.queries(), 1);
    assert!(matches!(reader.inner.calls[0].parameters.as_slice(), [Parameter::Integer(7)]));
    assert_eq!(reader.inner.calls[0].limit, 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn counters_reject_foreign_or_hidden_catalog_name_role_database_generation_and_clock() {
    let owner = owner().await;
    for (field, value) in [
        ("native_instance", Val::Text("_Total".into())),
        ("native_instance", Val::Text("foreign".into())),
        ("selected_database_id", Val::Int(8)),
        ("selected_database", Val::Text("foreign".into())),
        ("catalog_matches", Val::Int(2)),
        ("catalog_visible", Val::Null),
        ("catalog_visible", Val::Int(0)),
        ("database_guid", Val::Null),
        ("database_guid", Val::Text("another".into())),
        ("database_file_guid", Val::Text("another".into())),
        ("recovery_fork_guid", Val::Text("another".into())),
        ("database_created", Val::Text("2019-01-01T04:00:00.3066668".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
        ("counter_observed_at", Val::Text("2019-01-01T00:00:00Z".into())),
    ] {
        let mut rows = frames();
        rows[1] = replace(&rows[1], &[(field, value)]);
        let mut reader = Reader::new(vec![Ok(rows)]);
        assert!(read_current(&mut reader, 3, &owner).await.is_err(), "{field}");
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn absent_native_counter_rows_and_duplicates_are_unavailable_while_unknown_type_stays_raw() {
    let owner = owner().await;
    for rows in [
        vec![],
        vec![replace(&frame("Log Bytes Flushed/sec"), &[("native_object", Val::Null)])],
        vec![frame("Log Bytes Flushed/sec"), frame("Log Bytes Flushed/sec")],
    ] {
        let mut reader = Reader::new(vec![Ok(rows)]);
        assert!(read_current(&mut reader, 3, &owner).await.is_err());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut reader =
        Reader::new(vec![Ok(vec![replace(&frame("Log Bytes Flushed/sec"), &[("native_type", Val::Int(42))])])]);
    let read = read_current(&mut reader, 3, &owner).await.unwrap();
    assert_eq!(read.sample.counters[0].nature, CounterNature::Unknown);
    assert_eq!(read.sample.counters[0].unit, CounterUnit::Unknown);
    assert_eq!(read.sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn connection_generation_and_conservative_name_admission_fail_before_reusing_an_original_frame() {
    let owner = owner().await;
    let mut zero = Reader::new(vec![]);
    assert_eq!(
        CollectionStop::from_error(&read_current(&mut zero, 0, &owner).await.unwrap_err()),
        CollectionStop::RowLimit
    );
    assert_eq!(zero.queries(), 0);
    assert_eq!(zero.close().await, SourceShutdown::Quiescent);
    for name in ["_Total".to_owned(), " shop".into(), "shop ".into(), "x".repeat(128), "𐐀".repeat(64)] {
        let mut owner = owner.clone();
        owner.activity.database.database = name;
        let mut reader = Reader::new(vec![]);
        assert!(read_current(&mut reader, 3, &owner).await.is_err());
        assert_eq!(reader.queries(), 0);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut reader = Reader::new(vec![Ok(frames())]);
    reader.change_at = Some(1);
    assert_eq!(
        CollectionStop::from_error(&read_current(&mut reader, 3, &owner).await.unwrap_err()),
        CollectionStop::SourceChanged
    );
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[test]
fn native_query_uses_one_bounded_catalog_witness_and_exact_binary_counter_instance_without_setup_or_full_counter_count()
{
    let sql = query(4).unwrap();
    for required in [
        "SELECT TOP (2) d.database_id",
        "SUBSTRING(CONVERT(varbinary(256),d.name),1,DATALENGTH(@selected_name))",
        "HAS_PERMS_BY_NAME(NULL,NULL,N'ALTER ANY DATABASE')",
        "SELECT TOP (4)",
        "@selected_id=@P1",
        "RTRIM(object_name) LIKE N'%:Databases'",
        "RTRIM(instance_name)",
        "DATABASEPROPERTYEX(DB_NAME(),N'Updateability')",
    ] {
        assert!(sql.contains(required), "{required}");
    }
    for forbidden in [
        "GRANT ",
        "ALTER DATABASE",
        "CREATE ",
        "USE ",
        "_Total",
        "DB_ID(instance_name)",
        "COUNT(*) FROM sys.dm_os_performance_counters",
    ] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert_eq!(sql.matches("SYSUTCDATETIME()").count(), 1);
    assert_eq!(sql.matches("DATABASEPROPERTYEX").count(), 1);
    assert!(!super::super::sql().contains("catalog_matches"), "shared _Total selection keeps its original query");
}
