//! Native-shaped original Common inputs reuse the authentic capture; no native/cost qualification is implied.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommonPerformanceCounterCase {
    Observed,
    WithGovernance,
    MeasuredZero,
    MissingBase,
    UnknownNativeType,
    NativePartial,
    MissingCommon,
    Denied,
}
impl CommonPerformanceCounterCase {
    pub const ALL: [Self; 8] = [
        Self::Observed,
        Self::WithGovernance,
        Self::MeasuredZero,
        Self::MissingBase,
        Self::UnknownNativeType,
        Self::NativePartial,
        Self::MissingCommon,
        Self::Denied,
    ];
}

pub fn common_performance_counter_evidence(
    case: CommonPerformanceCounterCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let governed = match case {
        CommonPerformanceCounterCase::Denied => ResourceGovernorCounterCase::Denied,
        CommonPerformanceCounterCase::WithGovernance | CommonPerformanceCounterCase::NativePartial => {
            ResourceGovernorCounterCase::Exact
        }
        _ => ResourceGovernorCounterCase::NoRows,
    };
    let (mut source, mut rows) = resource_governor_counter_evidence(governed);
    if case == CommonPerformanceCounterCase::Denied {
        return (source, rows);
    }
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(sample), .. }) =
        &mut rows[0]
    else {
        panic!()
    };
    let mut common = counters(case);
    let key = |name: &str| common.iter().find(|counter| counter.native_name == name).map(|counter| counter.key.clone());
    sample.ratios.push(NativeCounterRatio {
        kind: CounterRatioKind::BufferCacheHit,
        numerator: key("Buffer cache hit ratio"),
        base: key("Buffer cache hit ratio base"),
        support: CounterRatioSupport::Supported,
        observed_at: sample.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY).cloned(),
    });
    sample.ratios.push(NativeCounterRatio {
        kind: CounterRatioKind::LockWaitAverageInputs,
        numerator: key("Average Wait Time (ms)"),
        base: key("Average Wait Time Base"),
        support: CounterRatioSupport::Supported,
        observed_at: sample.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY).cloned(),
    });
    common.append(&mut sample.counters);
    sample.counters = common;
    sample.properties.insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_COMMON_COUNTER_PROFILE.into());
    sample.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), sample.counters.len().to_string());
    Engine::Mssql.spec().performance_counters().unwrap().validate(sample).unwrap();
    let header = match &mut source.observation {
        SourceObservation::Snapshot(header) => header,
        _ => panic!(),
    };
    if case == CommonPerformanceCounterCase::NativePartial {
        header.sources[0] = StateSourceCoverage::observed(
            culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
            header.sources[0].records,
            false,
        );
    }
    assert!(source.partition_is_consistent());
    (source, rows)
}

fn counters(case: CommonPerformanceCounterCase) -> Vec<NativeCounterSample> {
    if case == CommonPerformanceCounterCase::MissingCommon {
        return vec![];
    }
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    profile
        .common_fields
        .iter()
        .chain(profile.detailed_fields)
        .flat_map(|spec| {
            let instances = match spec.instances {
                PerformanceCounterInstances::Named(instances) => {
                    instances.iter().map(|name| Some(*name)).collect::<Vec<_>>()
                }
                PerformanceCounterInstances::Uninstanced => vec![None],
                PerformanceCounterInstances::Any => panic!("Common fixture needs an exact bounded instance roster"),
            };
            instances.into_iter().filter_map(move |instance| {
                if case == CommonPerformanceCounterCase::MissingBase && spec.nature == CounterNature::FractionBase {
                    return None;
                }
                let object = format!("SQLServer:{}", spec.object_family.unwrap());
                let code = if case == CommonPerformanceCounterCase::UnknownNativeType
                    && matches!(
                        spec.id,
                        "common_page_reads" | "detailed_forwarded_records" | "detailed_lock_wait_numerator"
                    ) {
                    42
                } else {
                    match spec.nature {
                        CounterNature::Gauge => 65_792,
                        CounterNature::Cumulative => 272_696_576,
                        CounterNature::FractionNumerator => 537_003_264,
                        CounterNature::AverageNumerator => 1_073_874_176,
                        CounterNature::FractionBase => 1_073_939_712,
                        _ => panic!("Unexpected Common fixture meaning"),
                    }
                };
                let raw = if case == CommonPerformanceCounterCase::MeasuredZero {
                    "0"
                } else {
                    match spec.id {
                        "common_buffer_hits" => "3",
                        "common_buffer_base" => "4",
                        "detailed_lock_wait_numerator" => "4500",
                        "detailed_lock_wait_base" => "3",
                        "common_blocked" => "0",
                        _ => "9007199254740993",
                    }
                };
                let (nature, unit) = spec.meaning(code);
                Some(NativeCounterSample {
                    key: digest(&serde_json::to_vec(&(object.as_str(), spec.native_name, instance)).unwrap()),
                    value: MeasurementValue::Integer(raw.into()),
                    unit,
                    nature,
                    scope: MeasurementScope::Server,
                    reset: ResetMarker::Unknown,
                    native_object: object,
                    native_name: spec.native_name.into(),
                    native_instance: instance.map(str::to_owned),
                    native_type: Some(code.to_string()),
                })
            })
        })
        .collect()
}

/// Shared adapters exercise the same original descriptors, exact values and ratio inputs without provider access.
pub fn assert_common_performance_counter_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
    case: CommonPerformanceCounterCase,
) {
    super::super::assert_operational_capture(capture, source, rows);
    let sample = capture.records().iter().find_map(|entry| match &entry.row().record {
        TelemetryRecord::Operational(OperationalObservation {
            source,
            fact: OperationalFact::CounterSet(sample),
            ..
        }) if source == culprit_core::source_id::PERFORMANCE_COUNTERS && !resource_governor_counter_set(sample) => {
            Some(sample.as_ref())
        }
        _ => None,
    });
    if case == CommonPerformanceCounterCase::Denied {
        assert!(sample.is_none());
        return;
    }
    let sample = sample.unwrap();
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    profile.validate(sample).unwrap();
    assert!(profile.common_requested(sample));
    let common = sample
        .counters
        .iter()
        .filter(|counter| {
            profile
                .common_field(&counter.native_object, &counter.native_name, counter.native_instance.as_deref())
                .is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        common.len(),
        match case {
            CommonPerformanceCounterCase::MissingCommon => 0,
            CommonPerformanceCounterCase::MissingBase => 23,
            _ => 24,
        }
    );
    assert!(
        common.iter().all(|counter| counter.reset == ResetMarker::Unknown && counter.scope == MeasurementScope::Server)
    );
    let detailed = sample
        .counters
        .iter()
        .filter(|counter| {
            profile
                .detailed_field(&counter.native_object, &counter.native_name, counter.native_instance.as_deref())
                .is_some_and(|field| field.family == PerformanceCounterFamily::AccessMethods)
        })
        .collect::<Vec<_>>();
    assert_eq!(detailed.len(), if case == CommonPerformanceCounterCase::MissingCommon { 0 } else { 6 });
    for counter in detailed {
        assert_eq!(counter.native_object, "SQLServer:Access Methods");
        assert_eq!(counter.native_instance, None);
        assert_eq!(counter.scope, MeasurementScope::Server);
        assert_eq!(counter.reset, ResetMarker::Unknown);
        assert_eq!(
            counter.value.integer(),
            Some(if case == CommonPerformanceCounterCase::MeasuredZero { 0 } else { 9_007_199_254_740_993 })
        );
        assert_eq!(
            (counter.nature, counter.unit),
            if case == CommonPerformanceCounterCase::UnknownNativeType && counter.native_name == "Forwarded Records/sec"
            {
                (CounterNature::Unknown, CounterUnit::Unknown)
            } else {
                (CounterNature::Cumulative, CounterUnit::Count)
            }
        );
    }
    let locks = sample
        .counters
        .iter()
        .filter(|counter| {
            profile
                .detailed_field(&counter.native_object, &counter.native_name, counter.native_instance.as_deref())
                .is_some_and(|field| field.family == PerformanceCounterFamily::Locks)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        locks.len(),
        match case {
            CommonPerformanceCounterCase::MissingCommon => 0,
            CommonPerformanceCounterCase::MissingBase => 5,
            _ => 6,
        }
    );
    assert!(locks.iter().all(|counter| counter.native_object == "SQLServer:Locks"
        && counter.native_instance.as_deref() == Some("_Total")
        && counter.scope == MeasurementScope::Server
        && counter.reset == ResetMarker::Unknown));
    let average = sample.ratios.iter().find(|ratio| ratio.kind == CounterRatioKind::LockWaitAverageInputs).unwrap();
    assert!(average.percent(sample).is_none());
    let issues = average.issues(sample);
    assert!(issues.contains(&CounterRatioIssue::IntervalRequired));
    assert!(!issues.contains(&CounterRatioIssue::OutsideSubset));
    match case {
        CommonPerformanceCounterCase::MissingCommon => assert!(
            issues.contains(&CounterRatioIssue::MissingNumerator) && issues.contains(&CounterRatioIssue::MissingBase)
        ),
        CommonPerformanceCounterCase::MissingBase => assert!(issues.contains(&CounterRatioIssue::MissingBase)),
        CommonPerformanceCounterCase::MeasuredZero => assert!(issues.contains(&CounterRatioIssue::ZeroBase)),
        CommonPerformanceCounterCase::UnknownNativeType => {
            assert!(issues.contains(&CounterRatioIssue::UnsupportedCounterType))
        }
        _ => assert_eq!(issues, [CounterRatioIssue::IntervalRequired]),
    }
    let ratio = sample.ratios.iter().find(|ratio| ratio.kind == CounterRatioKind::BufferCacheHit).unwrap();
    match case {
        CommonPerformanceCounterCase::MissingCommon => {
            assert!(ratio.numerator.is_none() && ratio.base.is_none());
            assert!(ratio.percent(sample).is_none());
        }
        CommonPerformanceCounterCase::MissingBase => {
            assert!(ratio.numerator.is_some() && ratio.base.is_none());
            assert!(ratio.issues(sample).contains(&CounterRatioIssue::MissingBase));
        }
        CommonPerformanceCounterCase::MeasuredZero => {
            assert!(common.iter().all(|counter| counter.value.integer() == Some(0)));
            assert!(ratio.issues(sample).contains(&CounterRatioIssue::ZeroBase));
        }
        _ => assert_eq!(ratio.percent(sample).as_deref(), Some("75.00")),
    }
    if case == CommonPerformanceCounterCase::UnknownNativeType {
        let raw = common.iter().find(|counter| counter.native_name == "Page reads/sec").unwrap();
        assert_eq!((raw.nature, raw.unit), (CounterNature::Unknown, CounterUnit::Unknown));
        assert_eq!(raw.value.integer(), Some(9_007_199_254_740_993));
    }
}
