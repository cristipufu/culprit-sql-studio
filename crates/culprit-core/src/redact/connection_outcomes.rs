//! Sharing keeps native counter schemas and meanings while removing physical source identifiers.
use super::{opaque, redact_read_context, redact_target};
use crate::{
    ConnectionOutcomeObservation, ConnectionOutcomeSnapshot, DatabaseIdentity, ResetMarker, Result, telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

pub(super) fn redact(mut read: Box<ConnectionOutcomeSnapshot>) -> Result<Box<ConnectionOutcomeSnapshot>> {
    if !read.is_valid_read(false) {
        return Err(invalid());
    }
    let known_sources = read
        .observations
        .iter()
        .filter(|row| read.profile(row).is_some())
        .map(|row| row.source.clone())
        .collect::<BTreeSet<_>>();
    for index in 0..read.observations.len() {
        let profile = read.profile(&read.observations[index]);
        let known_schema = profile.is_some();
        let native_fields = profile
            .map(|profile| profile.metrics.into_iter().map(|metric| metric.native_key).collect::<Vec<_>>())
            .unwrap_or_default();
        redact_row(&mut read.observations[index], known_schema, &native_fields, read.target.database())?;
    }
    for row in &mut read.observations {
        redact_source(&mut row.source, &known_sources);
    }
    for source in &mut read.sources {
        redact_source(&mut source.source, &known_sources);
    }
    redact_target(&mut read.target);
    if let ObservationReadContext::Native { context } = &mut read.context {
        redact_read_context(context);
    }
    read.is_valid_read(false).then_some(read).ok_or_else(invalid)
}

fn redact_row(
    row: &mut ConnectionOutcomeObservation,
    known_schema: bool,
    native_fields: &[&str],
    database: Option<&DatabaseIdentity>,
) -> Result<()> {
    if !known_schema {
        row.counters.identity.id = opaque(&row.counters.identity.id);
    }
    // Profiles interpret the counter schema, never arbitrary native qualifier labels or values.
    redact_pairs(&mut row.counters.identity.qualifiers, |_, _| false)?;
    redact_pairs(&mut row.counters.properties, |key, value| {
        known_schema && public_property(key, value, native_fields, database)
    })?;
    for counter in &mut row.counters.counters {
        if !native_fields.contains(&counter.key.as_str()) {
            counter.key = opaque(&counter.key);
        }
        if !known_schema || counter.native_object != row.source {
            counter.native_object = opaque(&counter.native_object);
        }
        if !native_fields.contains(&counter.native_name.as_str()) {
            counter.native_name = opaque(&counter.native_name);
        }
        if let Some(value) = &mut counter.native_instance {
            *value = opaque(value);
        }
        if let Some(value) = &mut counter.native_type
            && !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            *value = opaque(value);
        }
        if let ResetMarker::Generation(value) = &mut counter.reset {
            *value = opaque(value);
        }
    }
    Ok(())
}

fn public_property(key: &str, value: &str, native_fields: &[&str], database: Option<&DatabaseIdentity>) -> bool {
    match key {
        "datname" => database.is_some_and(|database| value == database.database.as_str()),
        "server_version_num" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "counter_observed_at" => canonical_time(value).is_ok(),
        key if key.strip_prefix("has_").is_some_and(|name| native_fields.contains(&name)) => {
            matches!(value, "true" | "false")
        }
        _ => false,
    }
}

fn redact_pairs(values: &mut BTreeMap<String, String>, public: impl Fn(&str, &str) -> bool) -> Result<()> {
    let mut redacted = BTreeMap::new();
    for (key, value) in std::mem::take(values) {
        let (key, value) = if public(&key, &value) { (key, value) } else { (opaque(&key), opaque(&value)) };
        match redacted.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(_) => return Err(invalid()),
        }
    }
    *values = redacted;
    Ok(())
}

fn redact_source(source: &mut String, known_sources: &BTreeSet<String>) {
    if !known_sources.contains(source.as_str()) {
        *source = opaque(source);
    }
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
