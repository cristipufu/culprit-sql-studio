//! Native operational facts, without localized space names or inferred counter epochs.
use crate::{
    read::{
        NativeRead, ReadError, integer, invalid_data, optional_boolean, optional_counter, optional_text,
        signed_integer, text,
    },
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
#[derive(Clone)]
pub(crate) struct SpaceFact {
    pub population: culprit_core::telemetry::StoragePopulation,
    pub observed_at: String,
    pub database: Option<String>,
    pub kind: FileKind,
    pub scope: FileScope,
    pub native_name: Option<String>,
    pub key: String,
    pub total_bytes: Option<String>,
    pub used_bytes: Option<String>,
    pub limit_bytes: Option<String>,
    pub growth_enabled: Option<bool>,
    pub reuse_wait: Option<String>,
    pub parts: Vec<SpacePart>,
}
#[derive(Clone)]
pub(crate) struct SpacePart {
    pub kind: SpacePartKind,
    pub bytes: Option<String>,
}
#[derive(Clone, Copy)]
pub(crate) enum SpacePartKind {
    Users,
    Internals,
    Versions,
}
#[derive(Clone)]
pub(crate) struct SequenceFact {
    pub key: String,
    pub capacity: SequenceCapacity,
}
#[derive(Clone)]
pub(crate) struct SuspectPages {
    pub count: u64,
    /// The native datetime has no offset; retain it without assigning a UTC timestamp.
    pub last_native_at: Option<String>,
}
#[derive(Clone)]
pub(crate) enum Facts {
    Spaces(Vec<SpaceFact>),
    Files { spaces: Vec<SpaceFact>, files: Vec<culprit_core::telemetry::MssqlDatabaseFile> },
    Sequences(Vec<SequenceFact>),
    SuspectPages(SuspectPages),
}
impl Facts {
    pub fn len(&self) -> usize {
        match self {
            Self::Spaces(rows) => rows.len(),
            Self::Files { spaces, files } => spaces.len() + files.len(),
            Self::Sequences(rows) => rows.len(),
            Self::SuspectPages(_) => 1,
        }
    }
    pub fn observations(self, kind: Kind) -> Result<Vec<culprit_core::telemetry::OperationalObservation>> {
        use culprit_core::telemetry::{OperationalFact, OperationalObservation};
        let wrap = |key, fact| OperationalObservation { source: kind.source().name.into(), key, fact };
        Ok(match self {
            Self::Spaces(facts) => {
                facts.into_iter().map(|space| space.observation(kind)).collect::<Result<Vec<_>>>()?
            }
            Self::Files { spaces, files } => spaces
                .into_iter()
                .map(|space| space.observation(kind))
                .chain(
                    files
                        .into_iter()
                        .map(|file| Ok(wrap(file.key()?, OperationalFact::MssqlDatabaseFile(Box::new(file))))),
                )
                .collect::<Result<Vec<_>>>()?,
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
        })
    }
}
pub(crate) async fn read<R: NativeRead>(io: &mut R, kind: Kind, limit: usize) -> Result<Facts> {
    let sql = format!("{} OPTION(MAXDOP 1)", kind.sql());
    let rows = io.query(&sql, vec![], limit.min(MAX_OPERATION_ROWS) + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit.min(MAX_OPERATION_ROWS) {
        return Err(CollectionStop::RowLimit.error());
    }
    match kind {
        Kind::Filegroups => {
            let mut spaces = Vec::new();
            let mut files = Vec::new();
            for row in &rows {
                match integer(row, "row_kind")? {
                    0 => spaces.push(space(row, kind)?),
                    1 => files.push(database_file(row)?),
                    _ => return Err(invalid_data()),
                }
            }
            if files.is_empty() {
                return Err(invalid_data());
            }
            let mut ids = std::collections::BTreeSet::new();
            if files.iter().any(|file| !ids.insert(file.file_id.clone())) {
                return Err(invalid_data());
            }
            Ok(Facts::Files { spaces, files })
        }
        Kind::Log | Kind::Temp => {
            let [row] = rows.as_slice() else {
                return Err(invalid_data());
            };
            Ok(Facts::Spaces(vec![space(row, kind)?]))
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
            parts.push(SpacePart { kind, bytes: optional_counter(row, column)? });
        }
    }
    let observed_at = text(row, "observed_at")?;
    culprit_core::telemetry::canonical_time(&observed_at)?;
    Ok(SpaceFact {
        population: match kind {
            Kind::Log => culprit_core::telemetry::StoragePopulation::DatabaseTransactionLog,
            Kind::Filegroups => culprit_core::telemetry::StoragePopulation::WritableOnlineDataFilegroups,
            Kind::Temp => culprit_core::telemetry::StoragePopulation::SharedTempdbDataFiles,
            _ => return Err(invalid_data()),
        },
        observed_at,
        database: optional_text(row, "database_name")?,
        key: match kind {
            Kind::Log => "database".into(),
            Kind::Temp => "shared-tempdb".into(),
            _ => format!("filegroup:{}", integer(row, "native_id")?),
        },
        kind: file_kind,
        scope,
        native_name: row.str("name"),
        total_bytes: optional_counter(row, "total")?,
        used_bytes: optional_counter(row, "used")?,
        limit_bytes: if temp { None } else { optional_counter(row, "max_bytes")? },
        growth_enabled: if temp { None } else { optional_boolean(row, "can_grow")? },
        reuse_wait: row.str("reuse_wait"),
        parts,
    })
}
impl SpaceFact {
    pub(crate) fn observation(self, kind: Kind) -> Result<culprit_core::telemetry::OperationalObservation> {
        use culprit_core::telemetry::*;
        let observation = OperationalObservation {
            source: kind.source().name.into(),
            key: self.key,
            fact: OperationalFact::Space(OperationalSpace {
                kind: self.kind,
                scope: self.scope,
                population: self.population,
                observed_at: self.observed_at,
                database: self.database,
                native_name: self.native_name,
                total_bytes: self.total_bytes,
                used_bytes: self.used_bytes,
                limit_bytes: self.limit_bytes,
                growth_enabled: self.growth_enabled,
                reuse_wait: self.reuse_wait,
                parts: self
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
        };
        observation.is_valid().then_some(observation).ok_or_else(invalid_data)
    }
}
fn database_file(row: &Rec) -> Result<culprit_core::telemetry::MssqlDatabaseFile> {
    use culprit_core::telemetry::{MssqlDatabaseFile, MssqlDatabaseFileProfile};
    let required = |key| optional_counter(row, key)?.ok_or_else(invalid_data);
    let native_type = u8::try_from(integer(row, "file_type")?).map_err(|_| invalid_data())?;
    let native_state = u8::try_from(integer(row, "file_state")?).map_err(|_| invalid_data())?;
    let snapshot = optional_boolean(row, "database_is_snapshot")?;
    let profile = MssqlDatabaseFileProfile::from_native(native_type, native_state, snapshot);
    let used_pages = optional_counter(row, "used_pages")?;
    if used_pages.is_some() && !profile.used_pages_supported {
        return Err(invalid_data());
    }
    let file = MssqlDatabaseFile {
        native_object: "sys.database_files".into(),
        kind: profile.kind,
        size_basis: profile.size_basis,
        page_size_bytes: culprit_core::telemetry::MssqlDatabaseFile::PAGE_SIZE_BYTES,
        observed_at: text(row, "observed_at")?,
        database: text(row, "database_name")?,
        native_database_id: required("database_id")?,
        file_id: required("native_id")?,
        file_guid: optional_text(row, "file_guid")?,
        native_type: native_type.to_string(),
        type_description: optional_text(row, "file_type_desc")?,
        native_state: native_state.to_string(),
        state_description: optional_text(row, "file_state_desc")?,
        data_space_id: required("data_space_id")?,
        logical_name: text(row, "name")?,
        size_pages: required("size_pages")?,
        maximum_size_pages: signed_integer(row, "max_size_pages")?.to_string(),
        growth: required("growth")?,
        growth_is_percent: optional_boolean(row, "is_percent_growth")?,
        is_read_only: optional_boolean(row, "is_read_only")?,
        filegroup_read_only: optional_boolean(row, "filegroup_read_only")?,
        is_sparse: optional_boolean(row, "is_sparse")?,
        database_is_snapshot: snapshot,
        used_pages,
    };
    file.is_valid().then_some(file).ok_or_else(invalid_data)
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

const LOG_SQL: &str = "DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
 SELECT /* culprit */ @observed_at AS observed_at,DB_NAME() AS database_name,
 total_log_size_in_bytes AS total,used_log_space_in_bytes AS used,
 (SELECT log_reuse_wait_desc FROM sys.databases WHERE database_id=DB_ID()) AS reuse_wait,
 (SELECT MAX(CASE WHEN growth>0 AND(max_size=-1 OR size<max_size) THEN 1 ELSE 0 END) FROM sys.database_files WHERE type=1) AS can_grow,
 (SELECT CASE WHEN MAX(CASE WHEN growth>0 AND max_size=-1 THEN 1 ELSE 0 END)=1 THEN NULL ELSE SUM(CONVERT(bigint,CASE WHEN growth=0 OR max_size<size THEN size ELSE max_size END))*CONVERT(bigint,8192) END FROM sys.database_files WHERE type=1) AS max_bytes
 FROM sys.dm_db_log_space_usage";

// One existing catalog dispatch now keeps every visible file independently of the narrower growth population.
const FILES_SQL: &str = "DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
 WITH files AS(SELECT df.*,fg.name AS filegroup_name,fg.is_read_only AS filegroup_read_only,
 CASE WHEN df.type=0 AND df.state=0 THEN FILEPROPERTY(df.name,'SpaceUsed') END AS used_pages,
 (SELECT CASE WHEN source_database_id IS NULL THEN CAST(0 AS bit) ELSE CAST(1 AS bit) END FROM sys.databases WHERE database_id=DB_ID()) AS database_is_snapshot
 FROM sys.database_files df LEFT JOIN sys.filegroups fg ON fg.data_space_id=df.data_space_id)
 SELECT /* culprit */ 0 AS row_kind,@observed_at AS observed_at,DB_NAME() AS database_name,DB_ID() AS database_id,
 data_space_id AS native_id,filegroup_name AS name,
 SUM(CONVERT(bigint,size))*CONVERT(bigint,8192) AS total,
 CASE WHEN COUNT(used_pages)=COUNT(*) THEN SUM(CONVERT(bigint,used_pages))*CONVERT(bigint,8192) END AS used,
 MAX(CASE WHEN growth>0 AND(max_size=-1 OR size<max_size) THEN 1 ELSE 0 END) AS can_grow,
 CASE WHEN MAX(CASE WHEN growth>0 AND max_size=-1 THEN 1 ELSE 0 END)=1 THEN NULL ELSE SUM(CONVERT(bigint,CASE WHEN growth=0 OR max_size<size THEN size ELSE max_size END))*CONVERT(bigint,8192) END AS max_bytes,
 CAST(NULL AS varchar(36)) AS file_guid,CAST(NULL AS tinyint) AS file_type,CAST(NULL AS nvarchar(60)) AS file_type_desc,
 CAST(NULL AS tinyint) AS file_state,CAST(NULL AS nvarchar(60)) AS file_state_desc,CAST(NULL AS int) AS data_space_id,
 CAST(NULL AS int) AS size_pages,CAST(NULL AS int) AS max_size_pages,CAST(NULL AS int) AS growth,
 CAST(NULL AS bit) AS is_percent_growth,CAST(NULL AS bit) AS is_read_only,CAST(NULL AS bit) AS filegroup_read_only,
 CAST(NULL AS bit) AS is_sparse,CAST(NULL AS bit) AS database_is_snapshot,CAST(NULL AS int) AS used_pages
 FROM files WHERE type=0 AND state=0 AND filegroup_read_only=0 GROUP BY data_space_id,filegroup_name
 UNION ALL
 SELECT 1,@observed_at,DB_NAME(),DB_ID(),file_id,name,NULL,NULL,NULL,NULL,CONVERT(varchar(36),file_guid),type,type_desc,state,state_desc,
 data_space_id,size,max_size,growth,is_percent_growth,is_read_only,filegroup_read_only,is_sparse,database_is_snapshot,used_pages
 FROM files ORDER BY row_kind,native_id";

const TEMP_SQL: &str = "DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
 SELECT /* culprit */ @observed_at AS observed_at,CAST(NULL AS nvarchar(128)) AS database_name,
 SUM(total_page_count)*CONVERT(bigint,8192) AS total,
 SUM(total_page_count-unallocated_extent_page_count)*CONVERT(bigint,8192) AS used,
 SUM(user_object_reserved_page_count)*CONVERT(bigint,8192) AS users,
 SUM(internal_object_reserved_page_count)*CONVERT(bigint,8192) AS internals,
 SUM(version_store_reserved_page_count)*CONVERT(bigint,8192) AS versions
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

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
