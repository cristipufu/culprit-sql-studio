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
                partition: culprit_core::telemetry::SourcePartition::Target,
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
    .chain(node_statement_work_evidence_cases())
    .chain(statement_outcome_evidence_cases())
    .collect()
}

/// Source-declared outcome counts use exactly the MySQL/MariaDB native schema/digest identity and unknown reset.
pub fn statement_outcome_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [
        ("app", Some("9007199254740993"), Some("0"), true),
        ("other", Some("18446744073709551615"), Some("7"), false),
        ("app", None, Some("18446744073709551615"), true),
        ("app", None, None, false),
    ]
    .into_iter()
    .map(|(database, errors, warnings, complete)| {
        let source = "performance_schema.events_statements_summary_by_digest";
        let mut totals = vec![metric(RuntimeMetricKey::Executions, RuntimeMetricUnit::Count, "17")];
        for (key, value) in [(RuntimeMetricKey::Errors, errors), (RuntimeMetricKey::Warnings, warnings)] {
            if let Some(value) = value {
                totals.push(metric(key, RuntimeMetricUnit::Count, value));
            }
        }
        let sample = StatementCounterSample {
            member: None,
            identity: NativeQueryIdentity {
                query_id: "0123456789abcdef0123456789abcdef".into(),
                qualifiers: [("schema".into(), database.into())].into(),
            },
            scope: culprit_core::MeasurementScope::Database,
            observed_at: "2020-01-01T12:00:00.123456Z".into(),
            reset: culprit_core::ResetMarker::Unknown,
            totals,
            distributions: vec![],
        };
        let records = vec![TelemetryRecord::Operational(OperationalObservation {
            source: source.into(),
            key: sample.key().unwrap(),
            fact: OperationalFact::StatementCounters(Box::new(sample)),
        })];
        let description = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mysql,
                database,
                "fixture.invalid",
                3306,
                None,
            )
            .into(),
            source: "mysql-statement-counters".into(),
            partition: SourcePartition::Target,
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementCounters,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(source.into(), 1, complete)],
            }),
        };
        (description, records)
    })
    .collect()
}

/// Distributed statement points retain one observed node and every native query-population dimension.
pub fn node_statement_work_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let node_a = "0123456789abcdef0123456789abcdef";
    let node_b = "fedcba9876543210fedcba9876543210";
    [
        (
            node_a,
            "native-process-a",
            vec![("10", true, Some("9007199254740993")), ("11", true, Some("0")), ("10", false, None)],
            true,
        ),
        (node_b, "native-process-b", vec![("10", true, Some("18446744073709551615"))], true),
        (node_a, "native-process-a", vec![("10", true, Some("7"))], false),
    ]
    .into_iter()
    .map(|(node, epoch, populations, complete)| {
        let source_name = "ysql-node-pg-stat-statements";
        let rows = populations
            .into_iter()
            .enumerate()
            .map(|(ordinal, (user, toplevel, written))| {
                let mut totals = vec![metric(RuntimeMetricKey::Executions, RuntimeMetricUnit::Count, "17")];
                if let Some(written) = written {
                    totals.push(metric(
                        RuntimeMetricKey::TemporaryBlocksRead,
                        RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                        "0",
                    ));
                    totals.push(metric(
                        RuntimeMetricKey::TemporaryBlocksWritten,
                        RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                        written,
                    ));
                    totals.push(StatementCounter {
                        key: RuntimeMetricKey::TemporaryReadTime,
                        unit: RuntimeMetricUnit::Milliseconds,
                        total: MeasurementValue::Real(0.0),
                    });
                }
                let sample = StatementCounterSample {
                    member: culprit_core::engines::ysql_node_member(node),
                    identity: NativeQueryIdentity {
                        query_id: "-42".into(),
                        qualifiers: [
                            ("database_oid".into(), "16384".into()),
                            ("userid".into(), user.into()),
                            ("node".into(), node.into()),
                            ("toplevel".into(), toplevel.to_string()),
                        ]
                        .into(),
                    },
                    scope: culprit_core::MeasurementScope::Node,
                    observed_at: format!("2020-01-01T12:00:00.{}00000000Z", ordinal + 1),
                    reset: culprit_core::ResetMarker::Unknown,
                    totals,
                    distributions: vec![],
                };
                assert!(sample.is_valid());
                TelemetryRecord::Operational(OperationalObservation {
                    source: source_name.into(),
                    key: sample.key().unwrap(),
                    fact: OperationalFact::StatementCounters(Box::new(sample)),
                })
            })
            .collect::<Vec<_>>();
        let source = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Yugabyte,
                "db",
                "fixture.invalid",
                5433,
                None,
            )
            .into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(culprit_core::engines::ysql_node_member(node).unwrap()),
                epoch: Some(epoch.into()),
            },
            source: source_name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementCounters,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: rows.len() as u32,
                sources: vec![StateSourceCoverage::observed(source_name.into(), rows.len() as u32, complete)],
            }),
        };
        assert!(source.partition_is_consistent());
        (source, rows)
    })
    .collect()
}

fn metric(key: RuntimeMetricKey, unit: RuntimeMetricUnit, value: &str) -> StatementCounter {
    StatementCounter { key, unit, total: MeasurementValue::Integer(value.into()) }
}

fn record(index: usize, totals: Vec<StatementCounter>) -> TelemetryRecord {
    let sample = StatementCounterSample {
        member: None,
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
