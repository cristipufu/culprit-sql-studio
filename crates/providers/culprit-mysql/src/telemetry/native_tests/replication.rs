//! Owned SHOW conformance, not production cost qualification or proof of advancing/applying replication.
//! Setup, privileges and final container cleanup belong to dev/smoke-providers.py; no test mutates its fixture.
//! Row/page caps bound retained client evidence; they do not establish bounded server enumeration work for SHOW.
//! MySQL monitoring privilege: https://dev.mysql.com/doc/refman/8.4/en/privileges-provided.html
//! MariaDB monitoring privilege: https://mariadb.com/docs/server/reference/sql-statements/administrative-sql-statements/show/show-replica-status
use crate::{
    connect::{Flavor, Scope},
    native,
    read::{QueryRead, ReadSession},
    rows::Rec,
};
use culprit_core::{ErrorCode, MeasurementScope, Provider, Result, telemetry::*};
use fixture::{Fixture, Login, budget};
pub(super) use fixture::{required, valid_owner};

pub(super) mod capture;
mod fixture;
mod streaming;

#[test]
fn empirical_replication_conformance_does_not_promote_the_recipe_cost_qualification() {
    let recipe = super::super::recipes()
        .into_iter()
        .find(|recipe| {
            recipe.selector == RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Replication))
        })
        .expect("native SHOW recipe");
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
}

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB channels and minimal monitor; run through the smoke supervisor"]
async fn minimal_monitor_captures_multiple_real_channels_with_native_clocks_and_strict_receipts() {
    let fixture = Fixture::open().await;
    let captured = capture::collect(&fixture, Login::Monitor, budget()).await;
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    let source = captured.manifest.description.as_ref().unwrap();
    assert_eq!(source.partition, SourcePartition::Target, "SHOW does not acquire a physical process generation");
    assert_eq!(source.continuity, SourceContinuity::Unverified);
    let frame = source.observation.snapshot().unwrap();
    assert_eq!(frame.dataset, SnapshotDataset::Replication);
    assert_eq!(frame.records, 2);
    assert_eq!(frame.sources.len(), 1);
    assert_eq!(frame.sources[0].records, 2);
    assert_eq!(frame.sources[0].state, culprit_core::SourceState::Collected);
    let samples = capture::samples(&captured.records);
    assert_native_channels(&fixture, &samples);
    let show = capture::show_work(
        captured.manifest.read_metrics.as_ref().unwrap(),
        fixture.native.flavor,
        &fixture.native.version,
    );
    assert_eq!(show.failures, 0);
    assert_eq!(show.rows_decoded, "2");
    let points = captured
        .records
        .iter()
        .map(|row| match &row.record {
            TelemetryRecord::Replication(point) => point.clone(),
            _ => panic!("original native replication point"),
        })
        .collect();
    let replay =
        required(ReplicationSnapshot::from_source(source, points), "captured native replication reconstruction");
    assert!(replay.is_valid());
    assert_eq!(replay.sources, frame.sources, "reconstruction preserves original native coverage");
    let context = replay.context.native().expect("context derives only from the captured source");
    assert_eq!(context.target, source.target);
    assert_eq!(context.partition, source.partition);
    assert!(context.partition.is_qualified(), "logical Target qualifies its declared source namespace");
    assert_eq!(context.partition, SourcePartition::Target, "no process epoch was acquired by SHOW");
    assert!(
        !context.has_qualified_local_owner(),
        "logical source qualification does not manufacture a physical member"
    );
    assert_eq!(source.continuity, SourceContinuity::Unverified);
    assert_eq!(context.database_writable, None, "snapshot replay cannot add an unrecorded role");
    eprintln!(
        "replication SHOW conformance: flavor={:?} version={} performance_schema={} native_rows=2 source_queries={}; client metrics are not native CPU/I/O or production cost qualification",
        fixture.native.flavor,
        fixture.native.version,
        fixture.native.performance_schema,
        captured.manifest.source_queries
    );
}

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB channels and minimal monitor; run through the smoke supervisor"]
async fn the_scan_buffered_show_uses_the_same_real_fields_without_process_or_instrumentation_privileges() {
    let fixture = Fixture::open().await;
    let provider = fixture.provider(Login::Monitor);
    let mut reader = ReadSession::new(provider.connector.clone(), budget(), Scope::Server);
    let context = native::metadata::read(&mut reader, &provider.connector).await;
    let (result, raw) = {
        let mut observed = ShowRows { reader: &mut reader, native: vec![] };
        let result = match context.as_ref() {
            Ok(context) => Some(
                native::replication::read(&mut observed, context.connection.flavor, &context.connection.version).await,
            ),
            Err(_) => None,
        };
        (result, observed.native)
    };
    let metrics = reader.read_metrics();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let context = required(context, "minimal-monitor metadata");
    assert!(
        !context.process_wide && !context.database_wide,
        "fixture monitor has only table SELECT and replication monitoring"
    );
    let rows = required(result.expect("metadata admitted the actual SHOW"), "buffered native replication SHOW");
    let samples = rows
        .iter()
        .map(|point| {
            assert_eq!(point.source, native::replication::SOURCE);
            let ReplicationFact::Native(sample) = &point.fact else { panic!("native SHOW channel") };
            sample.as_ref()
        })
        .collect::<Vec<_>>();
    assert_native_channels(&fixture, &samples);
    assert_eq!(raw.len(), samples.len());
    for (original, sample) in raw.iter().zip(&samples) {
        for (native, key) in original.counters.iter().zip(["lag-seconds", "remaining-delay", "configured-delay"]) {
            let original = native.map(|value| value.to_string());
            assert_eq!(counter(sample, key), original.as_deref(), "native NULL and zero remain different");
        }
        for (clock, key) in
            original.error_clocks.iter().zip(["io_error_at_without_offset", "sql_error_at_without_offset"])
        {
            assert_eq!(
                clock.present,
                fixture.native.flavor == Flavor::Mysql,
                "verify the actual native error-clock columns, not a guessed timestamp from another source"
            );
            assert_eq!(
                sample.measurements.properties.get(key).map(String::as_str),
                clock.value.as_deref().filter(|value| !value.is_empty()),
                "native error clocks stay exact; unsupported, NULL and empty clocks remain unavailable"
            );
        }
    }
    let metrics = metrics.unwrap();
    assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
    assert_eq!(capture::show_work(&metrics, fixture.native.flavor, &fixture.native.version).failures, 0);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB monitor and denied accounts; run through the smoke supervisor"]
async fn denied_native_show_preserves_the_terminal_failure_without_alias_retry_or_healthy_empty() {
    let fixture = Fixture::open().await;
    let captured = capture::denied(&fixture).await;
    assert_eq!(captured.manifest.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable));
    assert!(captured.records.is_empty());
    let show = capture::show_work(
        captured.manifest.read_metrics.as_ref().unwrap(),
        fixture.native.flavor,
        &fixture.native.version,
    );
    assert_eq!(show.failures, 1);
    assert_eq!(show.rows_decoded, "0");
    assert!(captured.manifest.files.iter().all(|file| file.rows == 0 && file.status != CollectionStatus::Complete));
    let provider = fixture.provider(Login::Denied);
    let mut reader = ReadSession::new(provider.connector.clone(), budget(), Scope::Server);
    let result = native::replication::read(&mut reader, fixture.native.flavor, &fixture.native.version).await;
    let metrics = reader.read_metrics();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let error = match result {
        Ok(_) => panic!("denied SHOW must never become a successful empty population"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(capture::show_work(&metrics.unwrap(), fixture.native.flavor, &fixture.native.version).failures, 1);
}

fn assert_native_channels(fixture: &Fixture, samples: &[&NativeReplicationSample]) {
    assert_eq!(samples.len(), 2);
    let mut channels =
        samples.iter().map(|sample| sample.measurements.properties["channel"].clone()).collect::<Vec<_>>();
    channels.sort();
    let mut expected = fixture.channels.clone();
    expected.sort();
    assert_eq!(channels, expected);
    let mut errors = 0;
    let mut no_errors = 0;
    for sample in samples {
        let set = &sample.measurements;
        assert_eq!(set.scope, MeasurementScope::Server);
        assert!(sample.session.is_none() && sample.unresolved_session_id.is_none());
        assert_eq!(set.properties.get("source_host").map(String::as_str), Some("127.0.0.1"));
        assert!(
            set.counters.iter().all(|counter| counter.scope == MeasurementScope::Server
                && counter.reset == culprit_core::ResetMarker::Unknown)
        );
        assert_eq!(counter(sample, "configured-delay"), Some("0"));
        let read = set
            .counters
            .iter()
            .find(|counter| counter.key == "received-source-log-position")
            .expect("native read position");
        let current_mysql = fixture.native.flavor == Flavor::Mysql
            && culprit_core::version_at_least(&fixture.native.version, [8, 0, 22]);
        assert_eq!(read.native_name, if current_mysql { "read_source_log_pos" } else { "read_master_log_pos" });
        if counter(sample, "io-error-code") == Some("0") {
            no_errors += 1;
            assert!(!set.properties.contains_key("io_error_at_without_offset"));
        } else {
            errors += 1;
            assert!(
                counter(sample, "io-error-code")
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|code| code > 0)
            );
            match fixture.native.flavor {
                Flavor::Mysql => {
                    let clock = set.properties.get("io_error_at_without_offset").expect("real native IO failure clock");
                    assert_eq!(clock.len(), 15);
                    assert!(!clock.ends_with('Z') && !clock.contains('+'));
                }
                Flavor::Mariadb => {
                    // MariaDB11.8 SHOW declares numeric errors but no Last_IO/SQL_Error_Timestamp.
                    // Its Master/Slave_last_event_time fields are event clocks, not error timestamps.
                    // https://github.com/MariaDB/server/blob/11.8/sql/sql_show.cc (slave_status_fields_info)
                    assert!(!set.properties.contains_key("io_error_at_without_offset"));
                    assert!(!set.properties.contains_key("sql_error_at_without_offset"));
                }
            }
        }
        for private in [
            "source_user",
            "master_user",
            "source_password",
            "master_password",
            "last_io_error",
            "last_sql_error",
            "retrieved_gtid_set",
            "executed_gtid_set",
        ] {
            assert!(!set.properties.contains_key(private), "native private transcript is outside the source allowlist");
        }
    }
    assert_eq!((no_errors, errors), (1, 1), "supervisor supplied one stopped and one real IO-error channel");
}

/// Observes the exact existing SHOW result in memory; it adds no native query or independent decoder.
struct ShowRows<'a> {
    reader: &'a mut ReadSession,
    native: Vec<NativeFields>,
}

struct NativeFields {
    counters: [Option<u64>; 3],
    error_clocks: [NativeClock; 2],
}

struct NativeClock {
    present: bool,
    value: Option<String>,
}

impl NativeClock {
    fn read(row: &Rec, field: &str) -> Result<Self> {
        let present = row.has_column(field);
        Ok(Self { present, value: if present { row.native_text(field)? } else { None } })
    }
}

#[async_trait::async_trait]
impl QueryRead for ShowRows<'_> {
    async fn read(&mut self, sql: &str, parameters: Vec<mysql_async::Value>, row_limit: usize) -> Result<Vec<Rec>> {
        let rows = self.reader.read(sql, parameters, row_limit).await?;
        self.native = rows
            .iter()
            .map(|row| {
                let lag = if row.has_column("seconds_behind_source") {
                    "seconds_behind_source"
                } else {
                    "seconds_behind_master"
                };
                Ok(NativeFields {
                    counters: [row.unsigned(lag)?, row.unsigned("sql_remaining_delay")?, row.unsigned("sql_delay")?],
                    error_clocks: [
                        NativeClock::read(row, "last_io_error_timestamp")?,
                        NativeClock::read(row, "last_sql_error_timestamp")?,
                    ],
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    async fn explain(&mut self, _: &str) -> Result<Option<Rec>> {
        panic!("replication conformance must not dispatch EXPLAIN")
    }
}

pub(super) fn counter<'a>(sample: &'a NativeReplicationSample, key: &str) -> Option<&'a str> {
    sample.measurements.counters.iter().find(|counter| counter.key == key).map(|counter| match &counter.value {
        MeasurementValue::Integer(value) => value.as_str(),
        MeasurementValue::Real(_) => panic!("SHOW native integers must remain exact integers"),
    })
}
