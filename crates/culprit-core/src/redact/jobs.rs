//! Sharing a scan withholds the read endpoint and owner label while keeping native point measurements intact.
use super::*;
use crate::telemetry::*;
pub(super) fn redact(read: JobsSnapshot) -> crate::Result<Box<JobsSnapshot>> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    match read {
        JobsSnapshot::Timescale { snapshot, read_complete } => {
            let snapshot = redact_timescale(snapshot, read_complete)?;
            Ok(Box::new(JobsSnapshot::Timescale { snapshot, read_complete }))
        }
        // Job labels and immutable native grouping identities cannot be masked and reissued as original evidence.
        JobsSnapshot::MssqlAgent { .. } => Err(CollectionStop::InvalidPage.error()),
    }
}
fn redact_timescale(
    mut read: Box<TimescaleJobsSnapshot>,
    read_complete: bool,
) -> crate::Result<Box<TimescaleJobsSnapshot>> {
    if !read.is_valid_read(read_complete) {
        return Err(CollectionStop::InvalidPage.error());
    }
    redact_read_context(&mut read.context);
    for row in &mut read.observations {
        match &mut row.fact {
            OperationalFact::TimescaleJobState(point) => {
                point.owner = opaque(&point.owner);
                procedure(&mut point.procedure);
                if let Some(check) = &mut point.configuration_check {
                    procedure(check);
                }
                if let Some(object) = &mut point.hypertable {
                    procedure(object);
                }
                if let Some(statistics) = &mut point.statistics {
                    if let Some(TimescaleReportedJobState::Unknown { native }) = &mut statistics.reported_job_state {
                        *native = opaque(native);
                    }
                    if let Some(TimescaleJobLastRun::Unknown { native }) = &mut statistics.last_run_status {
                        *native = opaque(native);
                    }
                }
                row.key = point.key()?;
            }
            OperationalFact::TimescaleJobRun(point) => {
                if let Some(reported) = &mut point.procedure {
                    procedure(&mut reported.label);
                }
                row.key = point.key()?;
            }
            _ => return Err(CollectionStop::InvalidPage.error()),
        }
    }
    read.is_valid_read(read_complete).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}
fn procedure(value: &mut TimescaleJobProcedure) {
    value.schema = shareable_name(&value.schema);
    value.name = shareable_name(&value.name);
}
