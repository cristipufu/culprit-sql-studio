//! Project declared native counter meanings without inventing intervals, byte amounts or provider identities.
use super::*;
use crate::StatementTemporaryWork;

/// Project a single already-qualified native runtime population. The caller owns source, interval, definition
/// and epoch membership; this function neither joins captures nor differences cumulative counters.
///
/// Every contributing fragment must declare page-based temporary-space averages. Missing fragments, byte-only
/// representations and incompatible page sizes withhold the projection rather than turn a partial sum into a
/// complete one. Native zero averages remain measured zero; zero executions establish no space observation.
pub fn runtime_temporary_space<'a>(
    rows: impl IntoIterator<Item = &'a RuntimeMeasurement>,
    source: &str,
) -> Option<StatementTemporaryWork> {
    const MAX_EXACT_EXECUTIONS: u64 = 1 << 53;
    if !valid_key(source) || source.trim().is_empty() {
        return None;
    }
    let mut executions = 0u64;
    let mut page_bytes = None;
    let mut usage_pages_total = 0.0;
    for row in rows {
        if !row.is_valid() {
            return None;
        }
        let n = row.executions.parse::<u64>().ok()?;
        executions = executions.checked_add(n).filter(|count| *count <= MAX_EXACT_EXECUTIONS)?;
        if n == 0 {
            continue;
        }
        let metric = row.metric(&RuntimeMetricKey::TemporaryStorage)?;
        let RuntimeMetricUnit::Pages { bytes_per_page } = metric.unit else { return None };
        if page_bytes.is_some_and(|previous| previous != bytes_per_page) {
            return None;
        }
        page_bytes = Some(bytes_per_page);
        usage_pages_total += metric.average * n as f64;
        if !usage_pages_total.is_finite() {
            return None;
        }
    }
    let work =
        StatementTemporaryWork::SpaceUsage { source: source.into(), bytes_per_page: page_bytes?, usage_pages_total };
    work.is_valid().then_some(work)
}

/// An observation may record both block I/O and table creation. Native names have no analytical meaning here.
pub fn statement_temporary_work(sample: &StatementCounterSample, source: &str) -> Result<Vec<StatementTemporaryWork>> {
    if !sample.is_valid() || !valid_key(source) || source.trim().is_empty() {
        return Err(invalid());
    }
    let mut blocks = BlockWork::default();
    let mut tables = TableWork::default();
    for metric in &sample.totals {
        match metric.key {
            RuntimeMetricKey::TemporaryBlocksRead => {
                blocks.set_page_size(metric.unit)?;
                blocks.read = Some(count(metric)?);
            }
            RuntimeMetricKey::TemporaryBlocksWritten => {
                blocks.set_page_size(metric.unit)?;
                blocks.written = Some(count(metric)?);
            }
            RuntimeMetricKey::TemporaryReadTime => blocks.read_ms = Some(milliseconds(metric)?),
            RuntimeMetricKey::TemporaryWriteTime => blocks.write_ms = Some(milliseconds(metric)?),
            RuntimeMetricKey::TemporaryTables => tables.created = Some(count(metric)?),
            RuntimeMetricKey::TemporaryDiskTables => tables.disk = Some(count(metric)?),
            RuntimeMetricKey::SortMergePasses => tables.passes = Some(count(metric)?),
            _ => {}
        }
    }
    let mut work = Vec::new();
    if blocks.read.is_some() || blocks.written.is_some() || blocks.read_ms.is_some() || blocks.write_ms.is_some() {
        work.push(StatementTemporaryWork::BlockIo {
            source: source.into(),
            bytes_per_block: blocks.page_bytes,
            read_blocks: blocks.read,
            written_blocks: blocks.written,
            read_time_ms: blocks.read_ms,
            write_time_ms: blocks.write_ms,
        });
    }
    if tables.created.is_some() || tables.disk.is_some() || tables.passes.is_some() {
        work.push(StatementTemporaryWork::TableCreation {
            source: source.into(),
            created_tables: tables.created,
            disk_tables: tables.disk,
            sort_merge_passes: tables.passes,
        });
    }
    if work.iter().all(StatementTemporaryWork::is_valid) { Ok(work) } else { Err(invalid()) }
}

#[derive(Default)]
struct BlockWork {
    page_bytes: Option<u32>,
    read: Option<String>,
    written: Option<String>,
    read_ms: Option<f64>,
    write_ms: Option<f64>,
}
impl BlockWork {
    fn set_page_size(&mut self, unit: RuntimeMetricUnit) -> Result<()> {
        let RuntimeMetricUnit::Pages { bytes_per_page } = unit else { return Err(invalid()) };
        if self.page_bytes.is_some_and(|previous| previous != bytes_per_page) {
            return Err(invalid());
        }
        self.page_bytes = Some(bytes_per_page);
        Ok(())
    }
}

#[derive(Default)]
struct TableWork {
    created: Option<String>,
    disk: Option<String>,
    passes: Option<String>,
}

fn count(metric: &StatementCounter) -> Result<String> {
    let MeasurementValue::Integer(value) = &metric.total else { return Err(invalid()) };
    Ok(value.clone())
}

fn milliseconds(metric: &StatementCounter) -> Result<f64> {
    let value = match &metric.total {
        MeasurementValue::Integer(value) => value.parse::<u64>().map_err(|_| invalid())? as f64,
        MeasurementValue::Real(value) => *value,
    };
    let milliseconds = match metric.unit {
        RuntimeMetricUnit::Picoseconds => value / 1_000_000_000.0,
        RuntimeMetricUnit::Nanoseconds => value / 1_000_000.0,
        RuntimeMetricUnit::Microseconds => value / 1_000.0,
        RuntimeMetricUnit::Milliseconds => value,
        RuntimeMetricUnit::Seconds => value * 1_000.0,
        _ => return Err(invalid()),
    };
    if milliseconds.is_finite() { Ok(milliseconds) } else { Err(invalid()) }
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
#[path = "temporary_work/runtime_tests.rs"]
mod runtime_tests;
#[cfg(test)]
mod tests;
