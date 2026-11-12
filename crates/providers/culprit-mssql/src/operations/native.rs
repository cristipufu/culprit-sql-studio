//! Native operational facts, without localized space names or inferred counter epochs.
use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_nonnegative_number as optional_number},
    value::Rec,
};
use culprit_core::{FileKind, FileScope, Result, operations::SequenceCapacity, telemetry::CollectionStop};
const MAX_OPERATION_ROWS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Log,
    Filegroups,
    Temp,
    Sequences,
    SuspectPages,
}
impl Kind {
    pub const ALL: [Self; 5] = [Self::Log, Self::Filegroups, Self::Temp, Self::Sequences, Self::SuspectPages];
    pub fn source(self) -> &'static culprit_core::Source {
        match self {
            Self::Log => &crate::sources::LOG_CAPACITY,
            Self::Filegroups => &crate::sources::FILE_CAPACITY,
            Self::Temp => &crate::sources::TEMP_CAPACITY,
            Self::Sequences => &crate::sources::SEQUENCE_CAPACITY,
            Self::SuspectPages => &crate::sources::SUSPECT_PAGES,
        }
    }
    fn sql(self) -> &'static str {
        match self {
            Self::Log => LOG_SQL,
            Self::Filegroups => FILES_SQL,
            Self::Temp => TEMP_SQL,
            Self::Sequences => SEQUENCES_SQL,
            Self::SuspectPages => SUSPECT_PAGES_SQL,
        }
    }
}
pub(crate) struct SpaceFact {
    pub kind: FileKind,
    pub scope: FileScope,
    pub native_name: Option<String>,
    pub key: String,
    pub total_bytes: Option<f64>,
    pub used_bytes: Option<f64>,
    pub limit_bytes: Option<f64>,
    pub growth_enabled: Option<bool>,
    pub reuse_wait: Option<String>,
    pub parts: Vec<SpacePart>,
}
pub(crate) struct SpacePart {
    pub kind: SpacePartKind,
    pub bytes: Option<f64>,
}
#[derive(Clone, Copy)]
pub(crate) enum SpacePartKind {
    Users,
    Internals,
    Versions,
}
pub(crate) struct SequenceFact {
    pub key: String,
    pub capacity: SequenceCapacity,
}
pub(crate) struct SuspectPages {
    pub count: u64,
    /// The native datetime has no offset; retain it without assigning a UTC timestamp.
    pub last_native_at: Option<String>,
}
pub(crate) enum Facts {
    Spaces(Vec<SpaceFact>),
    Sequences(Vec<SequenceFact>),
    SuspectPages(SuspectPages),
}
impl Facts {
    pub fn len(&self) -> usize {
        match self {
            Self::Spaces(rows) => rows.len(),
            Self::Sequences(rows) => rows.len(),
            Self::SuspectPages(_) => 1,
        }
    }
    pub fn observations(self, kind: Kind) -> Vec<culprit_core::telemetry::OperationalObservation> {
        use culprit_core::telemetry::{
            OperationalFact, OperationalObservation, OperationalSpace, SpaceAllocation, SpaceAllocationKind,
        };
        let wrap = |key, fact| OperationalObservation { source: kind.source().name.into(), key, fact };
        match self {
            Self::Spaces(facts) => facts
                .into_iter()
                .map(|space| {
                    wrap(
                        space.key,
                        OperationalFact::Space(OperationalSpace {
                            kind: space.kind,
                            scope: space.scope,
                            native_name: space.native_name,
                            total_bytes: space.total_bytes,
                            used_bytes: space.used_bytes,
                            limit_bytes: space.limit_bytes,
                            growth_enabled: space.growth_enabled,
                            reuse_wait: space.reuse_wait,
                            parts: space
                                .parts
                                .into_iter()
                                .map(|part| SpaceAllocation {
                                    kind: match part.kind {
                                        SpacePartKind::Users => SpaceAllocationKind::UserObjects,
                                        SpacePartKind::Internals => SpaceAllocationKind::InternalObjects,
                                        SpacePartKind::Versions => SpaceAllocationKind::VersionStore,
                                    },
                                    bytes: part.bytes,
                                })
                                .collect(),
                        }),
                    )
                })
                .collect(),
            Self::Sequences(facts) => {
                facts.into_iter().map(|fact| wrap(fact.key, OperationalFact::Sequence(fact.capacity))).collect()
            }
            Self::SuspectPages(fact) => vec![wrap(
                "retained".into(),
                OperationalFact::SuspectPages {
                    count: fact.count.to_string(),
                    last_reported_time_without_offset: fact.last_native_at,
                    reset: culprit_core::ResetMarker::Unknown,
                },
            )],
        }
    }
}
pub(crate) async fn read<R: NativeRead>(io: &mut R, kind: Kind, limit: usize) -> Result<Facts> {
    let sql = format!("{} OPTION(MAXDOP 1)", kind.sql());
    let rows = io.query(&sql, vec![], limit.min(MAX_OPERATION_ROWS) + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit.min(MAX_OPERATION_ROWS) {
        return Err(CollectionStop::RowLimit.error());
    }
    match kind {
        Kind::Log | Kind::Filegroups | Kind::Temp => {
            rows.iter().map(|row| space(row, kind)).collect::<Result<Vec<_>>>().map(Facts::Spaces)
        }
        Kind::Sequences => rows.iter().map(sequence).collect::<Result<Vec<_>>>().map(Facts::Sequences),
        Kind::SuspectPages => {
            if rows.len() != 1 {
                return Err(invalid_data());
            }
            Ok(Facts::SuspectPages(SuspectPages {
                count: integer(&rows[0], "count")?.try_into().map_err(|_| invalid_data())?,
                last_native_at: rows[0].str("last_native_at"),
            }))
        }
    }
}
fn space(row: &Rec, kind: Kind) -> Result<SpaceFact> {
    let (file_kind, scope) = match kind {
        Kind::Log => (FileKind::Log, FileScope::Database),
        Kind::Filegroups => (FileKind::Data, FileScope::Database),
        Kind::Temp => (FileKind::Temp, FileScope::Server),
        _ => return Err(invalid_data()),
    };
    let temp = kind == Kind::Temp;
    let mut parts = vec![];
    if temp {
        for (kind, column) in [
            (SpacePartKind::Users, "users"),
            (SpacePartKind::Internals, "internals"),
            (SpacePartKind::Versions, "versions"),
        ] {
            parts.push(SpacePart { kind, bytes: optional_number(row, column)? });
        }
    }
    Ok(SpaceFact {
        key: match kind {
            Kind::Log => "database".into(),
            Kind::Temp => "shared-tempdb".into(),
            _ => integer(row, "native_id")?.to_string(),
        },
        kind: file_kind,
        scope,
        native_name: row.str("name"),
        total_bytes: optional_number(row, "total")?,
        used_bytes: optional_number(row, "used")?,
        limit_bytes: if temp { None } else { optional_number(row, "max_bytes")? },
        growth_enabled: if temp {
            None
        } else {
            match row.f64("can_grow") {
                Some(0.0) => Some(false),
                Some(1.0) => Some(true),
                None => None,
                _ => return Err(invalid_data()),
            }
        },
        reuse_wait: row.str("reuse_wait"),
        parts,
    })
}
fn sequence(row: &Rec) -> Result<SequenceFact> {
    let required = |column| crate::read::text(row, column);
    let cycles = match integer(row, "cycles")? {
        0 => false,
        1 => true,
        _ => return Err(invalid_data()),
    };
    Ok(SequenceFact {
        key: required("native_id")?,
        capacity: SequenceCapacity {
            name: required("name")?,
            table: row.str("table_name"),
            current: required("current_value")?,
            increment: required("step")?,
            minimum: required("min_value")?,
            maximum: required("max_value")?,
            cycles,
            allocated: true,
        },
    })
}
const SUSPECT_PAGES_SQL: &str = "SELECT COUNT_BIG(*) AS count,
 CONVERT(varchar(30), MAX(last_update_date), 126) AS last_native_at
 FROM msdb.dbo.suspect_pages
 WHERE database_id = DB_ID() AND event_type IN (1, 2, 3)";

const LOG_SQL: &str = "SELECT total_log_size_in_bytes AS total, used_log_space_in_bytes AS used,
 (SELECT log_reuse_wait_desc FROM sys.databases WHERE database_id = DB_ID()) AS reuse_wait,
 (SELECT MAX(CASE WHEN growth > 0 AND (max_size = -1 OR size < max_size) THEN 1 ELSE 0 END) FROM sys.database_files WHERE type = 1) AS can_grow,
 (SELECT CASE WHEN MAX(CASE WHEN growth > 0 AND max_size = -1 THEN 1 ELSE 0 END) = 1 THEN NULL ELSE SUM(CAST(CASE WHEN growth = 0 OR max_size < size THEN size ELSE max_size END AS bigint)) * 8192.0 END FROM sys.database_files WHERE type = 1) AS max_bytes
 FROM sys.dm_db_log_space_usage";

const FILES_SQL: &str = "SELECT fg.data_space_id AS native_id,fg.name, SUM(CAST(df.size AS bigint)) * 8192.0 AS total,
 CASE WHEN COUNT(FILEPROPERTY(df.name, 'SpaceUsed')) = COUNT(*) THEN SUM(CAST(FILEPROPERTY(df.name, 'SpaceUsed') AS bigint)) * 8192.0 END AS used,
 MAX(CASE WHEN df.growth > 0 AND (df.max_size = -1 OR df.size < df.max_size) THEN 1 ELSE 0 END) AS can_grow,
 CASE WHEN MAX(CASE WHEN df.growth > 0 AND df.max_size = -1 THEN 1 ELSE 0 END) = 1 THEN NULL ELSE SUM(CAST(CASE WHEN df.growth = 0 OR df.max_size < df.size THEN df.size ELSE df.max_size END AS bigint)) * 8192.0 END AS max_bytes
 FROM sys.database_files df JOIN sys.filegroups fg ON fg.data_space_id = df.data_space_id
 WHERE df.type = 0 AND df.state = 0 AND fg.is_read_only = 0 GROUP BY fg.data_space_id,fg.name";

const TEMP_SQL: &str = "SELECT SUM(total_page_count) * 8192.0 AS total,
 SUM(total_page_count - unallocated_extent_page_count) * 8192.0 AS used,
 SUM(user_object_reserved_page_count) * 8192.0 AS users,
 SUM(internal_object_reserved_page_count) * 8192.0 AS internals,
 SUM(version_store_reserved_page_count) * 8192.0 AS versions
 FROM tempdb.sys.dm_db_file_space_usage";

const SEQUENCES_SQL: &str = "SELECT CONCAT('sequence:',q.object_id) AS native_id,s.name + '.' + q.name AS name, CAST(NULL AS nvarchar(512)) AS table_name,
 CONVERT(nvarchar(40), q.current_value) AS current_value, CONVERT(nvarchar(40), q.increment) AS step,
 CONVERT(nvarchar(40), q.minimum_value) AS min_value, CONVERT(nvarchar(40), q.maximum_value) AS max_value,
 CAST(q.is_cycling AS int) AS cycles, 1 AS allocated
 FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id
 WHERE q.current_value IS NOT NULL
 UNION ALL
 SELECT CONCAT('identity:',c.object_id,':',c.column_id),s.name + '.' + t.name + '.' + c.name, s.name + '.' + t.name,
 CONVERT(nvarchar(40), c.last_value), CONVERT(nvarchar(40), c.increment_value),
 CASE TYPE_NAME(c.system_type_id) WHEN 'tinyint' THEN '0' WHEN 'smallint' THEN '-32768' WHEN 'int' THEN '-2147483648' WHEN 'bigint' THEN '-9223372036854775808' WHEN 'numeric' THEN '-' + REPLICATE('9', c.precision) WHEN 'decimal' THEN '-' + REPLICATE('9', c.precision) END,
 CASE TYPE_NAME(c.system_type_id) WHEN 'tinyint' THEN '255' WHEN 'smallint' THEN '32767' WHEN 'int' THEN '2147483647' WHEN 'bigint' THEN '9223372036854775807' WHEN 'numeric' THEN REPLICATE('9', c.precision) WHEN 'decimal' THEN REPLICATE('9', c.precision) END,
 0, 1 FROM sys.identity_columns c JOIN sys.tables t ON t.object_id = c.object_id
 JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE t.is_ms_shipped = 0 AND c.last_value IS NOT NULL";
