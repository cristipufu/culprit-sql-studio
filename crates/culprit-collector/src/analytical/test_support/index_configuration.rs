//! Manufactured original catalog states, using the existing authenticated collection and physical codecs.
use super::*;
use serde_json::json;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexConfigurationCase {
    Mixed,
    Heap,
    NativeNulls,
    UnavailableColumn,
    Partial,
    UnknownCode,
    Denied,
    NoRows,
}
impl IndexConfigurationCase {
    pub const ALL: [Self; 8] = [
        Self::Mixed,
        Self::Heap,
        Self::NativeNulls,
        Self::UnavailableColumn,
        Self::Partial,
        Self::UnknownCode,
        Self::Denied,
        Self::NoRows,
    ];
}
pub fn index_configuration_evidence(case: IndexConfigurationCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = if case == IndexConfigurationCase::Heap {
        table_full_text_evidence(TableFullTextCase::Absent)
    } else {
        columnstore_configuration_evidence_cases().remove(0)
    };
    if matches!(case, IndexConfigurationCase::Denied | IndexConfigurationCase::NoRows) {
        rows.clear();
    } else {
        let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
        match &mut row.fact {
            OperationalFact::MssqlIndex(index) => {
                index.type_code = 2;
                index.native_type = "NONCLUSTERED".into();
                index.columnstore = None;
                index.name = "ix_recorded".into();
                for (position, column) in index.columns.iter_mut().enumerate() {
                    column.key_ordinal = if position == 0 { 1 } else { 0 };
                    column.included = position != 0;
                    column.descending = false;
                }
                index.configuration = Some(Box::new(configuration(
                    case,
                    Parent {
                        table: &index.table,
                        schema: &index.schema,
                        table_name: &index.table_name,
                        modified: &index.table_modified_without_offset,
                        index_id: index.index_id,
                        name: Some(&index.name),
                        type_code: index.type_code,
                    },
                )));
                row.key = index.key().unwrap();
            }
            OperationalFact::MssqlTable(table) => {
                table.is_heap = Some(true);
                table.heap_configuration = Some(Box::new(configuration(
                    case,
                    Parent {
                        table: &table.identity,
                        schema: &table.schema,
                        table_name: &table.name,
                        modified: &table.modified_without_offset,
                        index_id: 0,
                        name: None,
                        type_code: 0,
                    },
                )));
                row.key = table.identity.key().unwrap();
            }
            _ => panic!(),
        }
        assert!(row.is_valid());
    }
    let header = match &mut source.observation {
        SourceObservation::Snapshot(header) => header,
        _ => panic!(),
    };
    header.records = rows.len() as u32;
    let name = if case == IndexConfigurationCase::Heap {
        culprit_core::source_id::TABLES
    } else {
        culprit_core::source_id::INDEXES
    };
    header.sources = vec![if case == IndexConfigurationCase::Denied {
        StateSourceCoverage::unavailable(name.into(), culprit_core::ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(name.into(), rows.len() as u32, case != IndexConfigurationCase::Partial)
    }];
    assert!(source.partition_is_consistent());
    (source, rows)
}
struct Parent<'a> {
    table: &'a MssqlObjectIdentity,
    schema: &'a str,
    table_name: &'a str,
    modified: &'a str,
    index_id: u32,
    name: Option<&'a str>,
    type_code: u32,
}
fn configuration(case: IndexConfigurationCase, parent: Parent<'_>) -> MssqlIndexConfiguration {
    let Parent { table, schema, table_name, modified, index_id, name, type_code } = parent;
    let count = if case == IndexConfigurationCase::Partial { MAX_MSSQL_CONFIGURATION_PARTITIONS } else { 2 };
    let null = case == IndexConfigurationCase::NativeNulls;
    let denied = case == IndexConfigurationCase::UnavailableColumn;
    let partitions=(1..=count).map(|number|json!({
        "object_id":table.object_id,"index_id":index_id,"partition_id":(9_007_199_254_740_993_u64+number as u64).to_string(),
        "partition_number":number,"hobt_id":(9_007_199_254_741_993_u64+number as u64).to_string(),
        "data_compression":if null{None}else{Some(if case==IndexConfigurationCase::UnknownCode{255}else if number==1{1}else{2})},
        "data_compression_description":if null{None}else{Some(if case==IndexConfigurationCase::UnknownCode{"FUTURE_NATIVE_ENCODING"}else if number==1{"ROW"}else{"PAGE"})},
        "xml_compression":if null||denied{None}else{Some(number!=1)},
        "xml_compression_description":if null||denied{None}else{Some(if number==1{"OFF"}else{"ON"})}
    })).collect::<Vec<_>>();
    let configuration=serde_json::from_value::<MssqlIndexConfiguration>(json!({
        "table":table,"schema":schema,"table_name":table_name,"table_modified_without_offset":modified,
        "index_id":index_id,"name":name,"type_code":type_code,
        "options":if index_id==0{None}else{Some(json!({
            "fill_factor_availability":if denied{"permission-denied"}else{"available"},"fill_factor":if null||denied{None}else{Some(0)},
            "padded_availability":"available","padded":if null{None}else{Some(true)},
            "ignore_duplicate_key_availability":"available","ignore_duplicate_key":if null{None}else{Some(false)},
            "allow_row_locks_availability":"available","allow_row_locks":if null{None}else{Some(false)},
            "allow_page_locks_availability":"available","allow_page_locks":if null{None}else{Some(true)}
        }))},
        "partition_identity_availability":"available","data_compression_availability":"available",
        "data_compression_description_availability":"available","xml_compression_availability":if denied{"unsupported"}else{"available"},
        "xml_compression_description_availability":if denied{"unsupported"}else{"available"},
        "partitions_complete":case!=IndexConfigurationCase::Partial,"partitions":partitions
    })).unwrap();
    assert!(configuration.is_valid());
    configuration
}
