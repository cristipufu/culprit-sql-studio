//! Redact literal values before a plan, a comparison or a finding leaves the machine (a PR, a ticket, a chat). Strings
//! and numbers in SQL text and predicates become `'?'` and `?`; names (tables, columns, indexes) and parameter markers
//! (`@P1`, `$1`, `?`, `:name`) stay, so the text still explains the query. Each engine's quoting is followed: SQL
//! Server's `[names]` and `N'…'`, Postgres's `"names"`, `E'…'` and `$$…$$`, MySQL's `` `names` ``, `"…"` strings and
//! backslash escapes.

use crate::finding::Finding;
use crate::model::{Engine, Snapshot};

mod cached_runtime;
mod configured_log_rates;
mod connection_outcomes;
mod instance_resources;
mod jobs;
mod page_server_reads;
mod plan;
mod replication;
mod resource_counters;
mod resources;
mod sql;
mod statement_work;
#[cfg(test)]
mod tests;
mod wait_counters;

pub use plan::{redact_native, redact_parameter_context, redact_plan};
pub(crate) use sql::{dollar_tag, skip_quoted_smart};
pub use sql::{redact_prose, redact_sql, redact_strings, strip_database};

fn opaque(value: &str) -> String {
    use sha2::{Digest, Sha256};
    // Preserve our own tokens when a shared artifact is read again.
    if value.strip_prefix("redacted-").is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())) {
        value.into()
    } else {
        let digest: String = Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
        format!("redacted-{digest}")
    }
}

/// Physical paths are not public finding identifiers. Logical catalog names remain recognizable.
pub fn shareable_name(value: &str) -> String {
    if value.contains(['/', '\\']) { opaque(value) } else { value.into() }
}

/// Keep database labels and equality relationships, without retaining connection endpoints or file paths.
pub fn redact_identity(identity: &mut crate::DatabaseIdentity) {
    use crate::DatabaseLocation;
    match &mut identity.location {
        DatabaseLocation::Server { host, port, instance } => {
            if !host.trim().is_empty() {
                let canonical = host.trim().trim_end_matches('.').to_ascii_lowercase();
                *host = if canonical.starts_with("redacted-") && *port == 1 {
                    opaque(&canonical)
                } else {
                    opaque(&format!("{canonical}:{port}"))
                };
            }
            if *port != 0 {
                *port = 1;
            }
            *instance = instance.as_deref().map(opaque);
        }
        DatabaseLocation::Socket { directory, port } => {
            if !directory.is_empty() {
                *directory = if directory.starts_with("redacted-") && *port == 1 {
                    opaque(directory)
                } else {
                    opaque(&format!("{directory}:{port}"))
                };
            }
            if *port != 0 {
                *port = 1;
            }
        }
        DatabaseLocation::File { path } if !path.is_empty() => *path = opaque(path),
        _ => {}
    }
    identity.incarnation = identity.incarnation.as_deref().map(opaque);
}

/// Keep a shared resource's kind and equality relationships without publishing its physical native identity.
pub fn redact_resource_identity(identity: &mut crate::telemetry::SharedResourceIdentity) {
    for native in [&mut identity.namespace, &mut identity.native] {
        redact_native_identity(native);
    }
}

/// Keep equality and repeat-sharing stability without publishing a physical source's native identity.
fn redact_native_identity(identity: &mut crate::telemetry::NativeIdentity) {
    identity.id = opaque(&identity.id);
    redact_native_qualifiers(identity);
}
fn redact_native_qualifiers(identity: &mut crate::telemetry::NativeIdentity) {
    for value in identity.qualifiers.values_mut() {
        *value = opaque(value);
    }
}

fn redact_page_target(target: &mut crate::telemetry::MappedPageTarget) -> bool {
    use crate::telemetry::{PageTargetIdentityBasis, PageTargetScope};
    if !target.is_valid() {
        return false;
    }
    // Fixed system coordinates, native kinds and the creation clock are structural facts. Preserve them only
    // after validating this narrow basis; physical identifiers and the native process epoch remain opaque.
    let primary = target.identity_basis == PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch;
    if let PageTargetScope::SharedResource { context } = &mut target.scope {
        redact_read_context(context);
    }
    if primary {
        redact_qualifiers_except(&mut target.database, &["kind", "database_created_without_offset"]);
        redact_qualifiers_except(&mut target.file, &["kind", "file_type", "file_state"]);
    } else {
        redact_native_identity(&mut target.database);
        redact_native_identity(&mut target.file);
    }
    if let Some(object) = &mut target.object {
        redact_native_qualifiers(&mut object.native);
    }
    let Ok((database, file)) = target.scope.bind_identities(target.database.clone(), target.file.clone()) else {
        return false;
    };
    target.database = database;
    target.file = file;
    target.is_valid()
}

fn redact_qualifiers_except(identity: &mut crate::telemetry::NativeIdentity, structural: &[&str]) {
    for (key, value) in &mut identity.qualifiers {
        if !structural.contains(&key.as_str()) {
            *value = opaque(value);
        }
    }
}

/// Ordinary export retains native evidence. Only this explicit sharing boundary withdraws private context.
fn redact_activity_context_for_sharing(observation: &mut crate::telemetry::ActivityObservation) {
    observation.client = None;
    if let Some(crate::telemetry::SessionNativeContext::Mssql(native)) =
        observation.session_context.as_deref_mut().and_then(|context| context.native.as_deref_mut())
    {
        native.idle_connection = None;
    }
}

fn redact_target(target: &mut crate::telemetry::CollectionTarget) {
    match target {
        crate::telemetry::CollectionTarget::Database(identity) => redact_identity(identity),
        crate::telemetry::CollectionTarget::Shared(identity) => redact_resource_identity(identity),
    }
}

fn redact_partition(partition: &mut crate::telemetry::SourcePartition) {
    if let crate::telemetry::SourcePartition::InstanceLocal { member, epoch } = partition {
        if let Some(member) = member {
            redact_resource_identity(member);
        }
        *epoch = epoch.as_deref().map(opaque);
    }
}

fn redact_read_context(context: &mut crate::telemetry::NativeReadContext) {
    redact_target(&mut context.target);
    redact_partition(&mut context.partition);
}

pub fn redact_measurement(measurement: &mut crate::MeasurementContext) {
    redact_identity(&mut measurement.database);
    if let Some(population) = &mut measurement.population {
        redact_native_identity(population);
    }
    if let crate::MeasurementSource::Named(source) = &mut measurement.source {
        *source = redact_scope(source);
    }
}

// Native scope labels retain node/role visibility without publishing physical UUIDs or catalog role ids.
fn redact_scope(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, label)) = ["node ", "role "]
        .into_iter()
        .filter_map(|label| rest.find(label).map(|at| (at, label)))
        .min_by_key(|(at, _)| *at)
    {
        out.push_str(&rest[..at + label.len()]);
        let boundary = rest[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_');
        rest = &rest[at + label.len()..];
        let end = rest
            .bytes()
            .take_while(|b| if label == "node " { b.is_ascii_hexdigit() || *b == b'-' } else { b.is_ascii_digit() })
            .count();
        let value = &rest[..end];
        let valid = boundary && if label == "node " { value.replace('-', "").len() == 32 } else { !value.is_empty() };
        if valid {
            out.push_str(&if label == "node " { opaque(value) } else { "[redacted]".into() });
        } else {
            out.push_str(value);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

// Retained observation labels carry private scope identifiers separately from translated words.
fn redact_scoped_message(message: &mut crate::i18n::Message) {
    use crate::i18n::{Message, MessageValue};
    match message {
        Message::Text { text } => *text = redact_scope(text),
        Message::Sequence { messages, .. } => {
            for message in messages {
                redact_scoped_message(message);
            }
        }
        Message::Catalog { arguments, .. } => {
            for (name, value) in arguments {
                match value {
                    MessageValue::Text(text) if name == "node" && !text.is_empty() => *text = opaque(text),
                    MessageValue::Text(text) if name == "role" && !text.is_empty() => *text = "[redacted]".into(),
                    MessageValue::Message(message) => redact_scoped_message(message),
                    _ => {}
                }
            }
        }
    }
}

fn redact_reference(reference: &mut crate::ScanReference) {
    reference.id = reference.id.as_deref().map(opaque);
    if let Some(database) = &mut reference.database {
        redact_identity(database);
    }
}

pub fn redact_baseline(reference: &mut crate::BaselineReference) {
    redact_reference(&mut reference.scan);
    if let Some(measurement) = &mut reference.measurement {
        redact_measurement(measurement);
    }
}

/// Only fixed, argument-free evidence rejection messages may survive sharing unchanged.
fn public_evidence_issue(source: &str, message: Option<&crate::i18n::Message>) -> bool {
    let Some(crate::i18n::Message::Catalog { key, arguments }) = message else { return false };
    arguments.is_empty()
        && matches!(
            (source, key.as_str()),
            (crate::source_id::SESSIONS, "view.connection_outcomes.invalid")
                | (crate::source_id::RESOURCES, "collection.invalid_source")
                | (crate::telemetry::CONFIGURED_LOG_RATES_SOURCE, "view.configured_log_rates.invalid")
                | (crate::telemetry::HYPERSCALE_LOG_COMPONENTS_SOURCE, "view.hyperscale_log_components.invalid")
                | (
                    crate::telemetry::HYPERSCALE_LOG_COMPONENTS_SOURCE,
                    "view.hyperscale_log_components.sharing_withheld"
                )
                | (crate::source_id::PERFORMANCE_COUNTERS, "view.counter_ratios.invalid")
                | (crate::source_id::PERFORMANCE_COUNTERS, "view.performance_counters.invalid")
                | (crate::source_id::TEMPDB_METADATA_MEMORY, "view.tempdb_metadata_memory.invalid")
                | (crate::source_id::TEMPDB_METADATA_MEMORY, "view.tempdb_metadata_memory.sharing_withheld")
                | (crate::source_id::MEMORY_CLERKS, "collected.memory_clerks.invalid")
                | (crate::source_id::MEMORY_CLERKS, "collected.memory_clerks.sharing_withheld")
                | (crate::telemetry::DATABASE_PROPERTIES_SOURCE, "view.database_properties.invalid")
                | (crate::telemetry::INSTANCE_RESOURCES_SOURCE, "view.instance_resources.invalid")
                | (crate::source_id::FILE_CAPACITY, "view.storage.invalid")
                | (crate::source_id::FILE_CAPACITY, "view.storage.sharing_withheld")
                | (crate::telemetry::DATABASE_PROPERTIES_SOURCE, "view.database_properties.sharing_withheld")
                | (crate::source_id::SUSPECT_PAGES, "view.suspect_pages.invalid")
                | (crate::source_id::SUSPECT_PAGES, "view.suspect_pages.sharing_withheld")
                | (crate::source_id::WAITS, "view.wait_counters.invalid")
                | (crate::telemetry::TIMESCALE_JOBS_SOURCE, "view.jobs.invalid")
                | (crate::telemetry::MSSQL_AGENT_JOBS_SOURCE, "view.jobs.invalid")
                | (crate::telemetry::MSSQL_AGENT_JOBS_SOURCE, "view.mssql_agent_jobs.sharing_withheld")
                | (crate::telemetry::CACHED_RUNTIME_READ_SOURCE, "view.completed_spills.invalid")
                | (crate::source_id::QUERY_STORE, "view.page_server_reads.invalid")
        )
}

fn admit_shared_evidence<T>(
    snapshot: &mut Snapshot,
    source: &str,
    message: crate::i18n::Message,
    result: crate::Result<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(_) => {
            snapshot.record_unavailable_evidence(source, message);
            None
        }
    }
}

/// A snapshot to build text that leaves the machine from (an export, a digest posted to chat): statements, plans,
/// query samples and deadlock statements go through the SQL lexer, so their literals become placeholders, and the
/// parameter values plans were compiled for are dropped. Findings checked from it can only quote what is left, so
/// nothing has to be picked back out of their prose: re-check this, don't redact the findings of the original.
pub fn redact_snapshot(snapshot: &Snapshot) -> Snapshot {
    let engine = snapshot.server.engine;
    let mut s = snapshot.clone();
    if page_server_reads::redact(&mut s).is_err() {
        s.record_unavailable_evidence(crate::source_id::QUERY_STORE, crate::message!("view.page_server_reads.invalid"));
    }
    if !s.runtime_populations.as_ref().is_some_and(|catalog| catalog.is_valid() && !catalog.partitioned) {
        s.runtime_populations = None;
    }
    let resources = std::mem::take(&mut s.resources);
    s.resources = admit_shared_evidence(
        &mut s,
        crate::source_id::RESOURCES,
        crate::message!("collection.invalid_source"),
        resources::redact(resources),
    )
    .unwrap_or_default();
    if let Some(read) = s.pressure.as_mut().and_then(|pressure| pressure.wait_counters.take()) {
        // The raw carrier is validated before masking. Sharing never repairs an invalid original population.
        let names = read.sources.iter().map(|source| source.source.clone()).collect::<Vec<_>>();
        let result = wait_counters::redact(read, engine);
        let redacted = admit_shared_evidence(
            &mut s,
            crate::source_id::WAITS,
            crate::message!("view.wait_counters.invalid"),
            result,
        );
        if let Some(pressure) = &mut s.pressure {
            pressure.wait_counters = redacted;
        }
        for source in &mut s.sources {
            if names.contains(&source.source) {
                source.source = wait_counters::source_name(&source.source);
            }
        }
    }
    for recorded in &mut s.operations.group_replication {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.group_replication.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.galera {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.galera.sharing_withheld"),
        };
    }
    if let Some(recorded) = &mut s.operations.managed_instance_resources {
        **recorded = crate::telemetry::RecordedManagedInstanceResourceRead::Unavailable {
            reason: crate::message!("view.mi_resources.sharing_withheld"),
        };
    }
    if let Some(recorded) = &mut s.operations.pool_settings {
        **recorded = crate::telemetry::RecordedPoolSettingsRead::Unavailable {
            reason: crate::message!("view.pool_settings.sharing_withheld"),
        };
    }
    if let Some(recorded) = &mut s.operations.runtime_fragments {
        **recorded = crate::telemetry::RecordedQueryHistoryRead::Unavailable {
            reason: crate::message!("view.runtime_fragments.sharing_withheld"),
        };
    }
    if let Some(recorded) = &mut s.operations.query_wait_fragments {
        **recorded = crate::telemetry::RecordedQueryHistoryRead::Unavailable {
            reason: crate::message!("view.query_wait_fragments.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.indexed_view_originals {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.indexed_views.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.index_configuration {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.index_configuration.sharing_withheld"),
        };
    }
    if s.operations.provider_acquisition.is_some() {
        s.operations.provider_acquisition = Some(Box::new(crate::RecordedProviderAcquisition::Unavailable {
            reason: crate::message!("view.provider_acquisition.sharing_withheld"),
        }));
    }
    for recorded in &mut s.operations.source_acquisitions {
        *recorded = crate::RecordedSourceAcquisition::Unavailable {
            reason: crate::message!("view.source_overhead.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.table_memory {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.table_memory.sharing_withheld"),
        };
    }
    for recorded in s.operations.cdc_history.iter_mut().chain(&mut s.operations.change_feed_history) {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.change_capture.history.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.process_cpu_originals {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.process_cpu.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.numa_nodes {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.numa_nodes.sharing_withheld"),
        };
    }
    for recorded in &mut s.operations.full_text_populations {
        *recorded = crate::telemetry::RecordedOperationalRead::Unavailable {
            reason: crate::message!("view.full_text_populations.sharing_withheld"),
        };
    }
    if let Some(recorded) = &mut s.operations.database_performance_counters {
        // An indivisible original cannot be masked and re-signed. Keep only this population's typed gap;
        // its shared-counter sibling has independent source coverage and remains untouched.
        *recorded = crate::telemetry::RecordedPerformanceCounters::Unavailable {
            reason: crate::message!("view.performance_counters.sharing_withheld"),
        };
    }
    if s.operations.hyperscale_log_components.take().is_some() {
        s.record_unavailable_evidence(
            crate::telemetry::HYPERSCALE_LOG_COMPONENTS_SOURCE,
            crate::message!("view.hyperscale_log_components.sharing_withheld"),
        );
    }
    if s.operations.storage.take().is_some() {
        s.record_unavailable_evidence(
            crate::source_id::FILE_CAPACITY,
            crate::message!("view.storage.sharing_withheld"),
        );
    }
    if s.operations.tempdb_metadata_memory.take().is_some() {
        // The exact receipt/body is indivisible; masking cannot grant a new signature or source incarnation.
        s.record_unavailable_evidence(
            crate::source_id::TEMPDB_METADATA_MEMORY,
            crate::message!("view.tempdb_metadata_memory.sharing_withheld"),
        );
    }
    if s.operations.memory_clerks.take().is_some() {
        // Original receipt inputs are indivisible: withholding cannot repair or re-sign a masked body.
        s.record_unavailable_evidence(
            crate::source_id::MEMORY_CLERKS,
            crate::message!("collected.memory_clerks.sharing_withheld"),
        );
    }
    if s.operations.suspect_pages.take().is_some() {
        s.record_unavailable_evidence(
            crate::source_id::SUSPECT_PAGES,
            crate::message!("view.suspect_pages.sharing_withheld"),
        );
    }
    if s.operations.database_properties.take().is_some() {
        s.record_unavailable_evidence(
            crate::telemetry::DATABASE_PROPERTIES_SOURCE,
            crate::message!("view.database_properties.sharing_withheld"),
        );
    }
    if let Some(read) = s.pressure.as_mut().and_then(|pressure| pressure.instance_resources.take()) {
        let retained = admit_shared_evidence(
            &mut s,
            crate::telemetry::INSTANCE_RESOURCES_SOURCE,
            crate::message!("view.instance_resources.invalid"),
            instance_resources::redact(read),
        );
        if let Some(pressure) = &mut s.pressure {
            pressure.instance_resources = retained;
        }
    }
    let counters = s.pressure.as_mut().and_then(|pressure| pressure.resource_counters.take());
    if let Some(counters) = counters {
        let (clerks, counters): (Vec<_>, Vec<_>) =
            counters.into_iter().partition(crate::telemetry::memory_clerk_candidate);
        if !clerks.is_empty() {
            s.record_unavailable_evidence(
                crate::source_id::MEMORY_CLERKS,
                crate::message!("collected.memory_clerks.sharing_withheld"),
            );
        }
        let result = resource_counters::redact(counters, engine.spec().performance_counters());
        let redacted = admit_shared_evidence(
            &mut s,
            crate::source_id::PERFORMANCE_COUNTERS,
            crate::message!("view.counter_ratios.invalid"),
            result,
        );
        if let Some(pressure) = &mut s.pressure {
            pressure.resource_counters = redacted;
        }
    }
    if let Some(identity) = &mut s.identity {
        redact_identity(identity);
    }
    if let Some(population) = s.options.as_mut().and_then(|options| options.workload_population.as_mut()) {
        redact_native_identity(population);
    }
    for note in &mut s.notes {
        if note.table.is_none() && public_evidence_issue(&note.collector, note.message_source.as_ref()) {
            note.refresh_language();
            continue;
        }
        note.message_source = None;
        note.message = "Collection details omitted from shared data.".into();
    }
    for source in &mut s.sources {
        if public_evidence_issue(&source.source, source.reason_message.as_ref()) {
            source.refresh_language();
            continue;
        }
        source.reason_message = None;
        if source.reason.is_some() {
            source.reason = Some("Collection details omitted from shared data.".into());
        }
    }
    for missing in &mut s.server.missing {
        missing.reason_message = None;
        missing.fix_message = None;
        missing.reason = "This capability was not collected; diagnostic details omitted from shared data.".into();
        missing.fix = None;
        missing.action = None;
    }
    for failure in &mut s.operations.log_failures {
        failure.statement = redact_sql(&failure.statement, engine);
    }
    if let Some(resource) = s.operations.memory_failures.as_mut().and_then(|evidence| evidence.resource.as_mut()) {
        redact_resource_identity(resource);
    }
    if let Some(read) = s.operations.cached_runtime.take() {
        let result = cached_runtime::redact(&mut s, read);
        s.operations.cached_runtime = admit_shared_evidence(
            &mut s,
            crate::telemetry::CACHED_RUNTIME_READ_SOURCE,
            crate::message!("view.completed_spills.invalid"),
            result,
        );
    }
    if let Some(evidence) = s.operations.jobs.take() {
        let source = evidence.primary_source();
        let message = if matches!(evidence.as_ref(), crate::telemetry::JobsSnapshot::MssqlAgent { .. })
            && evidence.is_valid()
        {
            crate::message!("view.mssql_agent_jobs.sharing_withheld")
        } else if matches!(evidence.as_ref(), crate::telemetry::JobsSnapshot::MysqlEvents { .. }) && evidence.is_valid()
        {
            crate::message!("view.mysql_events.sharing_withheld")
        } else {
            crate::message!("view.jobs.invalid")
        };
        s.operations.jobs = admit_shared_evidence(&mut s, source, message, jobs::redact(*evidence));
    }
    let configured = s.pressure.as_mut().and_then(|pressure| pressure.configured_log_rates.take());
    if let Some(evidence) = configured {
        let redacted = admit_shared_evidence(
            &mut s,
            crate::telemetry::CONFIGURED_LOG_RATES_SOURCE,
            crate::message!("view.configured_log_rates.invalid"),
            configured_log_rates::redact(evidence),
        );
        if let Some(pressure) = &mut s.pressure {
            pressure.configured_log_rates = redacted;
        }
    }
    let connections = s.pressure.as_mut().and_then(|pressure| pressure.connection_outcomes.take());
    if let Some(evidence) = connections {
        let redacted = admit_shared_evidence(
            &mut s,
            crate::source_id::SESSIONS,
            crate::message!("view.connection_outcomes.invalid"),
            connection_outcomes::redact(evidence),
        );
        if let Some(pressure) = &mut s.pressure {
            pressure.connection_outcomes = redacted;
        }
    }
    if let Some(evidence) = &mut s.operations.native_failures {
        evidence.redact();
        if let Some(resource) = &mut evidence.resource {
            redact_resource_identity(resource);
        }
    }
    if let Some(evidence) = &mut s.operations.session_memory {
        redact_resource_identity(&mut evidence.resource);
        for row in &mut evidence.observations {
            redact_native_identity(&mut row.identity);
        }
    }
    if let Some(evidence) = &mut s.operations.compiler_admission {
        if let Some(context) = &mut evidence.context {
            redact_read_context(context);
        }
        for row in &mut evidence.observations {
            if let crate::telemetry::OperationalFact::CompilerWaiter(waiter) = &mut row.fact {
                waiter.request.redact(engine);
                redact_activity_context_for_sharing(&mut waiter.request);
            }
        }
    }
    s.operations.statement_work = s.operations.statement_work.take().map(|read| {
        // Invalid original evidence remains an explicit gap; masking must not repair it into native proof.
        let source = opaque(&read.source);
        statement_work::redact(read).unwrap_or_else(|error| {
            Box::new(crate::telemetry::StatementCounterSnapshot::unavailable(
                &source,
                error.code,
                Some(crate::telemetry::CollectionStop::InvalidPage),
            ))
        })
    });
    if let Some(context) = s.operations.query_compilation.as_mut().and_then(|read| read.context.as_mut()) {
        redact_read_context(context);
    }
    if let Some(context) = s.compilations_before.as_mut().and_then(|baseline| baseline.context.as_mut()) {
        redact_read_context(context);
    }
    if let Some(evidence) = &mut s.operations.maintenance_progress {
        redact_read_context(&mut evidence.context);
        if let Some(focus) = &mut evidence.result.focus {
            redact_target(&mut focus.target);
            redact_partition(&mut focus.partition);
            focus.epoch = opaque(&focus.epoch);
            focus.original.redact(engine);
            redact_activity_context_for_sharing(&mut focus.original);
            redact_native_identity(&mut focus.statement_identity);
            focus.original.statement_identity = Some(focus.statement_identity.clone());
        }
        for row in &mut evidence.observations {
            if let Some(statement) = &mut row.statement {
                redact_native_identity(statement);
            }
        }
    }
    if let Some(evidence) = &mut s.operations.page_waits {
        for observation in &mut evidence.observations {
            observation.redact(engine);
            redact_activity_context_for_sharing(&mut observation.request);
            if let Some(target) = &mut observation.target
                && !redact_page_target(target)
            {
                observation.target = None;
                observation.resolution = crate::telemetry::PageWaitResolution::UnverifiedSource;
                observation.statement_identity = None;
                observation.plan_identity = None;
                if let Some(crate::telemetry::PageIndexOperations::Observed { relation, .. }) =
                    &mut observation.index_operations
                {
                    *relation = crate::telemetry::PageIndexOperationRelation::UnverifiedTarget;
                }
            }
            if let Some(crate::telemetry::PageIndexOperations::Observed { object: Some(object), .. }) =
                &mut observation.index_operations
            {
                redact_native_qualifiers(object);
            }
        }
    }
    if let Some(evidence) = &mut s.operations.task_space {
        for observation in &mut evidence.observations {
            observation.redact(engine);
            if let Some(request) = &mut observation.request {
                redact_activity_context_for_sharing(request);
            }
        }
    }
    if let Some(resources) = &mut s.operations.node_resources {
        for message in resources.source_message.iter_mut().chain(resources.note_messages.iter_mut().flatten()) {
            redact_scoped_message(message);
        }
        resources.source = redact_scope(&resources.source);
        for note in &mut resources.notes {
            *note = redact_scope(note);
        }
        for node in &mut resources.nodes {
            node.node = opaque(&node.node);
            if let Some(message) = node.process_memory.as_mut().and_then(|memory| memory.name_message.as_mut()) {
                redact_scoped_message(message);
            }
        }
    }
    if let Some(history) = &mut s.operations.activity_history {
        for message in history.period.source_message.iter_mut().chain(history.period.note_messages.iter_mut().flatten())
        {
            redact_scoped_message(message);
        }
        history.period.source = redact_scope(&history.period.source);
        for note in &mut history.period.notes {
            *note = redact_scope(note);
        }
        for node in &mut history.period.nodes {
            *node = opaque(node);
        }
        for event in &mut history.events {
            event.node = event.node.as_deref().map(opaque);
        }
    }
    if let Some(history) = &mut s.operations.termination_history {
        for message in history
            .source_message
            .iter_mut()
            .chain(history.visibility_message.iter_mut())
            .chain(history.note_messages.iter_mut().flatten())
        {
            redact_scoped_message(message);
        }
        history.source = redact_scope(&history.source);
        history.visibility = redact_scope(&history.visibility);
        for note in &mut history.notes {
            *note = redact_scope(note);
        }
        for node in &mut history.nodes {
            *node = opaque(node);
        }
        for event in &mut history.events {
            event.node = event.node.as_deref().map(opaque);
        }
    }
    s.operations.render_recorded_messages();
    for table in &mut s.tables {
        table.definition = table.definition.as_deref().map(|sql| redact_sql(sql, engine));
        if let Some(storage) = &mut table.storage {
            for option in &mut storage.options {
                if let Some(bound) = option.strip_prefix("partition_bound=") {
                    *option = format!("partition_bound={}", redact_partition_bound(bound, engine));
                }
            }
        }
    }
    for index in &mut s.indexes {
        if let Some(owner) = index.contention.as_mut().and_then(|counters| counters.source_owner.as_mut()) {
            redact_resource_identity(owner);
        }
        if index.filter_facts.is_none()
            && let Some(filter) = &index.filter
        {
            index.filter_facts = Some(crate::IndexFilterFacts {
                fingerprint: opaque(filter),
                matching_queries: snapshot
                    .queries()
                    .iter()
                    .filter(|q| crate::index_filter::matches(&index.table, filter, &q.text))
                    .map(|q| q.id.clone())
                    .collect(),
                equalities: crate::index_filter::filter_equalities(filter).map(|pairs| {
                    pairs.into_iter().map(|(column, value)| (column, redact_sql(&value, engine))).collect()
                }),
            });
        }
        if let Some(facts) = &mut index.filter_facts {
            facts.fingerprint = opaque(&facts.fingerprint);
            if let Some(equalities) = &mut facts.equalities {
                for (_, value) in equalities {
                    *value = redact_sql(value, engine);
                }
            }
        }
        index.filter = index.filter.as_deref().map(|sql| redact_sql(sql, engine));
        index.definition = index.definition.as_deref().map(|sql| redact_sql(sql, engine));
    }
    for column in &mut s.columns {
        column.default = column.default.as_deref().map(|sql| redact_sql(sql, engine));
    }
    for point in s.contention_before.iter_mut().flat_map(|baseline| baseline.indexes.values_mut()) {
        if let Some(owner) = &mut point.source_owner {
            redact_resource_identity(owner);
        }
    }
    for point in s
        .escalations_before
        .iter_mut()
        .flat_map(|baseline| baseline.tables.values_mut())
        .flat_map(|indexes| indexes.values_mut())
    {
        redact_resource_identity(&mut point.source_owner);
    }
    for reference in s
        .escalations_before
        .iter_mut()
        .filter_map(|b| b.reference.as_mut())
        .chain(s.contention_before.iter_mut().filter_map(|b| b.reference.as_mut()))
        .chain(s.sizes_before.iter_mut().filter_map(|b| b.reference.as_mut()))
        .chain(s.evictions_before.iter_mut().filter_map(|b| b.reference.as_mut()))
        .chain(s.compilations_before.iter_mut().filter_map(|b| b.reference.as_mut()))
        .chain(s.operations.counters.iter_mut().filter_map(|c| c.before.as_mut()).filter_map(|b| b.reference.as_mut()))
        .chain(s.operations.sqlite.iter_mut().filter_map(|c| c.before.as_mut()).filter_map(|b| b.reference.as_mut()))
    {
        redact_reference(reference);
    }
    if let Some(measurement) = s.evictions_before.as_mut().and_then(|b| b.measurement.as_mut()) {
        redact_measurement(measurement);
    }
    if let Some(w) = &mut s.workload {
        if let Some(population) = &mut w.population {
            redact_native_identity(population);
        }
        // Partitioned catalog history can contain endpoints and replica names. An empty, positively unpartitioned
        // catalog has no private population data and retains the native proof used by independent plan advice.
        if !w.population_catalog.as_ref().is_some_and(|catalog| catalog.is_valid() && !catalog.partitioned) {
            w.population_catalog = None;
        }
        w.source = redact_scope(&w.source);
        w.source_label = None;
        w.source_message = None;
        if let Some(counters) = &mut w.counter_set {
            counters.members =
                std::mem::take(&mut counters.members).into_iter().map(|(node, epoch)| (opaque(&node), epoch)).collect();
        }
        for reference in &mut w.history_inputs {
            redact_baseline(reference);
        }
        for q in &mut w.queries {
            // Shared query labels and metrics are not authority for a current native lookup.
            q.native_identity = None;
            q.native_plan_identity = None;
            q.text = redact_sql(&q.text, engine);
            q.plan = q.plan.as_ref().map(redact_plan);
            if let Some(history) = &mut q.plan_history {
                for message in history.source_message.iter_mut().chain(history.note_message.iter_mut()) {
                    redact_scoped_message(message);
                }
                history.source = redact_scope(&history.source);
                crate::i18n::render_text(&mut history.source, history.source_message.as_ref());
                crate::i18n::render_text(&mut history.note, history.note_message.as_ref());
                history.captured_plan = history.captured_plan.as_ref().map(redact_plan);
            }
            for p in &mut q.plans {
                p.native_identity = None;
                p.compiled = None;
            }
        }
    }
    if let Some(tuning) = &mut s.tuning {
        for recommendation in &mut tuning.recommendations {
            recommendation.script = recommendation.script.as_deref().map(|sql| redact_sql(sql, engine));
            recommendation.reason = recommendation.reason.as_deref().map(|t| redact_prose(t, engine));
        }
        for forced in &mut tuning.forced_plans {
            forced.last_failure_reason =
                forced.last_failure_reason.as_deref().map(|_| "Failure details omitted from shared data.".into());
        }
        for hint in &mut tuning.hints {
            hint.hint = redact_sql(&hint.hint, engine);
            hint.last_failure_reason =
                hint.last_failure_reason.as_deref().map(|_| "Failure details omitted from shared data.".into());
        }
    }
    if let Some(p) = &mut s.pressure {
        if let Some(transaction) = &mut p.oldest_transaction {
            transaction.program = None;
        }
        if let Some(retention) = &mut p.retention {
            for holder in &mut retention.holders {
                holder.redact_context();
            }
        }
        if let Some(grants) = &mut p.request_memory {
            redact_read_context(&mut grants.context);
            for grant in &mut grants.observations {
                grant.redact(engine);
                if let Some(request) = &mut grant.request {
                    redact_activity_context_for_sharing(request);
                }
            }
        }
        for read in &mut p.grants {
            redact_target(&mut read.description.target);
            redact_partition(&mut read.description.partition);
            read.capture.observation_id = opaque(&read.capture.observation_id);
            // A shared artifact retains read distinction, not an address into the owner's private outbox.
            read.capture.revision = None;
            for row in &mut read.observations {
                if let crate::telemetry::DefinitionPayload::Available { text, .. } = &row.statement {
                    row.statement = crate::telemetry::DefinitionPayload::sql(text, engine);
                }
            }
        }
        for file in &mut p.files {
            file.name = shareable_name(&file.name);
        }
        for replica in &mut p.replicas {
            replica.error = replica.error.as_deref().map(|e| {
                if e.is_empty() { "".into() } else { "Replication error details omitted from shared data.".into() }
            });
        }
        // A failed re-key cannot leave a raw native identity in the shared snapshot.
        p.replication = p.replication.take().and_then(|read| replication::redact(read).ok());
    }
    for space in &mut s.operations.spaces {
        space.origin = None;
        space.name = shareable_name(&space.name);
        for part in &mut space.parts {
            part.name = shareable_name(&part.name);
        }
    }
    for f in &mut s.query_families {
        f.sample = redact_sql(&f.sample, engine);
    }
    if let Some(report) = &mut s.deadlocks {
        for d in &mut report.deadlocks {
            for p in &mut d.processes {
                p.statement = p.statement.as_deref().map(|t| redact_sql(t, engine));
                // Who ran it: an Entra login is a person's email address.
                p.login = None;
                p.host = None;
                p.application = None;
            }
            // The headline, explanation and fixes quote the processes: written again from what is left.
            crate::deadlock::classify(engine, d);
        }
        if report.note.is_some() {
            report.note = Some(
                engine
                    .spec()
                    .deadlock_limitation()
                    .unwrap_or_else(|| "Deadlock collection was limited; details omitted from shared data.".into()),
            );
        }
    }
    s
}

/// Redact literal values in a partition bound expression (`FOR VALUES IN ('tenant_a', 'tenant_b')`) while keeping
/// `DEFAULT`, `MAXVALUE`, and ISO date/timestamp range bounds (`'2026-10-01'`) so partition expiry checks still work
/// on shared snapshots.
pub(crate) fn redact_partition_bound(bound: &str, engine: Engine) -> String {
    if bound.eq_ignore_ascii_case("default") || bound.eq_ignore_ascii_case("maxvalue") {
        return bound.to_string();
    }
    let mut dates: Vec<String> = Vec::new();
    let mut masked = String::with_capacity(bound.len());
    let mut rest = bound;
    while let Some(open) = rest.find('\'') {
        masked.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = find_closing_single_quote(after_open) {
            let candidate = &after_open[..close];
            if is_iso_date_or_timestamp(candidate) {
                let idx = dates.len();
                dates.push(format!("'{candidate}'"));
                masked.push_str(&format!("__culprit_date_{idx}__"));
            } else {
                masked.push_str(&rest[open..open + 1 + close + 1]);
            }
            rest = &after_open[close + 1..];
            continue;
        }
        masked.push_str(&rest[open..]);
        rest = "";
        break;
    }
    masked.push_str(rest);
    let mut redacted = redact_sql(&masked, engine);
    for (idx, date_lit) in dates.iter().enumerate() {
        redacted = redacted.replace(&format!("__culprit_date_{idx}__"), date_lit);
    }
    redacted
}

fn find_closing_single_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_iso_date_or_timestamp(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.len() < 10 || trimmed.len() > 35 {
        return false;
    }
    let b = trimmed.as_bytes();
    let is_ymd = b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit();
    if !is_ymd {
        return false;
    }
    if trimmed.len() == 10 {
        crate::format::epoch_seconds(&format!("{trimmed}T00:00:00Z")).is_some()
    } else {
        crate::format::epoch_seconds(trimmed).is_some()
    }
}

/// A second layer over [`redact_snapshot`], for text from a finding that wasn't checked from a redacted snapshot (a
/// plug-in check an export can't re-run): its prose redacted with [`redact_prose`], which reads SQL out of prose by
/// heuristics and so can't be the only layer. Fix SQL can also quote catalog predicates or plugin-supplied values.
pub fn redact_finding(finding: &Finding, engine: Engine) -> Finding {
    let prose = |t: &str| redact_prose(t, engine);
    let mut f = finding.clone();
    // Original editor locations authorize edits only in the original private document.
    f.facts.statement = None;
    // A standalone finding has no shared snapshot against which to rebuild its private read/row reference.
    f.facts.statement_grant = None;
    f.title = prose(&f.title);
    f.summary = prose(&f.summary);
    f.caveats = f.caveats.iter().map(|c| prose(c)).collect();
    f.verify = f.verify.iter().map(|v| prose(v)).collect();
    for m in &mut f.metrics {
        m.text = m.text.as_deref().map(prose);
    }
    for q in &mut f.queries {
        q.note = q.note.as_deref().map(prose);
    }
    if let Some(fix) = &mut f.fix {
        fix.summary = prose(&fix.summary);
        fix.cost = fix.cost.as_deref().map(prose);
        fix.sql = fix.sql.as_deref().map(|sql| redact_sql(sql, engine));
    }
    f
}
