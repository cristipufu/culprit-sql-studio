//! Native-shaped metadata points use existing authenticated CounterSet captures; no source or cost is qualified.
use super::*;
use culprit_core::MeasurementScope;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempdbMetadataMemoryCase {
    Reserved,
    Disabled,
    Unknown,
    NativePartial,
}
impl TempdbMetadataMemoryCase {
    pub const ALL: [Self; 4] = [Self::Reserved, Self::Disabled, Self::Unknown, Self::NativePartial];
}

pub fn assert_tempdb_metadata_memory_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
    case: TempdbMetadataMemoryCase,
) {
    assert_operational_capture(capture, source, records);
    assert!(capture.is_read_complete(), "native partial coverage is independent from archived body completeness");
    let context = NativeReadContext::from_snapshot(capture.source()).unwrap();
    assert!(tempdb_metadata_memory_owner_is_qualified(&context));
    let sets = capture
        .records()
        .iter()
        .filter_map(|record| match &record.row().record {
            TelemetryRecord::Operational(row) if row.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY => {
                let OperationalFact::CounterSet(set) = &row.fact else { panic!("native counter set") };
                Some(set)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let feature = TempdbMetadataFeature::from_original(sets[0], &context).unwrap();
    let expected_enabled = match case {
        TempdbMetadataMemoryCase::Disabled => Some(false),
        TempdbMetadataMemoryCase::Unknown => None,
        _ => Some(true),
    };
    assert_eq!(feature.enabled(), expected_enabled);
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    assert!(tempdb_metadata_memory_source_is_valid(coverage));
    assert_eq!(sets.len(), coverage.records as usize);
    assert_eq!(
        coverage.state == culprit_core::SourceState::Partial,
        matches!(case, TempdbMetadataMemoryCase::Unknown | TempdbMetadataMemoryCase::NativePartial)
    );
    if expected_enabled == Some(true) {
        let consumer = TempdbMetadataConsumer::from_original(sets[1], &context).unwrap();
        assert_eq!(consumer.allocated_bytes(), Some("9007199254740993"));
        assert_eq!(consumer.used_bytes(), Some("9007199254740992"));
        assert!(consumer.usage_is_applicable());
        assert_eq!(consumer.sample().scope, MeasurementScope::Server);
        assert_eq!(consumer.database_created(), feature.database_created());
        assert_ne!(consumer.observed_at(), feature.observed_at());
    } else {
        assert_eq!(sets.len(), 1, "disabled or unknown feature state is never an empty healthy consumer total");
    }
}

pub fn tempdb_metadata_memory_evidence(
    case: TempdbMetadataMemoryCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, original) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let enabled = match case {
        TempdbMetadataMemoryCase::Disabled => Some(false),
        TempdbMetadataMemoryCase::Unknown => None,
        _ => Some(true),
    };
    let mut properties = BTreeMap::from([
        ("native_major".into(), "16".into()),
        ("native_edition".into(), "3".into()),
        ("observed_at".into(), "2020-01-01T12:00:00.2Z".into()),
    ]);
    if let Some(enabled) = enabled {
        properties.insert("metadata_enabled".into(), i32::from(enabled).to_string());
    } else {
        properties.insert("metadata_enabled_state".into(), "native-null".into());
    }
    let identity = |id, kind, object| {
        NativeIdentity::new(id)
            .qualified("kind", kind)
            .qualified("native_object", object)
            .qualified("database_id", "2")
            .qualified("database_created", "2020-01-01T14:00:00.003")
    };
    let feature = NativeCounterSet {
        identity: identity("feature", MSSQL_TEMPDB_METADATA_FEATURE_KIND, "SERVERPROPERTY")
            .qualified("native_property", "IsTempDbMetadataMemoryOptimized"),
        scope: MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    };
    let mut samples = vec![feature];
    if enabled == Some(true) {
        samples.push(NativeCounterSet {
            identity: identity("42", MSSQL_TEMPDB_METADATA_CONSUMER_KIND, MSSQL_TEMPDB_METADATA_CONSUMERS),
            scope: MeasurementScope::Server,
            counters: [("allocated_bytes", 9007199254740993), ("used_bytes", 9007199254740992)]
                .into_iter()
                .map(|(name, value)| {
                    NativeCounterSample::gauge(
                        name,
                        value,
                        CounterUnit::Bytes,
                        MeasurementScope::Server,
                        MSSQL_TEMPDB_METADATA_CONSUMERS,
                        name,
                    )
                })
                .collect(),
            ratios: vec![],
            ratio_gaps: vec![],
            properties: BTreeMap::from([
                ("native_major".into(), "16".into()),
                ("native_edition".into(), "3".into()),
                ("observed_at".into(), "2020-01-01T12:00:00.5Z".into()),
                ("metadata_enabled".into(), "1".into()),
                ("memory_consumer_type".into(), "2".into()),
                ("memory_consumer_type_desc".into(), "VARHEAP".into()),
                ("memory_consumer_desc".into(), "Table heap".into()),
                ("used_bytes_state".into(), "measured".into()),
            ]),
        });
    }
    let native_rows = samples.len() as u32;
    let mut records = samples
        .into_iter()
        .map(|sample| {
            TelemetryRecord::Operational(
                OperationalObservation::from_counters(culprit_core::source_id::TEMPDB_METADATA_MEMORY, sample).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    records.extend(
        original.into_iter().filter(
            |row| matches!(row, TelemetryRecord::Operational(row) if row.source == "independent-native-counters"),
        ),
    );
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.observed_from = "2020-01-01T12:00:00Z".into();
    snapshot.observed_until = "2020-01-01T12:00:01Z".into();
    snapshot.records = records.len() as u32;
    snapshot.sources = vec![
        StateSourceCoverage::observed(
            culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(),
            native_rows,
            !matches!(case, TempdbMetadataMemoryCase::Unknown | TempdbMetadataMemoryCase::NativePartial),
        ),
        StateSourceCoverage::observed("independent-native-counters".into(), 1, true),
    ];
    (source, records)
}
