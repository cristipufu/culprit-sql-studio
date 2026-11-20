//! Tempdb XTP consumers are original shared-instance gauges, never application database or query allocations.
use super::*;
use std::collections::BTreeMap;

pub const MSSQL_TEMPDB_METADATA_CONSUMERS: &str = "tempdb.sys.dm_db_xtp_memory_consumers";
pub const MSSQL_TEMPDB_METADATA_CONSUMER_KIND: &str = "sql-server-tempdb-metadata-memory-consumer";
pub const MSSQL_TEMPDB_METADATA_FEATURE_KIND: &str = "sql-server-tempdb-metadata-memory-feature";
pub const MSSQL_TEMPDB_METADATA_MIN_MAJOR: u32 = 16;
pub const MAX_MSSQL_TEMPDB_METADATA_CONSUMERS: usize = 64;
pub const MSSQL_TEMPDB_METADATA_EDITIONS: &[i64] = &[2, 3, 4];

pub fn tempdb_metadata_memory_source_is_valid(source: &StateSourceCoverage) -> bool {
    source.source == crate::source_id::TEMPDB_METADATA_MEMORY
        && source.is_valid()
        && source.records as usize <= MAX_MSSQL_TEMPDB_METADATA_CONSUMERS + 1
}

pub fn tempdb_metadata_memory_owner_is_qualified(context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && matches!((&context.target, &context.partition),
            (CollectionTarget::Shared(owner), SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) })
            if owner == member && owner.engine == crate::Engine::Mssql && owner.kind == SharedResourceKind::Instance)
}

/// The native feature point is independent from the later consumer point and from source/body completeness.
pub struct TempdbMetadataFeature<'a> {
    sample: &'a NativeCounterSet,
    observed_at: &'a str,
    enabled: Option<bool>,
}
impl<'a> TempdbMetadataFeature<'a> {
    pub fn from_original(sample: &'a NativeCounterSet, context: &NativeReadContext) -> Result<Self> {
        let observed_at = header(sample, context, MSSQL_TEMPDB_METADATA_FEATURE_KIND, "SERVERPROPERTY", 5)?;
        if sample.identity.id != "feature"
            || !sample.counters.is_empty()
            || sample.identity.qualifiers.get("native_property").map(String::as_str)
                != Some("IsTempDbMetadataMemoryOptimized")
        {
            return Err(invalid());
        }
        let enabled = match sample.properties.get("metadata_enabled").map(String::as_str) {
            Some("0") if !sample.properties.contains_key("metadata_enabled_state") => Some(false),
            Some("1") if !sample.properties.contains_key("metadata_enabled_state") => Some(true),
            None if sample.properties.get("metadata_enabled_state").map(String::as_str) == Some("native-null") => None,
            _ => return Err(invalid()),
        };
        Ok(Self { sample, observed_at, enabled })
    }
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }
    pub fn observed_at(&self) -> &'a str {
        self.observed_at
    }
    pub fn database_created(&self) -> &'a str {
        &self.sample.identity.qualifiers["database_created"]
    }
}

/// Borrowed interpretation of one retained counter set; it never manufactures a capture or row receipt.
pub struct TempdbMetadataConsumer<'a> {
    sample: &'a NativeCounterSet,
    observed_at: &'a str,
    native_type: i32,
    allocated: Option<&'a str>,
    used: Option<&'a str>,
}
impl<'a> TempdbMetadataConsumer<'a> {
    pub fn from_original(sample: &'a NativeCounterSet, context: &NativeReadContext) -> Result<Self> {
        let observed_at =
            header(sample, context, MSSQL_TEMPDB_METADATA_CONSUMER_KIND, MSSQL_TEMPDB_METADATA_CONSUMERS, 4)?;
        let consumer_id = sample.identity.id.parse::<i64>().map_err(|_| invalid())?;
        let native_type = property(sample, "memory_consumer_type")?.parse::<i32>().map_err(|_| invalid())?;
        if consumer_id.to_string() != sample.identity.id
            || property(sample, "metadata_enabled")? != "1"
            || sample.properties.contains_key("metadata_enabled_state")
            || native_type <= 0
            || native_type.to_string() != property(sample, "memory_consumer_type")?
        {
            return Err(invalid());
        }
        let description = property(sample, "memory_consumer_type_desc")?;
        let expected = match native_type {
            2 => Some("VARHEAP"),
            3 => Some("HASH"),
            4 => Some("PGPOOL"),
            _ => None,
        };
        if description.encode_utf16().count() > 64
            || expected.is_some_and(|expected| expected != description)
            || sample.properties.get("memory_consumer_desc").is_some_and(|value| value.encode_utf16().count() > 64)
        {
            return Err(invalid());
        }
        let mut counters = BTreeMap::new();
        for counter in &sample.counters {
            let MeasurementValue::Integer(value) = &counter.value else { return Err(invalid()) };
            if !matches!(counter.key.as_str(), "allocated_bytes" | "used_bytes")
                || counter.native_name != counter.key
                || counter.native_object != MSSQL_TEMPDB_METADATA_CONSUMERS
                || counter.unit != CounterUnit::Bytes
                || counter.nature != CounterNature::Gauge
                || counter.scope != crate::MeasurementScope::Server
                || counter.reset != crate::ResetMarker::Unknown
                || counter.native_instance.is_some()
                || counter.native_type.is_some()
                || !value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == *value)
                || counters.insert(counter.key.as_str(), value.as_str()).is_some()
            {
                return Err(invalid());
            }
        }
        let used = counters.get("used_bytes").copied();
        let valid_usage = match property(sample, "used_bytes_state")? {
            "measured" => native_type == 2 && used.is_some(),
            "native-null" => native_type == 2 && used.is_none(),
            "not-applicable" => native_type != 2 && used.is_none(),
            _ => false,
        };
        if !valid_usage
            || sample.properties.get("native_used_bytes").is_some_and(|value| {
                native_type == 2
                    || !value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == *value)
            })
        {
            return Err(invalid());
        }
        Ok(Self { sample, observed_at, native_type, allocated: counters.get("allocated_bytes").copied(), used })
    }
    pub fn sample(&self) -> &'a NativeCounterSet {
        self.sample
    }
    pub fn observed_at(&self) -> &'a str {
        self.observed_at
    }
    pub fn database_created(&self) -> &'a str {
        &self.sample.identity.qualifiers["database_created"]
    }
    pub fn native_type(&self) -> i32 {
        self.native_type
    }
    pub fn allocated_bytes(&self) -> Option<&'a str> {
        self.allocated
    }
    pub fn used_bytes(&self) -> Option<&'a str> {
        self.used
    }
    pub fn usage_is_applicable(&self) -> bool {
        self.native_type == 2
    }
}

fn header<'a>(
    sample: &'a NativeCounterSet,
    context: &NativeReadContext,
    kind: &str,
    object: &str,
    qualifiers: usize,
) -> Result<&'a str> {
    let identity = &sample.identity;
    let major = property(sample, "native_major")?.parse::<u32>().map_err(|_| invalid())?;
    let edition = property(sample, "native_edition")?.parse::<i64>().map_err(|_| invalid())?;
    let observed_at = property(sample, "observed_at")?;
    if !sample.is_valid()
        || !tempdb_metadata_memory_owner_is_qualified(context)
        || sample.scope != crate::MeasurementScope::Server
        || identity.qualifiers.get("kind").map(String::as_str) != Some(kind)
        || identity.qualifiers.get("native_object").map(String::as_str) != Some(object)
        || identity.qualifiers.get("database_id").map(String::as_str) != Some("2")
        || !identity.qualifiers.get("database_created").is_some_and(|created| super::catalog::local_time(created))
        || identity.qualifiers.len() != qualifiers
        || major < MSSQL_TEMPDB_METADATA_MIN_MAJOR
        || !MSSQL_TEMPDB_METADATA_EDITIONS.contains(&edition)
        || major.to_string() != property(sample, "native_major")?
        || edition.to_string() != property(sample, "native_edition")?
        || !valid_utc_instant(observed_at)
        || !ObservationBounds::new(&context.observed_from, &context.observed_until)
            .is_some_and(|bounds| bounds.contains(observed_at))
        || !sample.ratios.is_empty()
        || !sample.ratio_gaps.is_empty()
    {
        return Err(invalid());
    }
    Ok(observed_at)
}
fn property<'a>(sample: &'a NativeCounterSet, name: &str) -> Result<&'a str> {
    sample.properties.get(name).map(String::as_str).ok_or_else(invalid)
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
