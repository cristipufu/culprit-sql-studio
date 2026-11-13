use super::*;

/// Native cumulative points keep exact values and reset uncertainty. No interval or query definition is supplied.
pub fn statement_work_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let tables = vec![
        metric(RuntimeMetricKey::TemporaryTables, RuntimeMetricUnit::Count, "18446744073709551615"),
        metric(RuntimeMetricKey::TemporaryDiskTables, RuntimeMetricUnit::Count, "9007199254740993"),
        metric(RuntimeMetricKey::SortMergePasses, RuntimeMetricUnit::Count, "0"),
    ];
    let blocks = vec![
        metric(RuntimeMetricKey::TemporaryBlocksRead, RuntimeMetricUnit::Pages { bytes_per_page: 8192 }, "0"),
        metric(
            RuntimeMetricKey::TemporaryBlocksWritten,
            RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
            "9007199254740993",
        ),
        StatementCounter {
            key: RuntimeMetricKey::TemporaryReadTime,
            unit: RuntimeMetricUnit::Microseconds,
            total: MeasurementValue::Real(1500.25),
        },
        metric(RuntimeMetricKey::TemporaryWriteTime, RuntimeMetricUnit::Nanoseconds, "0"),
    ];
    let mut mixed = tables.clone();
    mixed.extend(blocks.clone());
    let unknown =
        vec![metric(RuntimeMetricKey::Native("opaque-native-counter".into()), RuntimeMetricUnit::Count, "99")];
    [
        (
            culprit_core::Engine::Mysql,
            vec![tables, vec![metric(RuntimeMetricKey::TemporaryDiskTables, RuntimeMetricUnit::Count, "0")]],
            true,
            false,
        ),
        (culprit_core::Engine::Postgres, vec![blocks, unknown.clone()], true, false),
        (culprit_core::Engine::Yugabyte, vec![mixed, unknown], false, false),
        (culprit_core::Engine::Mysql, vec![], true, false),
        (culprit_core::Engine::Mysql, vec![], false, true),
    ]
    .into_iter()
    .map(|(engine, samples, complete, unavailable)| {
        let records = samples.into_iter().enumerate().map(|(index, totals)| record(index, totals)).collect::<Vec<_>>();
        let source = "native-statement-counters";
        let coverage = if unavailable {
            StateSourceCoverage::unavailable(source.into(), culprit_core::ErrorCode::PermissionDenied, None)
        } else {
            StateSourceCoverage::observed(source.into(), records.len() as u32, complete)
        };
        (
            CollectionDescription {
                target: culprit_core::DatabaseIdentity::server(engine, "db", "fixture.invalid", 5432, None).into(),
                source: "statement-counter-capture".into(),
                continuity: SourceContinuity::Unverified,
                observation: SourceObservation::Snapshot(StateSnapshotDescription {
                    dataset: SnapshotDataset::StatementCounters,
                    observed_from: "2020-01-01T12:00:00Z".into(),
                    observed_until: "2020-01-01T12:00:01Z".into(),
                    records: records.len() as u32,
                    sources: vec![coverage],
                }),
            },
            records,
        )
    })
    .collect()
}

fn metric(key: RuntimeMetricKey, unit: RuntimeMetricUnit, value: &str) -> StatementCounter {
    StatementCounter { key, unit, total: MeasurementValue::Integer(value.into()) }
}

fn record(index: usize, totals: Vec<StatementCounter>) -> TelemetryRecord {
    let sample = StatementCounterSample {
        identity: NativeQueryIdentity {
            query_id: format!("native-query-{index}"),
            qualifiers: [("opaque-context".into(), "native-qualified-identity".into())].into(),
        },
        scope: culprit_core::MeasurementScope::Database,
        observed_at: format!("2020-01-01T12:00:00.{}00000000Z", index + 1),
        reset: if index == 0 {
            culprit_core::ResetMarker::Unknown
        } else {
            culprit_core::ResetMarker::At("2020-01-01T10:00:00Z".into())
        },
        totals,
        distributions: vec![],
    };
    assert!(sample.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: "native-statement-counters".into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::StatementCounters(Box::new(sample)),
    })
}
