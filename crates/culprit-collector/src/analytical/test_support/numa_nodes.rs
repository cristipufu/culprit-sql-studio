//! Controlled original node populations for source-free conformance, never native/production-cost proof.
use super::*;
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumaNodeCase {
    Exact,
    Zero,
    NativeNull,
    ColumnUnavailable,
    CpuDenied,
    MemoryDenied,
    BothDenied,
    PartialMemory,
    Limited,
}
impl NumaNodeCase {
    pub const ALL: [Self; 9] = [
        Self::Exact,
        Self::Zero,
        Self::NativeNull,
        Self::ColumnUnavailable,
        Self::CpuDenied,
        Self::MemoryDenied,
        Self::BothDenied,
        Self::PartialMemory,
        Self::Limited,
    ];
}
pub fn numa_node_evidence(case: NumaNodeCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    source.source = MSSQL_NUMA_NODES_SOURCE.into();
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for kind in MssqlNumaNodeKind::ALL {
        let denied = case == NumaNodeCase::BothDenied
            || case == NumaNodeCase::CpuDenied && kind == MssqlNumaNodeKind::Cpu
            || case == NumaNodeCase::MemoryDenied && kind == MssqlNumaNodeKind::Memory;
        if denied {
            sources.push(StateSourceCoverage::unavailable(
                kind.source().into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ));
            continue;
        }
        let count = if case == NumaNodeCase::Limited && kind == MssqlNumaNodeKind::Cpu { 257 } else { 1 };
        for id in 0..count {
            let mut sample = NativeCounterSet {
                identity: NativeIdentity::new(id.to_string()).qualified("native_object", kind.object()),
                scope: culprit_core::MeasurementScope::Server,
                counters: vec![],
                properties: BTreeMap::new(),
                ratios: vec![],
                ratio_gaps: vec![],
            };
            for field in kind.fields() {
                match field.kind {
                    MssqlNativeFieldKind::Identity => {}
                    MssqlNativeFieldKind::TextProperty => {
                        sample.properties.insert(field.name.into(), "ONLINE DAC".into());
                    }
                    MssqlNativeFieldKind::IntegerProperty => {
                        sample.properties.insert(
                            field.name.into(),
                            if field.name.ends_with("mask") { "-9223372036854775808" } else { "0" }.into(),
                        );
                    }
                    MssqlNativeFieldKind::BooleanProperty => {
                        sample.properties.insert(field.name.into(), "0".into());
                    }
                    MssqlNativeFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
                        key: field.name.into(),
                        value: MeasurementValue::Integer(
                            if case == NumaNodeCase::Zero {
                                "0"
                            } else if kind == MssqlNumaNodeKind::Cpu {
                                "17"
                            } else {
                                "9007199254740993"
                            }
                            .into(),
                        ),
                        unit,
                        nature,
                        scope: sample.scope,
                        reset: culprit_core::ResetMarker::Unknown,
                        native_object: kind.object().into(),
                        native_name: field.name.into(),
                        native_instance: None,
                        native_type: None,
                    }),
                    MssqlNativeFieldKind::UnqualifiedCounter { .. } => unreachable!(),
                }
            }
            if kind == MssqlNumaNodeKind::Memory
                && matches!(case, NumaNodeCase::NativeNull | NumaNodeCase::ColumnUnavailable)
            {
                let field = if case == NumaNodeCase::NativeNull { "pages_kb" } else { "target_kb" };
                sample.counters.retain(|counter| counter.key != field);
                sample.properties.insert(
                    if case == NumaNodeCase::NativeNull { "native_null_columns" } else { "unavailable_columns" }.into(),
                    serde_json::to_string(&[field]).unwrap(),
                );
            }
            assert!(MssqlNumaNodePoint::from_sample(&sample).is_ok());
            rows.push(TelemetryRecord::Operational(
                OperationalObservation::from_counters(kind.source(), sample).unwrap(),
            ));
        }
        sources.push(StateSourceCoverage::observed(
            kind.source().into(),
            count,
            case != NumaNodeCase::PartialMemory || kind != MssqlNumaNodeKind::Memory,
        ));
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.dataset = SnapshotDataset::NumaNodes;
    header.sources = sources;
    header.records = rows.len() as u32;
    assert!(numa_node_header_is_valid(&source));
    assert!(source.partition_is_consistent());
    (source, rows)
}
