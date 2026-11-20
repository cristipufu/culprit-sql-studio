//! Original persisted storage settings travel through the existing bounded table-evidence family.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableStorageCase {
    Rowstore,
    DurableMemoryOptimized,
    SchemaOnlyMemoryOptimized,
    NativeNulls,
    Partial,
    SourceDenied,
}
impl TableStorageCase {
    pub const ALL: [Self; 6] = [
        Self::Rowstore,
        Self::DurableMemoryOptimized,
        Self::SchemaOnlyMemoryOptimized,
        Self::NativeNulls,
        Self::Partial,
        Self::SourceDenied,
    ];
}

pub fn table_storage_evidence(case: TableStorageCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source_case = match case {
        TableStorageCase::Partial => TableFullTextCase::Partial,
        TableStorageCase::SourceDenied => TableFullTextCase::SourceDenied,
        _ => TableFullTextCase::Absent,
    };
    let (source, mut records) = table_full_text_evidence(source_case);
    for record in &mut records {
        let TelemetryRecord::Operational(row) = record else { unreachable!() };
        let OperationalFact::MssqlTable(table) = &mut row.fact else { unreachable!() };
        let (memory_optimized, durability, description) = match case {
            TableStorageCase::Rowstore => (Some(false), Some(0), Some("SCHEMA_AND_DATA")),
            TableStorageCase::DurableMemoryOptimized => (Some(true), Some(0), Some("SCHEMA_AND_DATA")),
            TableStorageCase::SchemaOnlyMemoryOptimized | TableStorageCase::Partial => {
                (Some(true), Some(1), Some("SCHEMA_ONLY"))
            }
            TableStorageCase::NativeNulls | TableStorageCase::SourceDenied => (None, None, None),
        };
        table.storage_configuration = MssqlTableStorageConfiguration {
            memory_optimized,
            durability,
            durability_desc: description.map(str::to_owned),
        };
        assert!(table.is_valid());
    }
    (source, records)
}
