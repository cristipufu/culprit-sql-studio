//! Supported retained OOM snapshots. This DMV is unsupported on boxed SQL Server even when its object exists.
//! Native row positions belong to this capture, not to a persistent event feed. Event time has no established
//! offset; native causes are heuristic and the diagnostic factor identifies no responsible query.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-out-of-memory-events
use crate::{
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_signed_integer, optional_text,
        signed_integer, text,
    },
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

pub(crate) const NATIVE_OBJECT: &str = "sys.dm_os_out_of_memory_events";
const PLATFORM: &str = "SELECT /* culprit */ CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition";
const MAX_ROWS: usize = 128;
const EVENT_TIME: &str = "event_time";
const CAUSE: &str = "oom_cause";
const CAUSE_DESCRIPTION: &str = "oom_cause_desc";
const FACTOR: &str = "oom_factor";
const FACTOR_DESCRIPTION: &str = "oom_factor_desc";

#[derive(Clone, Copy)]
enum MemoryField {
    AvailablePhysical,
    ProcessUsage,
    NonSosUsage,
    CommittedTarget,
    Committed,
    AllocationPotential,
    InitialJobLimit,
    CurrentJobLimit,
    PossibleNonSosLeak,
}
impl MemoryField {
    const ALL: [Self; 9] = [
        Self::AvailablePhysical,
        Self::ProcessUsage,
        Self::NonSosUsage,
        Self::CommittedTarget,
        Self::Committed,
        Self::AllocationPotential,
        Self::InitialJobLimit,
        Self::CurrentJobLimit,
        Self::PossibleNonSosLeak,
    ];
    fn column(self) -> &'static str {
        match self {
            Self::AvailablePhysical => "available_physical_memory_mb",
            Self::ProcessUsage => "process_memory_usage_mb",
            Self::NonSosUsage => "non_sos_memory_usage_mb",
            Self::CommittedTarget => "committed_memory_target_mb",
            Self::Committed => "committed_memory_mb",
            Self::AllocationPotential => "allocation_potential_memory_mb",
            Self::InitialJobLimit => "initial_job_object_memory_limit_mb",
            Self::CurrentJobLimit => "current_job_object_memory_limit_mb",
            Self::PossibleNonSosLeak => "possible_non_sos_leaked_memory_mb",
        }
    }
    fn required(self, row: &Rec) -> Result<i32> {
        i32::try_from(signed_integer(row, self.column())?).map_err(|_| invalid_data())
    }
    fn optional(self, row: &Rec) -> Result<Option<i32>> {
        optional_signed_integer(row, self.column())?
            .map(|value| i32::try_from(value).map_err(|_| invalid_data()))
            .transpose()
    }
}
#[derive(Clone, Copy)]
enum JsonField {
    OomPools,
    TopClerks,
    TopPools,
    PossibleLeakedClerks,
}
impl JsonField {
    const ALL: [Self; 4] = [Self::OomPools, Self::TopClerks, Self::TopPools, Self::PossibleLeakedClerks];
    fn column(self) -> &'static str {
        match self {
            Self::OomPools => "oom_resource_pools",
            Self::TopClerks => "top_memory_clerks",
            Self::TopPools => "top_resource_pools",
            Self::PossibleLeakedClerks => "possible_leaked_memory_clerks",
        }
    }
    fn decode(self, row: &Rec) -> Result<Option<MemoryFailureJsonDocument>> {
        optional_text(row, self.column())?.map(MemoryFailureJsonDocument::from_raw).transpose()
    }
}
fn query() -> String {
    let columns = MemoryField::ALL
        .into_iter()
        .map(MemoryField::column)
        .chain(JsonField::ALL.into_iter().map(JsonField::column))
        .collect::<Vec<_>>()
        .join(",");
    // TOP bounds native rows without introducing a sort or a diagnostic memory grant. All JSON remains bounded
    // native nvarchar(4000); decoding happens in Culprit, not through OPENJSON/XQuery on the affected engine.
    format!(
        "SELECT TOP (@P1) /* culprit */ CONVERT(varchar(23),{EVENT_TIME},126) AS {EVENT_TIME},
        {CAUSE},{CAUSE_DESCRIPTION},{FACTOR},{FACTOR_DESCRIPTION},{columns}
        FROM {NATIVE_OBJECT} OPTION(MAXDOP 1)"
    )
}

/// None is a known unsupported boxed SQL Server product. Other failures retain their native error; no fallback.
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    limit: usize,
) -> Result<Option<Vec<MssqlOutOfMemoryObservation>>> {
    let limit = limit.min(MAX_ROWS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let platform = reader.query(PLATFORM, vec![], 1).await.map_err(ReadError::into_error)?;
    if platform.len() != 1 {
        return Err(invalid_data());
    }
    match integer(&platform[0], "edition")? {
        5 | 8 => {}
        2..=4 => return Ok(None),
        _ => return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    }
    let rows =
        reader.query(&query(), vec![Parameter::Integer((limit + 1) as i64)], limit + 1).await.map_err(source_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    rows.iter().enumerate().map(|(position, row)| decode(row, position)).collect::<Result<Vec<_>>>().map(Some)
}

fn source_error(error: ReadError) -> Error {
    let error = error.into_error();
    if error.code == ErrorCode::PermissionDenied && error.detail.is_none() {
        error.with_hint(culprit_core::message!("provider.mssql.memory_failures.permissions"))
    } else {
        error
    }
}

fn decode(row: &Rec, position: usize) -> Result<MssqlOutOfMemoryObservation> {
    let observation = MssqlOutOfMemoryObservation {
        position: u32::try_from(position).map_err(|_| invalid_data())?,
        event_time_without_offset: text(row, EVENT_TIME)?,
        oom_cause: u8::try_from(integer(row, CAUSE)?).map_err(|_| invalid_data())?,
        oom_cause_desc: text(row, CAUSE_DESCRIPTION)?,
        oom_factor: u8::try_from(integer(row, FACTOR)?).map_err(|_| invalid_data())?,
        oom_factor_desc: text(row, FACTOR_DESCRIPTION)?,
        available_physical_memory_mb: MemoryField::AvailablePhysical.required(row)?,
        process_memory_usage_mb: MemoryField::ProcessUsage.required(row)?,
        non_sos_memory_usage_mb: MemoryField::NonSosUsage.required(row)?,
        committed_memory_target_mb: MemoryField::CommittedTarget.required(row)?,
        committed_memory_mb: MemoryField::Committed.required(row)?,
        allocation_potential_memory_mb: MemoryField::AllocationPotential.required(row)?,
        initial_job_object_memory_limit_mb: MemoryField::InitialJobLimit.optional(row)?,
        current_job_object_memory_limit_mb: MemoryField::CurrentJobLimit.optional(row)?,
        possible_non_sos_leaked_memory_mb: MemoryField::PossibleNonSosLeak.optional(row)?,
        oom_resource_pools: JsonField::OomPools.decode(row)?,
        top_memory_clerks: JsonField::TopClerks.decode(row)?,
        top_resource_pools: JsonField::TopPools.decode(row)?,
        possible_leaked_memory_clerks: JsonField::PossibleLeakedClerks.decode(row)?,
    };
    if observation.is_valid() { Ok(observation) } else { Err(invalid_data()) }
}

#[cfg(test)]
pub(crate) mod tests;
