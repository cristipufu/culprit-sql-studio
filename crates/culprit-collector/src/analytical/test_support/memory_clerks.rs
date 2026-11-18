//! Native-shaped grouped allocations use the existing sealed CounterSet capture; no source or cost is qualified.
use super::*;
use culprit_core::{Engine, MeasurementScope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryClerkCase {
    SharedInstance,
    Exact,
    MeasuredZero,
    Unreported,
    NativePartial,
    UnqualifiedMember,
    Denied,
}
impl MemoryClerkCase {
    pub const ALL: [Self; 7] = [
        Self::SharedInstance,
        Self::Exact,
        Self::MeasuredZero,
        Self::Unreported,
        Self::NativePartial,
        Self::UnqualifiedMember,
        Self::Denied,
    ];
}

/// SharedInstance matches the current native acquisition route. Database-target cases are controlled model
/// conformance inputs; they do not claim an ordinary database scan or acquisition supports clerk collection.
pub fn memory_clerk_evidence(case: MemoryClerkCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let sets = if case == MemoryClerkCase::Denied {
        vec![]
    } else {
        vec![clerk("CACHESTORE_SQLCP", "SQL Plans", 0, case), clerk("CACHESTORE_SQLCP", "SQL Plans", 1, case)]
    };
    let mut records = sets
        .into_iter()
        .map(|set| {
            TelemetryRecord::Operational(
                OperationalObservation::from_counters(culprit_core::source_id::MEMORY_CLERKS, set).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let native_rows = records.len() as u32;
    records.push(TelemetryRecord::Operational(
        OperationalObservation::from_counters(
            "independent-native-counters",
            NativeCounterSet {
                identity: NativeIdentity::new("unrelated"),
                scope: MeasurementScope::Server,
                counters: vec![NativeCounterSample::gauge(
                    "unrelated",
                    42,
                    CounterUnit::Count,
                    MeasurementScope::Server,
                    "other-native-view",
                    "unrelated",
                )],
                properties: Default::default(),
                ratios: vec![],
                ratio_gaps: vec![],
            },
        )
        .unwrap(),
    ));
    let identified = case != MemoryClerkCase::UnqualifiedMember;
    let complete_scope = identified && case != MemoryClerkCase::NativePartial;
    let coverage = if case == MemoryClerkCase::Denied {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::MEMORY_CLERKS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(culprit_core::source_id::MEMORY_CLERKS.into(), native_rows, complete_scope)
    };
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-instance-namespace"),
        native: NativeIdentity::new("native-instance-a"),
    };
    let target = if case == MemoryClerkCase::SharedInstance {
        CollectionTarget::Shared(member.clone())
    } else {
        culprit_core::DatabaseIdentity::server(
            Engine::Mssql,
            "app",
            "fixture.invalid",
            1433,
            Some("native-database-a".into()),
        )
        .into()
    };
    let source = CollectionDescription {
        target,
        source: if case == MemoryClerkCase::SharedInstance {
            "shared-operational-state"
        } else {
            "controlled-database-clerk-capture"
        }
        .into(),
        partition: SourcePartition::InstanceLocal {
            member: identified.then_some(member),
            epoch: identified.then(|| "native-process-a".into()),
        },
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00.000000001Z".into(),
            observed_until: "2020-01-01T12:00:00.000000009Z".into(),
            records: records.len() as u32,
            sources: vec![coverage, StateSourceCoverage::observed("independent-native-counters".into(), 1, identified)],
        }),
    };
    assert!(source.partition_is_consistent());
    (source, records)
}

fn clerk(kind: &str, name: &str, node: i16, case: MemoryClerkCase) -> NativeCounterSet {
    let object = "sys.dm_os_memory_clerks";
    let mut counters = vec![NativeCounterSample::gauge(
        "clerk_count",
        2,
        CounterUnit::Count,
        MeasurementScope::Server,
        object,
        "COUNT_BIG(*)",
    )];
    if case != MemoryClerkCase::Unreported {
        let zero = case == MemoryClerkCase::MeasuredZero;
        for (key, value, unit, native_name) in [
            ("pages_kb", if zero { 0 } else { 9_007_199_254_740_993 }, CounterUnit::Kibibytes, "pages_kb"),
            (
                "virtual_memory_reserved_kb",
                if zero { 0 } else { 1024 },
                CounterUnit::Kibibytes,
                "virtual_memory_reserved_kb",
            ),
            (
                "virtual_memory_committed_kb",
                if zero { 0 } else { 512 },
                CounterUnit::Kibibytes,
                "virtual_memory_committed_kb",
            ),
            ("awe_allocated_kb", 0, CounterUnit::Kibibytes, "awe_allocated_kb"),
            // Native NULL shared_memory_reserved_kb is an omitted counter, not a measured zero.
            ("shared_memory_committed_kb", 0, CounterUnit::Kibibytes, "shared_memory_committed_kb"),
            ("minimum_page_size_in_bytes", if zero { 0 } else { 8192 }, CounterUnit::Bytes, "page_size_in_bytes"),
            ("maximum_page_size_in_bytes", if zero { 0 } else { 65536 }, CounterUnit::Bytes, "page_size_in_bytes"),
        ] {
            counters.push(NativeCounterSample::gauge(key, value, unit, MeasurementScope::Server, object, native_name));
        }
    }
    NativeCounterSet {
        identity: NativeIdentity::new(kind)
            .qualified("native_object", object)
            .qualified("memory_node_id", node.to_string())
            .qualified("name_sha256", digest(name.as_bytes())),
        scope: MeasurementScope::Server,
        counters,
        properties: [
            ("type".into(), kind.into()),
            ("name".into(), name.into()),
            ("memory_node_id".into(), node.to_string()),
            ("aggregation".into(), "clerk-type-name-and-memory-node".into()),
        ]
        .into(),
        ratios: vec![],
        ratio_gaps: vec![],
    }
}
