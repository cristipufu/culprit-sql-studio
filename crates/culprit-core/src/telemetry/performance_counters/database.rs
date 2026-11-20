//! The selected population binds one original native database identity, never a database-looking counter label.
use super::*;
use crate::{MeasurementScope, ResetMarker, SourceState};
use std::collections::BTreeSet;

pub const DATABASE_PERFORMANCE_COUNTER_PROFILE: &str = "sqlserver-database-performance-counters-v1";
pub const DATABASE_PERFORMANCE_NAME_PROPERTY: &str = "native_database_name";
/// Conservative admission below the published nchar(128) counter-instance capacity, not a truncation algorithm.
pub const DATABASE_PERFORMANCE_NAME_UNITS: usize = 128;

pub fn database_performance_name_is_admissible(name: &str) -> bool {
    !name.is_empty()
        && name.encode_utf16().count() < DATABASE_PERFORMANCE_NAME_UNITS
        && name.trim() == name
        && !name.eq_ignore_ascii_case("_Total")
}
pub fn database_performance_candidate(sample: &NativeCounterSet) -> bool {
    sample
        .properties
        .get(PERFORMANCE_COUNTER_PROFILE_PROPERTY)
        .is_some_and(|value| value == DATABASE_PERFORMANCE_COUNTER_PROFILE)
}

pub fn validate_database_performance_identity(identity: &NativeIdentity) -> bool {
    identity.is_valid()
        && identity.id.parse::<u32>().is_ok_and(|id| id > 0 && id <= i32::MAX as u32 && id.to_string() == identity.id)
        && identity.qualifiers.get("kind").map(String::as_str) == Some("sql-server-local-database-recovery-epoch")
        && identity
            .qualifiers
            .get("database_created_without_offset")
            .is_some_and(|at| at.len() <= 27 && super::super::catalog::local_time(at))
        && [
            "database_created_without_offset",
            "database_file_guid",
            "database_guid",
            "recovery_fork_guid",
            "instance_epoch",
        ]
        .iter()
        .all(|name| identity.qualifiers.contains_key(*name))
}

pub(super) fn validate_database_performance_sample(
    profile: &PerformanceCounterProfile,
    sample: &NativeCounterSet,
) -> Result<()> {
    if !sample.is_valid()
        || sample.scope != MeasurementScope::Database
        || !database_performance_candidate(sample)
        || !validate_database_performance_identity(&sample.identity)
        || sample.counters.is_empty()
        || sample.counters.len() > profile.database_field_ids.len()
        || !sample.ratios.is_empty()
        || !sample.ratio_gaps.is_empty()
        || sample.properties.get("catalog_name_matches").map(String::as_str) != Some("1")
        || sample.properties.get("catalog_visibility").map(String::as_str) != Some("complete")
    {
        return Err(invalid());
    }
    let name = sample.properties.get(DATABASE_PERFORMANCE_NAME_PROPERTY).ok_or_else(invalid)?;
    if !database_performance_name_is_admissible(name)
        || profile.observed_at(sample)?.is_none()
        || profile.native_rows(sample)? != Some(sample.counters.len() as u32)
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for row in &sample.counters {
        let field = profile
            .field_in_sample(sample, &row.native_object, &row.native_name, row.native_instance.as_deref())
            .ok_or_else(invalid)?;
        let raw = row.native_type.as_deref().ok_or_else(invalid)?;
        let code = raw.parse::<i32>().ok().filter(|value| value.to_string() == raw).ok_or_else(invalid)?;
        if !fields.insert(field.id)
            || !keys.insert((&row.native_object, &row.native_name, &row.native_instance))
            || row.reset != ResetMarker::Unknown
            || row.value.integer().is_none_or(|value| i64::try_from(value).is_err())
            || (row.nature, row.unit) != field.meaning(code)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn database_performance_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::DatabasePerformanceCounters
        && snapshot.records <= 1
        && matches!(snapshot.sources.as_slice(),[source] if source.source==crate::source_id::PERFORMANCE_COUNTERS && source.records==snapshot.records && source.state!=SourceState::Empty)
}

pub fn database_performance_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::DatabasePerformanceCounters { snapshot, native_database, database_writable } =
        &source.observation
    else {
        return Err(invalid());
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if !database_performance_header_is_valid(snapshot)
        || !context.has_qualified_local_owner()
        || !*database_writable
        || !matches!(&context.partition,SourcePartition::InstanceLocal{member:Some(member),epoch:Some(epoch)} if member.kind==SharedResourceKind::Instance && native_database.key().ok().as_ref()==Some(epoch))
        || !validate_database_performance_identity(native_database)
        || source.target.database().is_none_or(|database| !database_performance_name_is_admissible(&database.database))
        || context
            .target
            .engine()
            .spec()
            .performance_counters()
            .is_none_or(|profile| profile.database_field_ids.is_empty())
    {
        return Err(invalid());
    }
    Ok(context)
}

pub fn database_performance_sample_fits_source(sample: &NativeCounterSet, source: &CollectionDescription) -> bool {
    let Ok(context) = database_performance_context(source) else { return false };
    let SourceObservation::DatabasePerformanceCounters { native_database, .. } = &source.observation else {
        return false;
    };
    let Some(profile) = context.target.engine().spec().performance_counters() else { return false };
    profile.validate(sample).is_ok()
        && sample.identity == **native_database
        && source.observation.snapshot().is_some_and(|snapshot| {
            !snapshot.sources[0].complete_scope || sample.counters.len() == profile.database_field_ids.len()
        })
        && source.target.database().is_some_and(|database| {
            sample.properties.get(DATABASE_PERFORMANCE_NAME_PROPERTY) == Some(&database.database)
        })
        && profile.observed_at(sample).ok().flatten().is_some_and(|at| {
            ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(at))
        })
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
