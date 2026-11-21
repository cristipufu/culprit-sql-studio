//! Manufactured original mixed catalog states use the existing authenticated Indexes codec and source.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexedViewsCase {
    Present,
    Empty,
    Partial,
    Denied,
    UsageNotCollected,
    TableOnly,
}
impl IndexedViewsCase {
    pub const ALL: [Self; 6] =
        [Self::Present, Self::Empty, Self::Partial, Self::Denied, Self::UsageNotCollected, Self::TableOnly];
}
pub fn indexed_views_evidence(case: IndexedViewsCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, records) = index_configuration_evidence(IndexConfigurationCase::Mixed);
    let TelemetryRecord::Operational(original) = &records[0] else { panic!() };
    let OperationalFact::MssqlIndex(table) = &original.fact else { panic!() };
    let mut rows = vec![records[0].clone()];
    if !matches!(
        case,
        IndexedViewsCase::Empty | IndexedViewsCase::Denied | IndexedViewsCase::TableOnly | IndexedViewsCase::Partial
    ) {
        for (index_id, name) in [(1, "cx_revenue"), (2, "ix_revenue")] {
            let mut index = table.as_ref().clone();
            index.parent_kind = MssqlIndexParentKind::IndexedView;
            index.parent_memory_optimized = None;
            index.parent_object.object_id += 1;
            index.parent_schema = "reporting".into();
            index.parent_name = "Revenue".into();
            index.index_id = index_id;
            index.name = name.into();
            index.type_code = index_id;
            index.native_type = if index_id == 1 { "CLUSTERED" } else { "NONCLUSTERED" }.into();
            index.unique = index_id == 1;
            index.primary_key = false;
            index.filter = None;
            if case == IndexedViewsCase::UsageNotCollected {
                index.usage = MssqlIndexUsage::NotCollected;
            }
            let configuration = index.configuration.as_mut().unwrap();
            configuration.parent_kind = index.parent_kind;
            configuration.parent_object = index.parent_object.clone();
            configuration.parent_schema = index.parent_schema.clone();
            configuration.parent_name = index.parent_name.clone();
            configuration.index_id = index_id;
            configuration.name = Some(name.into());
            configuration.type_code = index_id;
            for partition in &mut configuration.partitions {
                partition.object_id = index.parent_object.object_id;
                partition.index_id = index_id;
                partition.partition_id =
                    (9_007_199_254_750_000u64 + u64::from(index_id) * 10 + u64::from(partition.partition_number))
                        .to_string();
                partition.hobt_id =
                    (9_007_199_254_760_000u64 + u64::from(index_id) * 10 + u64::from(partition.partition_number))
                        .to_string();
            }
            let observation = OperationalObservation {
                source: culprit_core::source_id::INDEXES.into(),
                key: index.key().unwrap(),
                fact: OperationalFact::MssqlIndex(Box::new(index)),
            };
            assert!(observation.is_valid());
            rows.push(TelemetryRecord::Operational(observation));
        }
    }
    if matches!(case, IndexedViewsCase::Empty | IndexedViewsCase::Denied) {
        rows.clear();
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = rows.len() as u32;
    header.sources = vec![if case == IndexedViewsCase::Denied {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::INDEXES.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::INDEXES.into(),
            rows.len() as u32,
            case != IndexedViewsCase::Partial,
        )
    }];
    assert!(source.partition_is_consistent());
    (source, rows)
}
