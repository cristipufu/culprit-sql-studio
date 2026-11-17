//! Sharing a scan withholds the read endpoint and owner label while keeping native point measurements intact.
use super::*;
use crate::telemetry::*;
pub(super) fn redact(mut read: Box<TimescaleJobsSnapshot>) -> crate::Result<Box<TimescaleJobsSnapshot>> {
    if !read.is_valid_read(false) {
        return Err(CollectionStop::InvalidPage.error());
    }
    redact_read_context(&mut read.context);
    for row in &mut read.observations {
        match &mut row.fact {
            OperationalFact::TimescaleJobState(point) => {
                point.owner = opaque(&point.owner);
                procedure(&mut point.procedure);
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
    read.is_valid_read(false).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}
fn procedure(value: &mut TimescaleJobProcedure) {
    value.schema = shareable_name(&value.schema);
    value.name = shareable_name(&value.name);
}
