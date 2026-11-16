//! The same column-detected native projection feeds scan work and exact collected counter points.
//! Temporary blocks are local executor work, not DocDB reads, memory allocation or an elapsed-time component.
//! https://docs.yugabyte.com/stable/launch-and-manage/monitor-and-alert/query-tuning/pg-stat-statements/
use super::*;
use culprit_core::StatementTemporaryWork;

const READ_BLOCKS: &str = "temp_blks_read";
const WRITTEN_BLOCKS: &str = "temp_blks_written";
const READ_TIME: &str = "temp_blk_read_time";
const WRITE_TIME: &str = "temp_blk_write_time";
const BLOCK_BYTES: &str = "temp_block_bytes";

#[derive(Clone, Copy)]
pub(crate) enum Projection {
    Point,
    SingleIdentityGroup,
}

pub(crate) fn projection(columns: &BTreeSet<String>, projection: Projection) -> String {
    let mut fields = [READ_BLOCKS, WRITTEN_BLOCKS, READ_TIME, WRITE_TIME]
        .into_iter()
        .map(|column| {
            if !columns.contains(column) {
                return format!("NULL::text AS {column}");
            }
            match projection {
                Projection::Point => format!("s.{column}::text AS {column}"),
                // Existing scan query-id groups can combine users or top-level contexts. Preserve only a
                // single original native row; do not assign pooled work to a representative statement.
                Projection::SingleIdentityGroup => {
                    format!("CASE WHEN count(*)=1 THEN min({column})::text END AS {column}")
                }
            }
        })
        .collect::<Vec<_>>();
    fields.push(format!("pg_catalog.current_setting('block_size',true)::text AS {BLOCK_BYTES}"));
    fields.join(",")
}

fn read_work(row: &Row) -> Result<StatementTemporaryWork> {
    let block_bytes = row
        .unsigned_text(BLOCK_BYTES)?
        .map(|bytes| bytes.parse::<u32>().ok().filter(|bytes| *bytes > 0).ok_or_else(invalid))
        .transpose()?;
    let work = StatementTemporaryWork::BlockIo {
        source: SOURCE.into(),
        bytes_per_block: block_bytes,
        read_blocks: row.unsigned_text(READ_BLOCKS)?,
        written_blocks: row.unsigned_text(WRITTEN_BLOCKS)?,
        read_time_ms: milliseconds(row, READ_TIME)?,
        write_time_ms: milliseconds(row, WRITE_TIME)?,
    };
    if work.is_valid() { Ok(work) } else { Err(invalid()) }
}

pub(crate) fn temporary_work(row: &Row) -> Result<Option<StatementTemporaryWork>> {
    let work = read_work(row)?;
    Ok(work.has_measurements().then_some(work))
}

pub(super) fn counters(row: &Row) -> Result<Vec<StatementCounter>> {
    let StatementTemporaryWork::BlockIo {
        bytes_per_block,
        read_blocks,
        written_blocks,
        read_time_ms,
        write_time_ms,
        ..
    } = read_work(row)?
    else {
        return Err(invalid());
    };
    let mut counters = Vec::new();
    for (key, value) in [
        (RuntimeMetricKey::TemporaryBlocksRead, read_blocks),
        (RuntimeMetricKey::TemporaryBlocksWritten, written_blocks),
    ] {
        if let Some(value) = value {
            counters.push(StatementCounter {
                key,
                unit: RuntimeMetricUnit::Pages { bytes_per_page: bytes_per_block.ok_or_else(invalid)? },
                total: MeasurementValue::Integer(value),
            });
        }
    }
    for (key, value) in
        [(RuntimeMetricKey::TemporaryReadTime, read_time_ms), (RuntimeMetricKey::TemporaryWriteTime, write_time_ms)]
    {
        if let Some(value) = value {
            counters.push(StatementCounter {
                key,
                unit: RuntimeMetricUnit::Milliseconds,
                total: MeasurementValue::Real(value),
            });
        }
    }
    Ok(counters)
}

fn milliseconds(row: &Row, column: &str) -> Result<Option<f64>> {
    match row.value(column).ok_or_else(invalid)? {
        Some(value) => {
            value.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).map(Some).ok_or_else(invalid)
        }
        None => Ok(None),
    }
}
