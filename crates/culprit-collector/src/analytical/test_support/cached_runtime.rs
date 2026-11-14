//! Completed native statement work retains the exact cache identity and optional spill-page distribution.
use super::*;

pub fn cached_runtime_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [None, Some("0"), Some("9007199254740993")]
        .into_iter()
        .map(|spilled_pages| {
            let mut counters = CachedRuntimeMetric::ALL
                .into_iter()
                .filter(|metric| metric.is_required())
                .map(|metric| (metric, distribution("17")))
                .collect::<std::collections::BTreeMap<_, _>>();
            if let Some(pages) = spilled_pages {
                counters.insert(CachedRuntimeMetric::SpilledPages, distribution(pages));
            }
            let native = CachedStatementRuntime {
                context: CachedStatementContext {
                    identity: CachedStatementIdentity {
                        plan_handle: "0x01020304".into(),
                        sql_handle: "0x05060708".into(),
                        statement_start_bytes: 0,
                        statement_end_bytes: -1,
                        created_without_offset: "2020-01-01T11:00:00.003".into(),
                        plan_generation: "2".into(),
                    },
                    database_id: 7,
                    query_hash: Some("0x0123456789ABCDEF".into()),
                    origin: StatementOrigin::Application,
                    monitoring_template_id: None,
                    source_text_bytes: None,
                    source_text_complete: false,
                },
                query_plan_hash: Some("0xFEDCBA9876543210".into()),
                executions: Some("9007199254740993".into()),
                counters,
                last_execution_without_offset: Some("2020-01-01T11:59:00.007".into()),
                reset: culprit_core::ResetMarker::Unknown,
            };
            assert!(native.is_valid());
            let source_id = "native-completed-statement-work";
            let record = TelemetryRecord::Operational(OperationalObservation {
                source: source_id.into(),
                key: native.context.identity.key().unwrap(),
                fact: OperationalFact::MssqlCachedRuntime(Box::new(native)),
            });
            let (manifest, _) = fixture();
            let source = CollectionDescription {
                target: manifest.source.target,
                source: "statement-runtime-cache".into(),
                continuity: SourceContinuity::Unverified,
                observation: SourceObservation::Snapshot(StateSnapshotDescription {
                    dataset: SnapshotDataset::CachedRuntime,
                    observed_from: "2020-01-01T12:00:00Z".into(),
                    observed_until: "2020-01-01T12:00:01Z".into(),
                    records: 1,
                    sources: vec![StateSourceCoverage::collected(source_id.into(), 1)],
                }),
            };
            (source, vec![record])
        })
        .collect()
}

fn distribution(value: &str) -> CachedCounterDistribution {
    CachedCounterDistribution { total: value.into(), last: value.into(), minimum: "0".into(), maximum: value.into() }
}
