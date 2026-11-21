//! Native MI intervals belong to the service history, independently of the reader's current process.
use super::super::source_read::same_time;
use super::*;

pub const MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE: &str = "managed-instance-resource-history";
pub const MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS: u32 = 512;
pub const MSSQL_MANAGED_INSTANCE_RESOURCE_PERIOD_MS: u32 = 15_000;
pub const MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_WINDOW_SECONDS: i64 = 2 * 60 * 60;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlManagedInstanceResourceLayout {
    pub interval: MssqlCatalogAvailability,
    pub cpu: MssqlCatalogAvailability,
    pub virtual_core_count: MssqlCatalogAvailability,
    pub reserved_storage_mb: MssqlCatalogAvailability,
    pub storage_space_used_mb: MssqlCatalogAvailability,
    pub io_request: MssqlCatalogAvailability,
    pub io_bytes_read: MssqlCatalogAvailability,
    pub io_bytes_written: MssqlCatalogAvailability,
    pub resource_type: MssqlCatalogAvailability,
    pub resource_name: MssqlCatalogAvailability,
    pub sku: MssqlCatalogAvailability,
    pub hardware_generation: MssqlCatalogAvailability,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlManagedInstanceResourceSample {
    pub started_at: String,
    pub ended_at: String,
    pub avg_cpu_percent: Option<String>,
    pub virtual_core_count: Option<String>,
    pub reserved_storage_mb: Option<String>,
    pub storage_space_used_mb: Option<String>,
    /// These three native values are totals within started_at..ended_at, never running counters.
    pub io_request: Option<String>,
    pub io_bytes_read: Option<String>,
    pub io_bytes_written: Option<String>,
    pub resource_type: Option<String>,
    pub resource_name: Option<String>,
    pub sku: Option<String>,
    pub hardware_generation: Option<String>,
}
impl MssqlManagedInstanceResourceSample {
    pub fn is_valid(&self) -> bool {
        // Native start/end are retained independently. The view documents nominal 15-second reporting;
        // comparing the two different native datetime precisions must not invent an exact-duration proof.
        parsed_interval(&self.started_at, &self.ended_at).is_some()
            && [&self.started_at, &self.ended_at]
                .into_iter()
                .all(|at| canonical_time(at).is_ok_and(|canonical| canonical == *at))
            && self
                .virtual_core_count
                .as_deref()
                .is_none_or(|value| value.parse::<i32>().is_ok_and(|number| number >= 0 && number.to_string() == value))
            && self.avg_cpu_percent.as_deref().is_none_or(|value| decimal(value, 5, 2))
            && self.storage_space_used_mb.as_deref().is_none_or(|value| decimal(value, 18, 2))
            && [
                &self.virtual_core_count,
                &self.reserved_storage_mb,
                &self.io_request,
                &self.io_bytes_read,
                &self.io_bytes_written,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(integer))
            && [&self.resource_type, &self.resource_name, &self.sku, &self.hardware_generation]
                .into_iter()
                .all(|value| value.as_ref().is_none_or(|value| value.encode_utf16().count() <= 128))
    }
    pub fn matches_measurement(&self, sample: &ResourceMeasurement) -> bool {
        self.is_valid()
            && sample.at == self.ended_at
            && sample.replica_id.is_none()
            && sample.replica_role.is_none()
            && sample.data_io_accounting == DataIoAccounting::Unknown
            && sample.metrics.len() == 1
            && sample.metrics[0].key == "cpu"
            && sample.metrics[0].scope == crate::MeasurementScope::Server
            && sample.metrics[0].unit == ResourceUnit::Percent
            && sample.metrics[0].statistic == ResourceStatistic::Mean
            && sample.metrics[0].period_ms == Some(MSSQL_MANAGED_INSTANCE_RESOURCE_PERIOD_MS)
            && sample.metrics[0].limit_basis == ResourceLimitBasis::InstanceLimit
            && sample.metrics[0].value == self.avg_cpu_percent.as_deref().and_then(|value| value.parse::<f64>().ok())
    }
    pub fn fits_layout(&self, layout: &MssqlManagedInstanceResourceLayout) -> bool {
        use MssqlCatalogAvailability::Available;
        layout.interval == Available
            && [
                (&self.avg_cpu_percent, layout.cpu),
                (&self.virtual_core_count, layout.virtual_core_count),
                (&self.reserved_storage_mb, layout.reserved_storage_mb),
                (&self.storage_space_used_mb, layout.storage_space_used_mb),
                (&self.io_request, layout.io_request),
                (&self.io_bytes_read, layout.io_bytes_read),
                (&self.io_bytes_written, layout.io_bytes_written),
                (&self.resource_type, layout.resource_type),
                (&self.resource_name, layout.resource_name),
                (&self.sku, layout.sku),
                (&self.hardware_generation, layout.hardware_generation),
            ]
            .into_iter()
            .all(|(value, status)| status == Available || value.is_none())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlManagedInstanceResourceHistory {
    pub from: String,
    pub until: String,
    pub native_engine_edition: i64,
    /// Original reader UTC, not the latest historical interval or proof that publication has caught up.
    pub observed_at: String,
    /// A current reader witness only; no historical sample inherits its member, role or process epoch.
    pub current_reader: Option<Box<NativeReadContext>>,
    pub coverage: StateSourceCoverage,
    /// Original native prefix, including the single lookahead when the selected row cap was reached.
    pub native_rows: u32,
    pub layout: MssqlManagedInstanceResourceLayout,
}
impl MssqlManagedInstanceResourceHistory {
    pub fn is_valid(&self) -> bool {
        let Some((from, until)) = parsed_interval(&self.from, &self.until) else { return false };
        let known = self.current_reader.as_ref().is_some_and(|context| {
            context.has_qualified_local_owner()
                && context.database_writable.is_none()
                && matches!((&context.target, &context.partition),
                    (CollectionTarget::Shared(owner),SourcePartition::InstanceLocal {member:Some(member),epoch:Some(_)} )
                    if owner == member && owner.engine == crate::Engine::Mssql && owner.kind == SharedResourceKind::Instance)
                && same_time(&context.observed_from, &self.observed_at)
                && same_time(&context.observed_until, &self.observed_at)
        });
        self.native_engine_edition > 0
            && self.current_reader.as_ref().is_none_or(|_| known)
            && until - from <= chrono::Duration::seconds(MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_WINDOW_SECONDS)
            && canonical_time(&self.observed_at).is_ok()
            && (same_time(&self.until, &self.observed_at) || parsed_interval(&self.until, &self.observed_at).is_some())
            && self.coverage.is_valid()
            && self.coverage.source == MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE
            && self.coverage.records <= MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS
            && self.native_rows <= MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS + 1
            && (self.coverage.records == 0
                || known
                    && self.native_engine_edition == 8
                    && self.layout.interval == MssqlCatalogAvailability::Available)
            && (known
                || matches!(self.coverage.state, crate::SourceState::Unavailable | crate::SourceState::NotApplicable))
            && self.coverage.state != crate::SourceState::Empty
            && if self.coverage.stopped == Some(CollectionStop::RowLimit) {
                self.coverage.state == crate::SourceState::Partial
                    && !self.coverage.complete_scope
                    && self.native_rows == self.coverage.records + 1
            } else {
                self.native_rows == self.coverage.records
            }
    }
    pub fn matches_source(&self, source: &CollectionDescription) -> bool {
        self.is_valid()
            && source.source == MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE
            && source.partition == SourcePartition::Target
            && matches!(&source.target, CollectionTarget::Shared(owner) if owner.engine == crate::Engine::Mssql && owner.kind == SharedResourceKind::Instance)
            && self.current_reader.as_ref().is_none_or(|context| context.target == source.target)
    }
    pub fn accepts(&self, sample: &ResourceMeasurement) -> bool {
        self.is_valid()
            && self.current_reader.is_some()
            && matches!(self.coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
            && sample
                .mssql_managed_instance
                .as_ref()
                .is_some_and(|native| native.matches_measurement(sample) && native.fits_layout(&self.layout))
            && match parsed_interval(&self.from, &self.until) {
                Some((from, until)) => {
                    DateTime::parse_from_rfc3339(&sample.at).is_ok_and(|at| from <= at && at < until)
                }
                None => false,
            }
    }
}
fn parsed_interval(from: &str, until: &str) -> Option<(DateTime<chrono::FixedOffset>, DateTime<chrono::FixedOffset>)> {
    let from = DateTime::parse_from_rfc3339(from).ok()?;
    let until = DateTime::parse_from_rfc3339(until).ok()?;
    (from < until).then_some((from, until))
}
fn integer(value: &str) -> bool {
    value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == value)
}
fn decimal(value: &str, precision: usize, scale: usize) -> bool {
    let Some((whole, fraction)) = value.split_once('.') else { return false };
    !whole.is_empty()
        && (whole.len() == 1 || !whole.starts_with('0'))
        && whole.len() <= precision - scale
        && fraction.len() == scale
        && whole.bytes().chain(fraction.bytes()).all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests;

mod read;
pub use read::*;
