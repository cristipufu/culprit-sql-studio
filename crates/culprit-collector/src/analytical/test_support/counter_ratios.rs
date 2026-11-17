//! Controlled native fraction inputs use the real common capture/archive path; they qualify no engine or cost.
use super::*;
use culprit_core::{MeasurementScope, ResetMarker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterRatioCase {
    Exact,
    ZeroNumerator,
    ZeroBase,
    MissingBase,
    MissingBoth,
    Negative,
    OutsideSubset,
    UnknownProfile,
    UnsupportedType,
    MultipleInstances,
    PartialSource,
    PermissionDenied,
}
impl CounterRatioCase {
    pub const ALL: [Self; 12] = [
        Self::Exact,
        Self::ZeroNumerator,
        Self::ZeroBase,
        Self::MissingBase,
        Self::MissingBoth,
        Self::Negative,
        Self::OutsideSubset,
        Self::UnknownProfile,
        Self::UnsupportedType,
        Self::MultipleInstances,
        Self::PartialSource,
        Self::PermissionDenied,
    ];
}

pub fn counter_ratio_evidence(case: CounterRatioCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (manifest, _) = fixture();
    let mut set = NativeCounterSet {
        identity: NativeIdentity::new("sys.dm_os_performance_counters")
            .qualified("native_object", "sys.dm_os_performance_counters"),
        scope: MeasurementScope::Server,
        counters: pair("one", 9_007_199_254_740_993, 9_007_199_254_740_994),
        ratios: vec![declaration("one")],
        ratio_gaps: vec![],
        properties: Default::default(),
    };
    match case {
        CounterRatioCase::Exact | CounterRatioCase::PartialSource | CounterRatioCase::PermissionDenied => (),
        CounterRatioCase::ZeroNumerator => set.counters[0].value = MeasurementValue::Integer("0".into()),
        CounterRatioCase::ZeroBase => set.counters[1].value = MeasurementValue::Integer("0".into()),
        CounterRatioCase::OutsideSubset => set.counters[1].value = MeasurementValue::Integer("1".into()),
        CounterRatioCase::MissingBase => {
            set.counters.pop();
            set.ratios[0].base = None;
        }
        CounterRatioCase::MissingBoth => {
            set.counters = vec![NativeCounterSample {
                key: "batches".into(),
                value: MeasurementValue::Integer("7".into()),
                unit: CounterUnit::Count,
                nature: CounterNature::Cumulative,
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object: "actual SQL Statistics".into(),
                native_name: "Batch Requests/sec".into(),
                native_instance: None,
                native_type: Some("272696576".into()),
            }];
            set.ratios[0].numerator = None;
            set.ratios[0].base = None;
        }
        CounterRatioCase::Negative => {
            let rejected = set.counters.remove(0);
            set.ratio_gaps.push(CounterRatioGap {
                key: rejected.key,
                raw_value: "-1".into(),
                native_object: rejected.native_object,
                native_name: rejected.native_name,
                native_instance: rejected.native_instance,
                native_type: rejected.native_type.unwrap(),
            });
        }
        CounterRatioCase::UnknownProfile => set.ratios[0].support = CounterRatioSupport::Unknown,
        CounterRatioCase::UnsupportedType => {
            set.counters[1].nature = CounterNature::Unknown;
            set.counters[1].unit = CounterUnit::Unknown;
            set.counters[1].native_type = Some("42".into());
        }
        CounterRatioCase::MultipleInstances => {
            set.counters.extend(pair("two", 0, 5));
            set.ratios.push(declaration("two"));
        }
    }
    assert!(set.is_valid());
    let records = if case == CounterRatioCase::PermissionDenied {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(
            OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, set).unwrap(),
        )]
    };
    let coverage = match case {
        CounterRatioCase::PermissionDenied => StateSourceCoverage::unavailable(
            culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        CounterRatioCase::PartialSource => StateSourceCoverage::observed(
            culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
            records.len() as u32,
            false,
        ),
        _ => StateSourceCoverage::collected(culprit_core::source_id::PERFORMANCE_COUNTERS.into(), records.len() as u32),
    };
    let source = CollectionDescription {
        source: "native-performance-counters".into(),
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![coverage],
        }),
        ..manifest.source
    };
    (source, records)
}

fn declaration(instance: &str) -> NativeCounterRatio {
    NativeCounterRatio {
        kind: CounterRatioKind::LocalSsdCacheHit,
        numerator: Some(format!("hits:{instance}")),
        base: Some(format!("base:{instance}")),
        support: CounterRatioSupport::Supported,
        observed_at: Some("2020-01-01T12:00:00.5Z".into()),
    }
}
fn pair(instance: &str, numerator: u64, base: u64) -> Vec<NativeCounterSample> {
    [
        (format!("hits:{instance}"), "RBPEX cache hit ratio", numerator, CounterNature::FractionNumerator, "537003264"),
        (format!("base:{instance}"), "RBPEX cache hit ratio base", base, CounterNature::FractionBase, "1073939712"),
    ]
    .into_iter()
    .map(|(key, name, value, nature, native_type)| NativeCounterSample {
        key,
        value: MeasurementValue::Integer(value.to_string()),
        unit: CounterUnit::Count,
        nature,
        scope: MeasurementScope::Server,
        reset: ResetMarker::Unknown,
        native_object: "actual-native-cache-object".into(),
        native_name: name.into(),
        native_instance: Some(instance.into()),
        native_type: Some(native_type.into()),
    })
    .collect()
}
