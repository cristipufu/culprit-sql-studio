//! Current component allocations, grouped by the native clerk type, name and memory node.
//!
//! `sys.dm_os_memory_clerks` describes the database-engine instance, including when an Azure pool member is the
//! access route. Callers must establish native resource identity and visibility before claiming complete coverage.
//! These rows establish neither a database's allocation nor a query owner. Allocation dimensions can overlap;
//! reserved, committed, page and locked memory must not be added into an invented total.
//!
//! SQL Server 2012+ supplies the selected columns. Permissions are native: VIEW SERVER STATE before SQL Server
//! 2022, VIEW SERVER PERFORMANCE STATE from 2022; Azure SQL requirements depend on service objective and pool.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-memory-clerks-transact-sql
//! https://learn.microsoft.com/azure/azure-sql/database-watcher-data
//!
//! The result cap bounds transport/materialization, not server work: GROUP BY visits the native clerk metadata.
//! Qualify that cost under load before enabling a production profile. There is no memory-object traversal or DBCC.
use crate::read::{NativeRead, Parameter, ReadError, invalid_data, optional_text, signed_integer};
use crate::value::Rec;
use culprit_core::{MeasurementScope, Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SOURCE_NAME: &str = culprit_core::source_id::MEMORY_CLERKS;
pub(crate) const MIN_SQL_SERVER_MAJOR: u16 = 11;
const NATIVE_OBJECT: &str = "sys.dm_os_memory_clerks";
const MAX_CLERK_GROUPS: usize = 4096;
const TYPE_COLUMN: &str = "type";
const NAME_COLUMN: &str = "name";
const NODE_COLUMN: &str = "memory_node_id";
const PAGE_SIZE_COLUMN: &str = "page_size_in_bytes";

enum Aggregation {
    Sum,
    Minimum(&'static str),
    Maximum(&'static str),
    Count,
}
struct Field {
    key: &'static str,
    unit: CounterUnit,
    aggregation: Aggregation,
}
impl Field {
    fn column(&self) -> &'static str {
        match self.aggregation {
            Aggregation::Sum => self.key,
            Aggregation::Minimum(column) | Aggregation::Maximum(column) => column,
            Aggregation::Count => "COUNT_BIG(*)",
        }
    }
    fn projection(&self) -> String {
        let column = self.column();
        let aggregate = match self.aggregation {
            Aggregation::Sum => format!("SUM(CONVERT(decimal(38,0),{column}))"),
            Aggregation::Minimum(_) => format!("MIN({column})"),
            Aggregation::Maximum(_) => format!("MAX({column})"),
            Aggregation::Count => return format!("CONVERT(varchar(40),COUNT_BIG(*)) AS {}", self.key),
        };
        // A masked or unavailable value in one clerk makes the corresponding grouped value unknown.
        format!("CASE WHEN COUNT_BIG({column})=COUNT_BIG(*) THEN CONVERT(varchar(40),{aggregate}) END AS {}", self.key)
    }
}
const FIELDS: &[Field] = &[
    Field { key: "clerk_count", unit: CounterUnit::Count, aggregation: Aggregation::Count },
    Field { key: "pages_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field { key: "virtual_memory_reserved_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field { key: "virtual_memory_committed_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field { key: "awe_allocated_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field { key: "shared_memory_reserved_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field { key: "shared_memory_committed_kb", unit: CounterUnit::Kibibytes, aggregation: Aggregation::Sum },
    Field {
        key: "minimum_page_size_in_bytes",
        unit: CounterUnit::Bytes,
        aggregation: Aggregation::Minimum(PAGE_SIZE_COLUMN),
    },
    Field {
        key: "maximum_page_size_in_bytes",
        unit: CounterUnit::Bytes,
        aggregation: Aggregation::Maximum(PAGE_SIZE_COLUMN),
    },
];

fn query() -> String {
    let fields = FIELDS.iter().map(Field::projection).collect::<Vec<_>>().join(",");
    let columns = FIELDS
        .iter()
        .filter(|field| !matches!(field.aggregation, Aggregation::Count))
        .map(Field::column)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    // Binary keys preserve case/trailing spaces and make grouping independent of the member database's collation.
    format!("WITH native_clerks AS (SELECT /* culprit */
        CONVERT(varbinary(120),{TYPE_COLUMN}) AS type_key,CONVERT(varbinary(512),{NAME_COLUMN}) AS name_key,
        {NODE_COLUMN},{columns} FROM {NATIVE_OBJECT})
        SELECT TOP (@P1) CONVERT(nvarchar(60),type_key) AS {TYPE_COLUMN},CONVERT(nvarchar(256),name_key) AS {NAME_COLUMN},
        {NODE_COLUMN},{fields} FROM native_clerks GROUP BY type_key,name_key,{NODE_COLUMN}
        ORDER BY type_key,name_key,{NODE_COLUMN} OPTION(MAXDOP 1)")
}

/// The caller owns the borrowed reader through confirmed completion/cancellation; this function never retries.
pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<Vec<NativeCounterSet>> {
    let limit = limit.min(MAX_CLERK_GROUPS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .query(&query(), vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let sample = decode(row)?;
            if !seen.insert(sample.identity.clone()) {
                return Err(invalid_data());
            }
            Ok(sample)
        })
        .collect()
}

fn decode(row: &Rec) -> Result<NativeCounterSet> {
    let clerk_type = optional_text(row, TYPE_COLUMN)?.filter(|value| !value.is_empty()).ok_or_else(invalid_data)?;
    let clerk_name = optional_text(row, NAME_COLUMN)?.ok_or_else(invalid_data)?;
    let node = i16::try_from(signed_integer(row, NODE_COLUMN)?).map_err(|_| invalid_data())?;
    if clerk_type.chars().count() > 60 || clerk_name.encode_utf16().count() > 256 {
        return Err(invalid_data());
    }
    let identity = NativeIdentity::new(&clerk_type)
        .qualified("native_object", NATIVE_OBJECT)
        .qualified(NODE_COLUMN, node.to_string())
        .qualified("name_sha256", digest(clerk_name.as_bytes()));
    let counters = FIELDS
        .iter()
        .filter_map(|field| match scalar(row, field.key) {
            Ok(Some(value)) if matches!(field.aggregation, Aggregation::Count) && value == 0 => {
                Some(Err(invalid_data()))
            }
            Ok(Some(value)) => Some(Ok(NativeCounterSample::gauge(
                field.key,
                value,
                field.unit,
                MeasurementScope::Server,
                NATIVE_OBJECT,
                field.column(),
            ))),
            Ok(None) if matches!(field.aggregation, Aggregation::Count) => Some(Err(invalid_data())),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let sample = NativeCounterSet {
        identity,
        scope: MeasurementScope::Server,
        counters,
        properties: BTreeMap::from([
            (TYPE_COLUMN.into(), clerk_type),
            (NAME_COLUMN.into(), clerk_name),
            (NODE_COLUMN.into(), node.to_string()),
            ("aggregation".into(), "clerk-type-name-and-memory-node".into()),
        ]),
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(sample)
}
fn scalar(row: &Rec, column: &str) -> Result<Option<u64>> {
    optional_text(row, column)?.map(|value| value.parse::<u64>().map_err(|_| invalid_data())).transpose()
}

#[cfg(test)]
pub(crate) mod tests;
