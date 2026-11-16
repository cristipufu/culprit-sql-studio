use super::*;
use crate::{DatabaseIdentity, Engine, ErrorDetail, MeasurementScope, ResetMarker, SourceState};

fn read() -> Box<StatementCounterSnapshot> {
    let point = StatementCounterSample {
        member: None,
        identity: NativeQueryIdentity {
            query_id: "private-query-canary".into(),
            qualifiers: [("schema".into(), "private-qualifier-canary".into())].into(),
        },
        scope: MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456Z".into(),
        reset: ResetMarker::Unknown,
        totals: vec![StatementCounter {
            key: RuntimeMetricKey::Errors,
            unit: RuntimeMetricUnit::Count,
            total: MeasurementValue::Integer("9007199254740993".into()),
        }],
        distributions: vec![],
    };
    let source = "private-source-canary";
    Box::new(StatementCounterSnapshot {
        source: source.into(),
        context: Some(Box::new(NativeReadContext {
            target: DatabaseIdentity::server(
                Engine::Mysql,
                "public-database",
                "private-counter-route.invalid",
                3306,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        })),
        coverage: StateSourceCoverage::collected(source.into(), 1),
        observations: vec![OperationalObservation {
            source: source.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::StatementCounters(Box::new(point)),
        }],
    })
}

#[test]
fn malformed_native_carriers_cannot_become_valid_through_masking_or_canonical_key_replacement() {
    for change in ["empty-query", "contradictory-key", "invalid-member", "count", "source"] {
        let mut original = read();
        assert!(original.is_valid());
        match change {
            "empty-query" => {
                let OperationalFact::StatementCounters(point) = &mut original.observations[0].fact else { panic!() };
                point.identity.query_id.clear();
                original.observations[0].key = point.key().unwrap();
            }
            "contradictory-key" => original.observations[0].key = digest(b"different-original-key"),
            "invalid-member" => {
                let mut member = crate::engines::ysql_node_member("0123456789abcdef0123456789abcdef").unwrap();
                member.namespace.id.clear();
                let context = original.context.as_mut().unwrap();
                context.target = DatabaseIdentity::server(
                    Engine::Yugabyte,
                    "public-database",
                    "private-counter-route.invalid",
                    5433,
                    None,
                )
                .into();
                context.partition = SourcePartition::InstanceLocal {
                    member: Some(member.clone()),
                    epoch: Some("private-process-canary".into()),
                };
                let OperationalFact::StatementCounters(point) = &mut original.observations[0].fact else { panic!() };
                point.scope = MeasurementScope::Node;
                point.member = Some(member);
            }
            "count" => original.coverage.records = 0,
            _ => original.source.clear(),
        }
        assert!(!original.is_valid_read(false), "{change}");
        assert_eq!(
            redact(original.clone()).unwrap_err().detail,
            Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)),
            "{change}"
        );

        let mut snapshot: Box<crate::Snapshot> =
            serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mysql-demo.json")))
                .unwrap();
        snapshot.operations.statement_work = Some(original);
        let shared = crate::redact::redact_snapshot(&snapshot);
        let unavailable =
            shared.operations.statement_work.as_ref().expect("invalid evidence stays explicitly unavailable");
        assert!(unavailable.is_valid());
        assert_eq!(unavailable.coverage.state, SourceState::Unavailable);
        assert_eq!(unavailable.coverage.stopped, Some(CollectionStop::InvalidPage));
        assert!(unavailable.context.is_none() && unavailable.observations.is_empty());
        assert_eq!(unavailable.coverage.records, 0);
        assert!(!unavailable.coverage.complete_scope);
        let body = serde_json::to_string(&shared).unwrap();
        for private in [
            "private-source-canary",
            "private-query-canary",
            "private-qualifier-canary",
            "private-counter-route",
            "private-process-canary",
            "0123456789abcdef0123456789abcdef",
        ] {
            assert!(!body.contains(private), "{change}: {private}");
        }
    }
}

#[test]
fn a_genuine_partial_archive_roster_keeps_its_original_counts_and_exact_positive_values() {
    let mut original = read();
    original.coverage.records = 2;
    assert!(original.is_valid_read(false) && !original.is_valid());
    let shared = redact(original).unwrap();
    assert_eq!(shared.coverage.records, 2);
    assert_eq!(shared.observations.len(), 1);
    assert!(shared.is_valid_read(false) && !shared.is_valid());
    let OperationalFact::StatementCounters(point) = &shared.observations[0].fact else { panic!() };
    assert_eq!(point.outcomes().unwrap().errors.as_deref(), Some("9007199254740993"));
    assert_eq!(point.reset, ResetMarker::Unknown);
}
