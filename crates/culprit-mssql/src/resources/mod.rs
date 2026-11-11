//! Provider-owned Azure SQL resource evidence, shared by snapshots and collection adapters.
mod inventory;
mod sample;
mod sql;

use crate::{
    conn::Pool,
    read::{NativeRead, Parameter, ReadError, ReadResult, ReadSession, invalid_data, text},
    value::Rec,
};
use chrono::{DateTime, Utc};
use culprit_core::{Result, telemetry::*};
pub(crate) use inventory::Inventory;
pub(crate) use sample::{Metric, Sample};
use std::sync::Arc;
pub(crate) const SAMPLE_PERIOD_MS: u32 = 15_000;
const MAX_RETAINED_SAMPLES: u32 = 10_000;
const SNAPSHOT_RESOURCE_TIMEOUT_MS: u32 = 10_000;

pub(crate) struct Window {
    pub from: DateTime<Utc>,
    pub until: DateTime<Utc>,
}
impl Window {
    pub(crate) fn new(from: DateTime<Utc>, until: DateTime<Utc>) -> Result<Self> {
        if from >= until {
            return Err(invalid_data());
        }
        Ok(Self { from, until })
    }
    fn contains(&self, at: &str) -> Result<bool> {
        let at = DateTime::parse_from_rfc3339(at).map_err(|_| invalid_data())?;
        Ok(at >= self.from && at < self.until)
    }
}
pub(crate) async fn inventory<R: NativeRead>(io: &mut R, window: &Window) -> Result<Inventory> {
    let rows = io
        .query(
            sql::INVENTORY,
            vec![Parameter::Text(window.from.to_rfc3339()), Parameter::Text(window.until.to_rfc3339())],
            1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() != 1 {
        return Err(invalid_data());
    }
    Inventory::decode(&rows[0], window)
}
pub(crate) async fn retained<R: NativeRead>(io: &mut R) -> ReadResult<Vec<Sample>> {
    let rows = io
        .query(
            sql::RETAINED,
            vec![Parameter::Integer(i64::from(MAX_RETAINED_SAMPLES) + 1)],
            MAX_RETAINED_SAMPLES as usize + 1,
        )
        .await?;
    if rows.len() > MAX_RETAINED_SAMPLES as usize {
        return Err(ReadError::Stopped(CollectionStop::RowLimit));
    }
    samples(rows, None)
}
pub(crate) async fn windowed<R: NativeRead>(
    io: &mut R,
    window: &Window,
    after: Option<&str>,
    limit: u32,
) -> ReadResult<Vec<Sample>> {
    if limit == 0 || limit > MAX_RETAINED_SAMPLES {
        return Err(invalid_data().into());
    }
    let after = after.map(canonical_time).transpose()?;
    if let Some(after) = &after
        && !window.contains(after)?
    {
        return Err(invalid_data().into());
    }
    let rows = io
        .query(
            sql::WINDOWED,
            vec![
                Parameter::Integer(i64::from(limit) + 1),
                Parameter::Text(window.from.to_rfc3339()),
                Parameter::Text(window.until.to_rfc3339()),
                Parameter::Text(after.clone().unwrap_or_default()),
            ],
            limit as usize + 1,
        )
        .await?;
    if rows.len() > limit as usize + 1 {
        return Err(invalid_data().into());
    }
    let samples = samples(rows, after.as_deref())?;
    for sample in &samples {
        if !window.contains(&sample.at)? {
            return Err(invalid_data().into());
        }
    }
    Ok(samples)
}
fn samples(rows: Vec<Rec>, after: Option<&str>) -> ReadResult<Vec<Sample>> {
    let samples = rows.iter().map(Sample::decode).collect::<Result<Vec<_>>>()?;
    let mut before = after;
    for sample in &samples {
        if before.is_some_and(|before| before >= sample.at.as_str()) {
            return Err(invalid_data().into());
        }
        before = Some(&sample.at);
    }
    Ok(samples)
}
pub(crate) struct Latest {
    pub sample: Sample,
    pub observed_at: DateTime<Utc>,
}
impl Latest {
    pub(crate) fn age_seconds(&self) -> Option<i64> {
        let at = DateTime::parse_from_rfc3339(&self.sample.at).ok()?.with_timezone(&Utc);
        (at <= self.observed_at).then(|| (self.observed_at - at).num_seconds())
    }
}
pub(crate) async fn latest<R: NativeRead>(io: &mut R) -> ReadResult<Option<Latest>> {
    let rows = io.query(sql::LATEST, vec![], 1).await?;
    if rows.len() > 1 {
        return Err(invalid_data().into());
    }
    rows.first()
        .map(|row| {
            Ok(Latest {
                sample: Sample::decode(row)?,
                observed_at: DateTime::parse_from_rfc3339(&text(row, "observed_at")?)
                    .map_err(|_| invalid_data())?
                    .with_timezone(&Utc),
            })
        })
        .transpose()
}
/// The snapshot owns this read until confirmed completion; successful connections return to the statistics pool.
pub(crate) async fn snapshot(pool: Arc<Pool>) -> Result<Vec<Sample>> {
    let budget = CollectionBudget {
        max_queries: 1,
        max_snapshot_rows: MAX_RETAINED_SAMPLES,
        total_timeout_ms: SNAPSHOT_RESOURCE_TIMEOUT_MS,
        ..CollectionBudget::default()
    };
    let mut io = ReadSession::new(pool, budget);
    snapshot_from(&mut io).await
}
async fn snapshot_from<R: NativeRead>(io: &mut R) -> Result<Vec<Sample>> {
    let result = retained(io).await.map_err(ReadError::into_error);
    if io.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}
#[derive(Default)]
pub(crate) struct Summary {
    pub data_io: Option<f64>,
    pub data_io_peak: Option<f64>,
    pub memory: Option<f64>,
}
impl Summary {
    pub(crate) fn from_samples(samples: &[Sample]) -> Self {
        let data_io: Vec<_> = samples.iter().filter_map(|sample| sample.value(Metric::DataIo)).collect();
        let memory: Vec<_> = samples.iter().filter_map(|sample| sample.value(Metric::Memory)).collect();
        fn mean(values: &[f64]) -> Option<f64> {
            (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
        }
        Self { data_io: mean(&data_io), data_io_peak: data_io.into_iter().reduce(f64::max), memory: mean(&memory) }
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod test_support;
