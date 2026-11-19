//! A round keeps one native counter reader through both endpoints and its confirmed close.
use super::invalid;
use crate::{
    index_operations::{self, Layout},
    local_source::Context,
    read::{NativeRead, Parameter, ReadError},
};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeMap;

const MAX_OWNED_DATABASE_INDEX_ROWS: usize = 256;
const MEASURED_OBJECTS: [&str; 3] = ["page_fixture_hot", "page_fixture_split", "page_fixture_select"];
const ROUND_QUERIES: u32 = 9;
// This owner spans the unchanged four-second actor window plus bounded endpoint/cleanup work. No native
// statement gets a longer timeout, and no production collection recipe receives this fixture-only envelope.
const ROUND_WINDOW_MS: u32 = 10_000;

pub(super) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: ROUND_QUERIES,
        statement_timeout_ms: 1_000,
        total_timeout_ms: ROUND_WINDOW_MS,
        ..Default::default()
    }
}

type Indexes = BTreeMap<String, MssqlIndexOperations>;

struct Started {
    source: Context,
    layout: Layout,
    indexes: Indexes,
}

pub(super) struct IndexMeasurement<R: NativeRead> {
    reader: R,
    port: u16,
    database: String,
    started: Option<Started>,
}

pub(in crate::activity::page_targets::native_tests) struct IndexWindow {
    before: Indexes,
    after: Indexes,
    context: NativeReadContext,
    queries: u32,
    metrics: ReadWorkMetrics,
}

impl IndexWindow {
    pub(in crate::activity::page_targets::native_tests) fn delta(
        &self,
        name: &str,
        counter: MssqlIndexCounter,
    ) -> Result<u64> {
        counter_delta(&self.before, &self.after, name, counter)
    }
    pub(in crate::activity::page_targets::native_tests) fn before(&self) -> &Indexes {
        &self.before
    }
    pub(in crate::activity::page_targets::native_tests) fn after(&self) -> &Indexes {
        &self.after
    }
    pub(in crate::activity::page_targets::native_tests) fn context(&self) -> &NativeReadContext {
        &self.context
    }
    pub(in crate::activity::page_targets::native_tests) fn queries(&self) -> u32 {
        self.queries
    }
    pub(in crate::activity::page_targets::native_tests) fn metrics(&self) -> &ReadWorkMetrics {
        &self.metrics
    }
}

pub(in crate::activity::page_targets::native_tests) fn counter_delta(
    before: &Indexes,
    after: &Indexes,
    name: &str,
    counter: MssqlIndexCounter,
) -> Result<u64> {
    let before = before.get(name).ok_or_else(|| invalid("native index was absent before work"))?;
    let after = after.get(name).ok_or_else(|| invalid("native index was absent after work"))?;
    if !same_index_target(before, after) {
        return Err(invalid("native index generation changed during fixture measurement"));
    }
    let value = |row: &MssqlIndexOperations| {
        row.counter(counter)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| invalid("required native index counter is unknown"))
    };
    value(after)?.checked_sub(value(before)?).ok_or_else(|| invalid("native index counter reset during fixture work"))
}

impl<R: NativeRead> IndexMeasurement<R> {
    pub(super) fn new(reader: R, port: u16, database: String) -> Self {
        Self { reader, port, database, started: None }
    }

    pub(super) async fn begin(&mut self) -> Result<()> {
        if self.started.is_some() || self.reader.queries() != 0 {
            return Err(invalid("owned index measurement cannot reuse an earlier native window"));
        }
        let source = Context::read(&mut self.reader, "127.0.0.1", self.port).await?;
        self.qualify_source(&source)?;
        let layout = Layout::read(&mut self.reader).await?;
        let indexes = read_indexes(&mut self.reader, &layout).await?;
        qualify_indexes(&indexes, source.database_id)?;
        self.started = Some(Started { source, layout, indexes });
        Ok(())
    }

    pub(super) async fn finish(&mut self) -> Result<IndexWindow> {
        let result = self.final_endpoint().await;
        let shutdown = self.close().await;
        if shutdown != SourceShutdown::Quiescent {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        let (after, context) = result?;
        let metrics = self.reader.read_metrics().ok_or_else(|| invalid("owned index read accounting was absent"))?;
        let queries = self.reader.queries();
        qualify_accounting(&metrics, queries)?;
        let before = self.started.as_ref().ok_or_else(|| invalid("owned index baseline was absent"))?.indexes.clone();
        Ok(IndexWindow { before, after, context, queries, metrics })
    }

    async fn final_endpoint(&mut self) -> Result<(Indexes, NativeReadContext)> {
        let started = self.started.as_ref().ok_or_else(|| invalid("owned index measurement was not started"))?;
        let after = read_indexes(&mut self.reader, &started.layout).await?;
        let source = started.source.recheck(&mut self.reader, "127.0.0.1", self.port).await?;
        self.qualify_source(&source)?;
        if !started.source.same_source(&source) {
            return Err(CollectionStop::SourceChanged.error());
        }
        qualify_indexes(&after, source.database_id)?;
        if started
            .indexes
            .iter()
            .any(|(name, before)| after.get(name).is_none_or(|after| !same_index_target(before, after)))
        {
            return Err(invalid("owned native index target membership or definition changed during work"));
        }
        Ok((after, started.source.completed(&source)?))
    }

    fn qualify_source(&self, source: &Context) -> Result<()> {
        if !source.is_qualified()
            || !matches!(source.activity.edition, 2..=4)
            || source.database_id <= 4
            || source.activity.database.database != self.database
            || !source.connection_generation().is_some_and(|generation| generation > 0)
        {
            return Err(invalid(
                "owned index measurement lacks its native database/member/process/connection authority",
            ));
        }
        Ok(())
    }

    pub(super) async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}

pub(in crate::activity::page_targets::native_tests) fn same_index_target(
    before: &MssqlIndexOperations,
    after: &MssqlIndexOperations,
) -> bool {
    before.identity == after.identity
        && before.schema == after.schema
        && before.object == after.object
        && before.index == after.index
        && before.optimize_for_sequential_key == after.optimize_for_sequential_key
        && before.reset == after.reset
}

fn qualify_accounting(metrics: &ReadWorkMetrics, queries: u32) -> Result<()> {
    if queries != ROUND_QUERIES
        || !metrics.is_valid()
        || metrics.has_unconfirmed_work()
        || metrics.untracked_tasks != 0
        || metrics
            .templates
            .iter()
            .any(|template| template.failures != 0 || template.timeouts != 0 || template.cancellations != 0)
        || metrics.templates.iter().map(|template| u64::from(template.completed_tasks)).sum::<u64>()
            != u64::from(queries)
    {
        return Err(invalid("owned index window did not settle its exact successful native reads"));
    }
    Ok(())
}

fn qualify_indexes(indexes: &Indexes, database_id: u32) -> Result<()> {
    if indexes.len() != MEASURED_OBJECTS.len()
        || MEASURED_OBJECTS.iter().any(|name| {
            indexes.get(*name).is_none_or(|row| {
                row.identity.database_id != database_id
                    || row.identity.index_id != 1
                    || row.identity.partition_number != 1
                    || row.schema.as_deref() != Some("dbo")
                    || row.object.as_deref() != Some(*name)
                    || row.index.as_deref().is_none_or(str::is_empty)
                    || row.object_type.as_deref() != Some("U")
                    || row.engine_internal != Some(false)
                    || [
                        MssqlIndexCounter::LeafInserts,
                        MssqlIndexCounter::PageLatchWaits,
                        MssqlIndexCounter::PageLatchWaitMs,
                    ]
                    .into_iter()
                    .any(|counter| row.counter(counter).is_none())
            })
        })
    {
        return Err(invalid("owned native counter endpoint did not establish its exact primary-index roster"));
    }
    Ok(())
}

/// The ordinary fixture observation and the retained window share the provider projection and decoder.
pub(super) async fn read_indexes<R: NativeRead>(reader: &mut R, layout: &Layout) -> Result<Indexes> {
    let rows = reader
        .query(
            &index_operations::select_sql(layout),
            vec![Parameter::Integer((MAX_OWNED_DATABASE_INDEX_ROWS + 1) as i64)],
            MAX_OWNED_DATABASE_INDEX_ROWS + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_OWNED_DATABASE_INDEX_ROWS {
        return Err(invalid("owned native index population exceeded its explicit bound"));
    }
    let mut measured = BTreeMap::new();
    for row in &rows {
        let native = index_operations::decode(row, layout)?;
        if native.schema.as_deref() != Some("dbo") {
            continue;
        }
        let Some(name) = native.object.as_deref().filter(|name| MEASURED_OBJECTS.contains(name)) else { continue };
        if measured.insert(name.to_owned(), native).is_some() {
            return Err(invalid("owned fixture unexpectedly has multiple native index rows per object"));
        }
    }
    if measured.len() != MEASURED_OBJECTS.len() {
        return Err(invalid("owned native index population omitted a required fixture object"));
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activity::page_targets::tests::boxed_context,
        index_operations::tests::{layout_row, row},
        instance::test_support::{epoch, metadata},
        read::ReadResult,
        read_metrics::Metrics,
        resources::test_support::{FakeRead, replace},
        value::{Rec, Val},
    };
    use culprit_core::{Engine, ErrorCode, telemetry::ReadTaskOutcome};
    use std::time::Duration;

    struct Reader {
        inner: FakeRead,
        generation: Option<u64>,
        reconnect_at: Option<usize>,
        metrics: Metrics,
        edit_metrics: Option<fn(&mut ReadWorkMetrics)>,
        omit_metrics: bool,
        pause_close: bool,
    }

    impl Reader {
        fn new(replies: Vec<ReadResult<Vec<Rec>>>) -> Self {
            Self {
                inner: FakeRead::new(replies),
                generation: Some(7),
                reconnect_at: None,
                metrics: Metrics::new(Engine::Mssql),
                edit_metrics: None,
                omit_metrics: false,
                pause_close: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl NativeRead for Reader {
        async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
            let task = self.metrics.begin(sql);
            let result = self.inner.query(sql, parameters, limit).await;
            self.metrics.finish(
                task.as_deref(),
                Duration::ZERO,
                &crate::read_metrics::Progress::default(),
                &ReadTaskOutcome { failed: result.is_err(), ..Default::default() },
            );
            result
        }
        fn queries(&self) -> u32 {
            self.inner.queries()
        }
        fn connection_generation(&self) -> Option<u64> {
            self.generation.map(|generation| {
                generation + u64::from(self.reconnect_at.is_some_and(|at| self.inner.calls.len() >= at))
            })
        }
        fn read_metrics(&self) -> Option<ReadWorkMetrics> {
            if self.omit_metrics {
                return None;
            }
            let mut metrics = self.metrics.snapshot()?;
            if let Some(edit) = self.edit_metrics {
                edit(&mut metrics);
            }
            Some(metrics)
        }
        async fn close(&mut self) -> SourceShutdown {
            if self.pause_close {
                self.pause_close = false;
                std::future::pending::<()>().await;
            }
            self.inner.close().await
        }
    }

    fn indexes() -> Vec<Rec> {
        MEASURED_OBJECTS
            .iter()
            .enumerate()
            .map(|(ordinal, name)| {
                replace(
                    &row(),
                    &[
                        ("database_id", Val::Int(7)),
                        ("object_id", Val::Int(100 + ordinal as i64)),
                        ("object_name", Val::Text((*name).into())),
                        ("index_name", Val::Text(format!("PK_{name}"))),
                    ],
                )
            })
            .collect()
    }

    fn replies() -> Vec<ReadResult<Vec<Rec>>> {
        vec![
            Ok(vec![metadata()]),
            Ok(vec![epoch(100)]),
            Ok(vec![boxed_context()]),
            Ok(vec![layout_row(true, &[])]),
            Ok(indexes()),
            Ok(indexes()),
            Ok(vec![replace(&metadata(), &[("observed_at", Val::Text("2020-01-01T06:00:01Z".into()))])]),
            Ok(vec![epoch(100)]),
            Ok(vec![replace(&boxed_context(), &[("observed_at", Val::Text("2020-01-01T06:00:01Z".into()))])]),
        ]
    }

    fn measurement(reader: Reader) -> IndexMeasurement<Reader> {
        IndexMeasurement::new(reader, 1433, "shop".into())
    }

    #[tokio::test]
    async fn one_owner_keeps_both_native_endpoints_and_exact_successful_reads_until_close() {
        let mut owner = measurement(Reader::new(replies()));
        owner.begin().await.unwrap();
        assert_eq!(owner.reader.inner.closed, 0);
        let window = owner.finish().await.unwrap();
        assert_eq!(owner.reader.inner.closed, 1);
        assert_eq!(window.queries(), ROUND_QUERIES);
        assert!(window.context().has_qualified_local_owner());
        assert_eq!(window.context().observed_from, "2020-01-01T06:00:00.000000000Z");
        assert_eq!(window.context().observed_until, "2020-01-01T06:00:01.000000000Z");
        assert_eq!(window.before(), window.after());
        assert!(window.before().values().all(|row| row.reset == culprit_core::ResetMarker::Unknown));
        let before = &owner.reader.inner.calls[4];
        let after = &owner.reader.inner.calls[5];
        assert_eq!(before.sql, after.sql);
        assert_eq!(before.limit, MAX_OWNED_DATABASE_INDEX_ROWS + 1);
        assert!(matches!(&before.parameters[..], [Parameter::Integer(257)]));
        assert_eq!(window.metrics().templates.iter().map(|task| task.completed_tasks).sum::<u32>(), ROUND_QUERIES);
    }

    #[tokio::test]
    async fn process_database_connection_and_native_clock_changes_cannot_publish_a_counter_window() {
        for change in ["process", "fork", "database", "connection", "clock"] {
            let mut supplied = replies();
            match change {
                "process" => supplied[7] = Ok(vec![epoch(200)]),
                "fork" => {
                    supplied[8] = Ok(vec![replace(
                        &supplied[8].as_ref().unwrap()[0],
                        &[("recovery_fork_guid", Val::Text("changed-fork".into()))],
                    )]);
                }
                "database" => {
                    supplied[8] =
                        Ok(vec![replace(&supplied[8].as_ref().unwrap()[0], &[("selected_database_id", Val::Int(8))])]);
                }
                "clock" => {
                    for phase in [6, 8] {
                        supplied[phase] = Ok(vec![replace(
                            &supplied[phase].as_ref().unwrap()[0],
                            &[("observed_at", Val::Text("2020-01-01T05:59:59Z".into()))],
                        )]);
                    }
                }
                _ => {}
            }
            let mut reader = Reader::new(supplied);
            if change == "connection" {
                reader.reconnect_at = Some(6);
            }
            let mut owner = measurement(reader);
            owner.begin().await.unwrap();
            assert!(owner.finish().await.is_err(), "{change}");
            assert_eq!(owner.reader.inner.closed, 1, "{change}");
            assert_eq!(owner.reader.queries(), ROUND_QUERIES, "{change}: no retries");
        }
    }

    #[tokio::test]
    async fn unknown_authority_or_connection_is_refused_before_layout_and_counters() {
        for change in ["authority", "connection", "database-name"] {
            let mut supplied = replies();
            if change == "authority" {
                supplied[2] = Ok(vec![replace(&boxed_context(), &[("database_guid", Val::Null)])]);
            }
            let mut reader = Reader::new(supplied);
            if change == "connection" {
                reader.generation = None;
            }
            let mut owner = measurement(reader);
            if change == "database-name" {
                owner.database = "another-owned-database".into();
            }
            assert!(owner.begin().await.is_err(), "{change}");
            assert_eq!(owner.reader.queries(), 3, "{change}");
            assert_eq!(owner.close().await, SourceShutdown::Quiescent);
        }
    }

    #[tokio::test]
    async fn missing_duplicate_foreign_or_changed_targets_do_not_become_a_complete_endpoint() {
        for change in [
            "missing",
            "duplicate",
            "foreign-database",
            "object",
            "partition",
            "hobt",
            "index-name",
            "rows",
            "unknown-counter",
            "negative-counter",
        ] {
            let mut supplied = replies();
            let mut rows = indexes();
            match change {
                "missing" => {
                    rows.pop();
                }
                "duplicate" => rows.push(rows[0].clone()),
                "rows" => rows = vec![rows[0].clone(); MAX_OWNED_DATABASE_INDEX_ROWS + 1],
                field => {
                    let (field, value) = match field {
                        "foreign-database" => ("database_id", Val::Int(8)),
                        "object" => ("object_id", Val::Int(999)),
                        "partition" => ("partition_number", Val::Int(2)),
                        "hobt" => ("hobt_id", Val::Int(999)),
                        "unknown-counter" => ("page_latch_wait_in_ms", Val::Null),
                        "negative-counter" => ("page_latch_wait_in_ms", Val::Int(-1)),
                        _ => ("index_name", Val::Text("replaced-index".into())),
                    };
                    rows[0] = replace(&rows[0], &[(field, value)]);
                }
            }
            supplied[5] = Ok(rows);
            let mut owner = measurement(Reader::new(supplied));
            owner.begin().await.unwrap();
            assert!(owner.finish().await.is_err(), "{change}");
            assert_eq!(owner.reader.inner.closed, 1, "{change}");
        }
    }

    #[tokio::test]
    async fn failed_reads_and_unconfirmed_close_never_publish_a_successful_window() {
        for uncertain_close in [false, true] {
            let mut supplied = replies();
            if !uncertain_close {
                supplied[5] = Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture")));
            }
            let mut reader = Reader::new(supplied);
            if uncertain_close {
                reader.inner.shutdown = SourceShutdown::Unconfirmed;
            }
            let mut owner = measurement(reader);
            owner.begin().await.unwrap();
            let error = owner.finish().await.err().unwrap();
            if uncertain_close {
                assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
            } else {
                assert_eq!(error.code, ErrorCode::PermissionDenied);
                assert_eq!(owner.reader.queries(), 6, "no permission retry or source reread");
            }
            assert_eq!(owner.reader.inner.closed, 1);
        }
    }

    #[tokio::test]
    async fn missing_failed_active_or_untracked_accounting_cannot_authorize_success() {
        for edit in [
            None,
            Some((|metrics: &mut ReadWorkMetrics| metrics.active_tasks = 1) as fn(&mut ReadWorkMetrics)),
            Some(|metrics: &mut ReadWorkMetrics| metrics.untracked_tasks = 1),
            Some(|metrics: &mut ReadWorkMetrics| metrics.templates[0].failures = 1),
            Some(|metrics: &mut ReadWorkMetrics| {
                metrics.templates[0].failures = 1;
                metrics.templates[0].timeouts = 1;
            }),
            Some(|metrics: &mut ReadWorkMetrics| metrics.templates[0].completed_tasks += 1),
        ] {
            let mut reader = Reader::new(replies());
            reader.omit_metrics = edit.is_none();
            reader.edit_metrics = edit;
            let mut owner = measurement(reader);
            owner.begin().await.unwrap();
            assert!(owner.finish().await.is_err());
            assert_eq!(owner.reader.inner.closed, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_endpoint_or_close_keeps_the_same_reader_available_to_its_cleanup_owner() {
        for phase in ["begin", "finish", "close"] {
            let mut reader = Reader::new(replies());
            if phase != "close" {
                reader.inner = reader.inner.pause_on_query(if phase == "begin" { 5 } else { 6 });
            }
            reader.pause_close = phase == "close";
            let mut owner = measurement(reader);
            if phase == "begin" {
                assert!(tokio::time::timeout(Duration::from_millis(1), owner.begin()).await.is_err());
                assert!(owner.started.is_none());
            } else {
                owner.begin().await.unwrap();
                assert!(tokio::time::timeout(Duration::from_millis(1), owner.finish()).await.is_err());
                assert!(owner.started.is_some());
            }
            assert_eq!(owner.reader.inner.closed, 0, "{phase}: cancellation is not a completed close");
            assert_eq!(owner.close().await, SourceShutdown::Quiescent);
            assert_eq!(owner.reader.inner.closed, 1, "{phase}");
        }
    }
}
