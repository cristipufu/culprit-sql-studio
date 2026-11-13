//! What a scan could read. A provider reads from sources: Query Store, the index usage view, pg_stat_statements,
//! performance_schema digests, the catalog… Each source either delivers a capability or doesn't, and one that fails
//! becomes a note and a locked capability with the reason and the fix, never a failed scan. Every provider keeps that
//! account with a [`Coverage`], so "what this login can see" means the same thing on every engine and checks that need
//! a missing capability are listed as locked instead of running on missing data.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{Error, ErrorCode};
use crate::model::{Capability, CollectorNote, MissingCapability};

/// What actually happened when a source was read. Notes are prose, never a success/failure protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceState {
    Collected,
    Empty,
    Partial,
    Unavailable,
    NotApplicable,
}

/// Source names are open identifiers so an embedding provider can declare its own sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceOutcome {
    pub source: String,
    pub state: SourceState,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason_message: Option<crate::i18n::Message>,
}

/// Canonical built-in source identifiers shared between engine collectors and check evidence contracts.
/// External providers may still record custom source names.
pub mod source_id {
    pub const CATALOG: &str = "catalog";
    pub const TABLES: &str = "tables";
    pub const INDEXES: &str = "indexes";
    pub const COLUMNS: &str = "columns";
    pub const FOREIGN_KEYS: &str = "foreign_keys";
    pub const STATISTICS: &str = "statistics";
    pub const SIZES: &str = "sizes";
    pub const TABLE_STORAGE: &str = "table storage";
    pub const CONSTRAINTS: &str = "constraints";
    pub const CHECK_CONSTRAINTS: &str = "check_constraints";
    pub const OBJECTS: &str = "objects";
    pub const TRIGGERS: &str = "triggers";
    pub const PARTITIONS: &str = "partitions";
    pub const RETAINED_PLANS: &str = "retained_plans";
    pub const TIMESCALE_HYPERTABLES: &str = "timescale-hypertables";
    pub const TIMESCALE_CHUNKS: &str = "timescale-chunks";
    pub const TIMESCALE_INDEXES: &str = "timescale-indexes";
    pub const DOCDB_EPOCHS: &str = "docdb-counter-epochs";
    pub const DOCDB_INDEX_USAGE: &str = "docdb-index-usage";
    pub const DOCDB_PLAN_HISTORY: &str = "docdb-plan-history";
    pub const DOCDB_ACTIVITY_HISTORY: &str = "docdb-activity-history";
    pub const DOCDB_TERMINATIONS: &str = "docdb-terminations";
    pub const DOCDB_NODE_RESOURCES: &str = "docdb-node-resources";

    pub const WORKLOAD: &str = "workload";
    pub const QUERY_STORE: &str = "query_store";
    pub const QUERY_WAITS: &str = "query_waits";
    pub const PLAN_CACHE: &str = "plan_cache";
    pub const QUERY_LOG: &str = "query log";
    pub const QUERY_COSTS: &str = "query costs";
    pub const QUERY_COMPILATION: &str = "query compilation";
    pub const QUERY_ERRORS: &str = "query errors";
    pub const QUERY_FAMILIES: &str = "query_families";
    pub const PLAN_CACHE_USE: &str = "plan cache use";
    pub const HISTOGRAMS: &str = "histograms";

    pub const MISSING_INDEXES: &str = "missing_indexes";
    pub const FORWARDED_FETCHES: &str = "forwarded_fetches";
    pub const TUNING: &str = "tuning";
    pub const INDEX_OPERATIONS: &str = "index operations";
    pub const INDEX_CONTENTION: &str = "index contention";
    pub const LIVE_LOCKS: &str = "live_locks";
    pub const METADATA_LOCKS: &str = "metadata_locks";

    pub const SETTINGS: &str = "settings";
    pub const DATABASE_SETTINGS: &str = "database settings";
    pub const SCOPED_SETTINGS: &str = "database scoped settings";
    pub const SERVER_SETTINGS: &str = "server settings";
    pub const QUERY_STORE_SETTINGS: &str = "query store settings";
    pub const ADVICE_SETTINGS: &str = "advice settings";
    pub const SERVER_VARIABLES: &str = "server variables";
    pub const WAITS: &str = "waits";
    pub const WAIT_TOTALS: &str = "wait totals";
    pub const GLOBAL_STATUS: &str = "global status";
    pub const PROCESSOR_TOPOLOGY: &str = "processor topology";
    pub const MEMORY_CLERKS: &str = "memory clerks";
    pub const MEMORY_FAILURES: &str = "memory failures";
    pub const SCHEDULERS: &str = "schedulers";
    pub const SHARED_MEMORY_ALLOCATIONS: &str = "shared memory allocations";
    pub const SHARED_RESOURCE_DETAILS: &str = "shared-resource-details";
    pub const CURRENT_REQUEST_MEMORY: &str = "current request memory";
    pub const REQUEST_MEMORY_DETAILS: &str = "request-memory-details";
    pub const PERFORMANCE_COUNTERS: &str = "performance counters";
    pub const DATABASE_STATS: &str = "database statistics";
    pub const RESOURCES: &str = "resources";
    pub const POOL_RESOURCES: &str = "pool resources";
    pub const RESOURCE_USE: &str = "resource use";
    pub const CHECKPOINTS: &str = "checkpoints";
    pub const OPEN_TRANSACTIONS: &str = "open transactions";
    pub const CLEANUP_SLOTS: &str = "cleanup slots";
    pub const CLEANUP_PREPARED: &str = "cleanup prepared";
    pub const PENDING_GRANTS: &str = "memory grants";
    pub const WORKER_QUEUE: &str = "worker queue";
    pub const COMPILATION_QUEUE: &str = "compilation queue";
    pub const COMPILATION_GATEWAYS: &str = "compilation gateways";
    pub const VERSION_STORE: &str = "version store";
    pub const PERSISTENT_VERSION_STORE: &str = "persistent version store";
    pub const TUNING_OPTIONS: &str = "automatic tuning options";
    pub const TUNING_RECOMMENDATIONS: &str = "tuning recommendations";
    pub const FORCED_PLANS: &str = "forced plans";
    pub const QUERY_HINTS: &str = "query hints";
    pub const GRANTS: &str = "memory grants per query";
    pub const FILE_LATENCY: &str = "file latency";
    pub const SESSIONS: &str = "sessions";
    pub const CONNECTIONS: &str = "connections";
    pub const LOGINS: &str = "logins";
    pub const REPLICATION: &str = "replication";
    pub const SIGNALS: &str = "signals";

    pub const LOG_CAPACITY: &str = "log_capacity";
    pub const FILE_CAPACITY: &str = "file_capacity";
    pub const TEMP_CAPACITY: &str = "temp_capacity";
    pub const SUSPECT_PAGES: &str = "suspect_pages";
    pub const SEQUENCE_CAPACITY: &str = "sequence_capacity";
    pub const MULTIXACTS: &str = "multixacts";
    pub const ARCHIVER: &str = "archiver";
    pub const SUBSCRIPTION_ERRORS: &str = "subscription_errors";
    pub const STANDBY_CONFLICTS: &str = "standby_conflicts";
    pub const REDO_PRESSURE: &str = "redo_pressure";
    pub const CLUSTER_HEALTH: &str = "cluster_health";
    pub const WAL_FILE: &str = "wal_file";
    pub const DEADLOCKS: &str = "deadlocks";

    pub const ALL: &[&str] = &[
        CATALOG,
        TABLES,
        INDEXES,
        COLUMNS,
        FOREIGN_KEYS,
        STATISTICS,
        SIZES,
        TABLE_STORAGE,
        CONSTRAINTS,
        CHECK_CONSTRAINTS,
        OBJECTS,
        TRIGGERS,
        PARTITIONS,
        RETAINED_PLANS,
        TIMESCALE_HYPERTABLES,
        TIMESCALE_CHUNKS,
        TIMESCALE_INDEXES,
        DOCDB_EPOCHS,
        DOCDB_INDEX_USAGE,
        DOCDB_PLAN_HISTORY,
        DOCDB_ACTIVITY_HISTORY,
        DOCDB_TERMINATIONS,
        DOCDB_NODE_RESOURCES,
        WORKLOAD,
        QUERY_STORE,
        QUERY_WAITS,
        PLAN_CACHE,
        QUERY_LOG,
        QUERY_COSTS,
        QUERY_COMPILATION,
        QUERY_ERRORS,
        QUERY_FAMILIES,
        PLAN_CACHE_USE,
        HISTOGRAMS,
        MISSING_INDEXES,
        FORWARDED_FETCHES,
        TUNING,
        INDEX_CONTENTION,
        INDEX_OPERATIONS,
        LIVE_LOCKS,
        METADATA_LOCKS,
        SETTINGS,
        DATABASE_SETTINGS,
        SCOPED_SETTINGS,
        SERVER_SETTINGS,
        QUERY_STORE_SETTINGS,
        ADVICE_SETTINGS,
        SERVER_VARIABLES,
        WAITS,
        WAIT_TOTALS,
        GLOBAL_STATUS,
        PROCESSOR_TOPOLOGY,
        MEMORY_CLERKS,
        MEMORY_FAILURES,
        SCHEDULERS,
        SHARED_MEMORY_ALLOCATIONS,
        SHARED_RESOURCE_DETAILS,
        CURRENT_REQUEST_MEMORY,
        REQUEST_MEMORY_DETAILS,
        PERFORMANCE_COUNTERS,
        DATABASE_STATS,
        RESOURCES,
        POOL_RESOURCES,
        RESOURCE_USE,
        CHECKPOINTS,
        OPEN_TRANSACTIONS,
        CLEANUP_SLOTS,
        CLEANUP_PREPARED,
        PENDING_GRANTS,
        WORKER_QUEUE,
        COMPILATION_QUEUE,
        COMPILATION_GATEWAYS,
        VERSION_STORE,
        PERSISTENT_VERSION_STORE,
        TUNING_OPTIONS,
        TUNING_RECOMMENDATIONS,
        FORCED_PLANS,
        QUERY_HINTS,
        GRANTS,
        FILE_LATENCY,
        SESSIONS,
        CONNECTIONS,
        LOGINS,
        REPLICATION,
        SIGNALS,
        LOG_CAPACITY,
        FILE_CAPACITY,
        TEMP_CAPACITY,
        SEQUENCE_CAPACITY,
        SUSPECT_PAGES,
        MULTIXACTS,
        ARCHIVER,
        SUBSCRIPTION_ERRORS,
        STANDBY_CONFLICTS,
        REDO_PRESSURE,
        CLUSTER_HEALTH,
        WAL_FILE,
        DEADLOCKS,
    ];
}

pub fn same_source(a: &str, b: &str) -> bool {
    let normalize = |byte: u8| if byte == b' ' { b'_' } else { byte.to_ascii_lowercase() };
    a.len() == b.len() && a.bytes().map(normalize).eq(b.bytes().map(normalize))
}

impl SourceOutcome {
    pub fn refresh_language(&mut self) {
        if let Some(message) = &self.reason_message {
            self.reason = Some(message.render());
        }
    }

    /// Combine independently read parts conservatively. A successful part cannot erase a failed part.
    fn merge(&mut self, other: Self) {
        use SourceState::*;
        self.state = match (self.state, other.state) {
            (a, b) if a == b => a,
            (Collected, Empty) | (Empty, Collected) => Collected,
            _ => Partial,
        };
        if let Some(reason) = other.reason {
            use crate::i18n::Message;
            let incoming = other.reason_message.unwrap_or_else(|| Message::from(reason));
            let message = match self.reason.take() {
                Some(existing) => {
                    let existing = self.reason_message.take().unwrap_or_else(|| Message::from(existing));
                    if existing == incoming { existing } else { Message::paragraph(vec![existing, incoming]) }
                }
                None => incoming,
            };
            self.reason = Some(message.render());
            self.reason_message = message.is_translatable().then_some(message);
        }
    }
}

/// One source a provider reads from its engine: a statistics view, the catalog, a log. Each provider declares its
/// sources in one list (`<provider>/src/sources.rs`), so what a scan reads on an engine is a table anyone can read, and
/// reading one goes through [`Coverage::read`], so what a failing source means is said once for every engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    /// Its name in the scan's notes: "tables", "query_store".
    pub name: &'static str,
    /// Catalog key describing what it reads, used in "Reading {reads} failed".
    pub reads: &'static str,
    /// What the checks get from it: the capabilities a failure locks. Empty for a source that only adds detail.
    pub delivers: &'static [Capability],
}

impl Source {
    pub const SHARED_RESOURCE_DETAILS: Self = Self {
        name: source_id::SHARED_RESOURCE_DETAILS,
        reads: crate::key!("provider.shared_resources.details"),
        delivers: &[],
    };
    pub const REQUEST_MEMORY_DETAILS: Self = Self {
        name: source_id::REQUEST_MEMORY_DETAILS,
        reads: crate::key!("provider.request_memory.details"),
        delivers: &[],
    };
    pub fn description(&self) -> String {
        crate::i18n::t(self.reads, &[])
    }
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut source = serializer.serialize_struct("Source", 3)?;
        source.serialize_field("name", self.name)?;
        source.serialize_field("reads", &self.description())?;
        source.serialize_field("delivers", self.delivers)?;
        source.end()
    }
}

/// The capabilities a scan's sources delivered, the ones they couldn't (with why and how to fix it), and notes.
#[derive(Default, Debug, Clone)]
pub struct Coverage {
    have: Vec<Capability>,
    missing: Vec<MissingCapability>,
    notes: Vec<CollectorNote>,
    sources: Vec<SourceOutcome>,
}

impl Coverage {
    /// A requested profile with no native implementation is unavailable, never healthy absence or a failed scan.
    pub fn shared_resource_details_unavailable(&mut self, engine: crate::Engine) {
        self.source_failed(
            &Source::SHARED_RESOURCE_DETAILS,
            &Error::new(
                ErrorCode::Unsupported,
                crate::message!("provider.shared_resources.unsupported", engine = engine.label()),
            ),
        );
    }
    /// Native per-request memory is a capability; an unsupported explicit request never becomes a measured zero.
    pub fn request_memory_unavailable(&mut self, engine: crate::Engine) {
        self.source_failed(
            &Source::REQUEST_MEMORY_DETAILS,
            &Error::new(
                ErrorCode::Unsupported,
                crate::message!("provider.request_memory.unsupported", engine = engine.label()),
            ),
        );
    }
    /// Start from what a probe already found: the capabilities the login and server settings allow, and what they
    /// don't. Sources then confirm them, or [`Coverage::lost`] one they couldn't read after all.
    pub fn with(have: Vec<Capability>, missing: Vec<MissingCapability>) -> Self {
        Coverage { have, missing, ..Self::default() }
    }

    pub fn recorded(&mut self, source: &Source, state: SourceState, reason: Option<String>) {
        self.record(SourceOutcome { reason_message: None, source: source.name.into(), state, reason });
    }

    pub fn recorded_message(&mut self, source: &Source, state: SourceState, message: crate::i18n::Message) {
        self.record(SourceOutcome {
            source: source.name.into(),
            state,
            reason: Some(message.render()),
            reason_message: message.is_translatable().then_some(message),
        });
    }

    /// Finish decoding a successful read. A previous failure or partial result is never cleared by this update.
    pub fn completed(&mut self, source: &Source, rows: usize) {
        let state = if rows == 0 { SourceState::Empty } else { SourceState::Collected };
        if let Some(existing) = self.sources.iter_mut().find(|s| same_source(&s.source, source.name))
            && matches!(existing.state, SourceState::Collected | SourceState::Empty)
        {
            existing.state = state;
        } else {
            self.recorded(source, state, None);
        }
    }

    fn record(&mut self, outcome: SourceOutcome) {
        if let Some(existing) = self.sources.iter_mut().find(|s| same_source(&s.source, &outcome.source)) {
            existing.merge(outcome);
        } else {
            self.sources.push(outcome);
        }
    }

    /// Some objects or measurements could not be read, though the source delivered useful data.
    pub fn partial(&mut self, source: &Source, reason: impl Into<crate::i18n::Message>) {
        let reason = reason.into();
        self.note(source.name, reason.clone());
        self.recorded_message(source, SourceState::Partial, reason);
    }

    /// The engine or configuration does not have this source. This is different from an unreadable source.
    pub fn not_applicable(&mut self, source: &Source, reason: impl Into<crate::i18n::Message>) {
        let reason = reason.into();
        if let Some(existing) = self.sources.iter_mut().find(|s| same_source(&s.source, source.name))
            && matches!(existing.state, SourceState::Collected | SourceState::Empty)
        {
            // A successful configuration probe may establish that the source does not apply.
            existing.state = SourceState::NotApplicable;
            existing.reason = Some(reason.render());
            existing.reason_message = reason.is_translatable().then_some(reason);
        } else {
            self.recorded_message(source, SourceState::NotApplicable, reason);
        }
    }

    /// Source outcomes, including successful reads. `finish` retains its original public tuple contract.
    pub fn outcomes(&self) -> Vec<SourceOutcome> {
        self.sources.clone()
    }

    /// Attach a later collector's evidence and capability changes without losing the main collector's account.
    pub fn append_to(mut self, snapshot: &mut crate::Snapshot) {
        for outcome in std::mem::take(&mut self.sources) {
            if let Some(existing) = snapshot.sources.iter_mut().find(|s| same_source(&s.source, &outcome.source)) {
                existing.merge(outcome);
            } else {
                snapshot.sources.push(outcome);
            }
        }
        let (have, missing, notes) = self.finish();
        for capability in have {
            if !snapshot.server.has(capability) {
                snapshot.server.capabilities.push(capability);
            }
            snapshot.server.missing.retain(|m| m.capability != capability);
        }
        for missing in missing {
            snapshot.server.capabilities.retain(|c| *c != missing.capability);
            snapshot.server.missing.retain(|m| m.capability != missing.capability);
            snapshot.server.missing.push(missing);
        }
        snapshot.server.capabilities.sort();
        snapshot.notes.extend(notes);
    }

    /// A source delivered `capability`.
    pub fn delivered(&mut self, capability: Capability) {
        if !self.have.contains(&capability) {
            self.have.push(capability);
        }
    }

    /// `capability` isn't available: the setting is off, the edition lacks it, the login can't read it. The first
    /// reason given for a capability is kept.
    pub fn lacking(&mut self, capability: Capability, reason: impl Into<crate::i18n::Message>, fix: Option<String>) {
        let mut missing = MissingCapability::new(capability, reason);
        if let Some(fix) = fix {
            missing = missing.with_fix(fix);
        }
        self.missing_capability(missing);
    }

    /// Keep the provider's structured repair with its explanation. The first diagnosis owns the repair.
    pub fn missing_capability(&mut self, missing: MissingCapability) {
        if !self.missing.iter().any(|m| m.capability == missing.capability) {
            self.missing.push(missing);
        }
    }

    /// A capability the probe expected that a source then couldn't deliver: no longer available, and missing with why.
    /// Unlike [`Coverage::lacking`], this overrides an earlier `delivered`, and its reason replaces one the probe gave:
    /// the source knows more about why.
    pub fn lost(&mut self, capability: Capability, reason: impl Into<crate::i18n::Message>, fix: Option<String>) {
        let mut missing = MissingCapability::new(capability, reason);
        if let Some(fix) = fix {
            missing = missing.with_fix(fix);
        }
        self.lost_capability(missing);
    }

    pub fn lost_capability(&mut self, missing: MissingCapability) {
        self.have.retain(|c| *c != missing.capability);
        self.missing.retain(|m| m.capability != missing.capability);
        self.missing_capability(missing);
    }

    /// A capability the probe expected that turned out to hold nothing (no statement could be planned, nothing was timed),
    /// without a missing reason of its own: the scan's notes, or the views that read it, say why.
    pub fn withdraw(&mut self, capability: Capability) {
        self.have.retain(|c| *c != capability);
    }

    /// Something the reader of the report should know about what `source` read, without a capability at stake.
    pub fn note(&mut self, source: &str, message: impl Into<crate::i18n::Message>) {
        self.notes.push(CollectorNote::recorded(source, message.into()));
    }

    /// `source` read part of what it should about one table (`schema.table`): a note about that table, and the
    /// source partial.
    pub fn partial_table(&mut self, source: &Source, table: &str, reason: impl Into<crate::i18n::Message>) {
        let reason = reason.into();
        let mut note = CollectorNote::recorded(source.name, reason.clone());
        note.table = Some(table.into());
        self.notes.push(note);
        self.recorded_message(source, SourceState::Partial, reason);
    }

    /// Reading `source` failed: a note with the error, and `capability` locked with why. A permission error says the
    /// login lacks the permission, and `grant` (the statement that gives it) becomes the fix.
    pub fn failed(&mut self, source: &str, capability: Capability, error: &Error, grant: Option<String>) {
        self.record(SourceOutcome {
            reason_message: error.message_source().is_translatable().then(|| error.message_source()),
            source: source.into(),
            state: SourceState::Unavailable,
            reason: Some(error.message.clone()),
        });
        self.note(source, error.message_source());
        let denied = error.code == ErrorCode::PermissionDenied;
        let reason = if denied {
            crate::message!("model.source.no_permission")
        } else {
            crate::message!("model.source.failed", error = error.message_source())
        };
        let fix = if denied {
            grant.map(crate::i18n::Message::from).or_else(|| error.hint_source())
        } else {
            error.hint_source()
        };
        let mut missing = MissingCapability::new(capability, reason);
        if let Some(fix) = fix {
            missing = missing.with_fix(fix);
        }
        self.missing_capability(missing);
    }

    /// What reading `source` gave: its value, or `None` with the failure recorded. A failing source never fails the scan:
    /// a note with the error, and each capability it delivers lost with the source's own reason (a permission error
    /// says the login lacks the permission, and the error's hint, the statement that grants it, is the fix).
    pub fn read<T>(&mut self, source: &Source, result: Result<T, Error>) -> Option<T> {
        match result {
            Ok(value) => {
                self.recorded(source, SourceState::Collected, None);
                Some(value)
            }
            Err(error) => {
                self.source_failed(source, &error);
                None
            }
        }
    }

    /// A successful empty catalog is evidence too; a failed catalog is never an empty one.
    pub fn read_rows<T>(&mut self, source: &Source, result: Result<Vec<T>, Error>) -> Option<Vec<T>> {
        match result {
            Ok(rows) => {
                self.recorded(source, if rows.is_empty() { SourceState::Empty } else { SourceState::Collected }, None);
                Some(rows)
            }
            Err(error) => {
                self.source_failed(source, &error);
                None
            }
        }
    }

    /// Record that reading `source` failed with `error` (see [`Coverage::read`]), for a source read in parts.
    pub fn source_failed(&mut self, source: &Source, error: &Error) {
        self.recorded_message(source, SourceState::Unavailable, error.message_source());
        self.note(source.name, error.message_source());
        let reason = if error.code == ErrorCode::PermissionDenied {
            crate::message!(
                "model.source.no_permission_to_read",
                reads = crate::i18n::Message::catalog(source.reads, &[])
            )
        } else {
            crate::message!(
                "model.source.reading_failed",
                reads = crate::i18n::Message::catalog(source.reads, &[]),
                error = error.message_source()
            )
        };
        for capability in source.delivers {
            let mut missing = MissingCapability::new(*capability, reason.clone());
            if let Some(hint) = error.hint_source() {
                missing = missing.with_fix(hint);
            }
            self.lost_capability(missing);
        }
    }

    /// A failed shared connection prevents every source assigned to it from being read.
    pub fn sources_failed(&mut self, sources: &[Source], error: &Error) {
        for source in sources {
            self.source_failed(source, error);
        }
    }

    /// Take in what another part of the scan found (sources read on another connection, concurrently).
    pub fn merge(&mut self, other: Coverage) {
        for outcome in other.sources {
            self.record(outcome);
        }
        for capability in other.have {
            self.delivered(capability);
        }
        for m in other.missing {
            self.missing_capability(m);
        }
        self.notes.extend(other.notes);
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.have.contains(&capability)
    }

    /// The account for the snapshot: what was delivered (sorted), what wasn't (a capability one source delivered isn't
    /// missing because another couldn't), and the notes.
    pub fn finish(mut self) -> (Vec<Capability>, Vec<MissingCapability>, Vec<CollectorNote>) {
        self.missing.retain(|m| !self.have.contains(&m.capability));
        self.have.sort();
        (self.have, self.missing, self.notes)
    }
}

impl crate::Snapshot {
    pub fn source_outcome(&self, name: &str) -> Option<&SourceOutcome> {
        self.sources.iter().find(|s| same_source(&s.source, name))
    }

    /// Why one of these sources didn't deliver in full: its recorded outcome, partial or unavailable. Notes are prose,
    /// never read as a failure; a source the scan didn't record has no problem to report (and no clean result either).
    pub fn source_problem(&self, names: &[&str]) -> Option<String> {
        names.iter().find_map(|name| {
            let outcome = self.source_outcome(name)?;
            matches!(outcome.state, SourceState::Partial | SourceState::Unavailable)
                .then(|| outcome.reason.clone().unwrap_or_else(|| format!("{name} was not completely collected.")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_requested_shared_details_stay_unavailable_without_withdrawing_other_evidence() {
        for engine in crate::Engine::ALL.iter().copied() {
            let mut coverage = Coverage::with(vec![Capability::TableStats], vec![]);
            coverage.shared_resource_details_unavailable(engine);
            let outcomes = coverage.outcomes();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].source, source_id::SHARED_RESOURCE_DETAILS);
            assert_eq!(outcomes[0].state, SourceState::Unavailable);
            assert!(outcomes[0].reason_message.as_ref().is_some_and(crate::i18n::Message::is_translatable));
            let (have, missing, notes) = coverage.finish();
            assert!(have.contains(&Capability::TableStats));
            assert!(missing.is_empty());
            assert_eq!(notes.len(), 1);
        }
    }

    #[test]
    fn unavailable_request_memory_does_not_withdraw_other_capabilities_or_claim_empty_measurements() {
        let mut coverage = Coverage::with(vec![Capability::TableStats], vec![]);
        coverage.request_memory_unavailable(crate::Engine::Postgres);
        let outcomes = coverage.outcomes();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].source, source_id::REQUEST_MEMORY_DETAILS);
        assert_eq!(outcomes[0].state, SourceState::Unavailable);
        assert!(outcomes[0].reason_message.as_ref().is_some_and(crate::i18n::Message::is_translatable));
        let (have, missing, notes) = coverage.finish();
        assert_eq!(have, [Capability::TableStats]);
        assert!(missing.is_empty());
        assert_eq!(notes.len(), 1);
    }

    const INDEXES: Source = Source {
        name: "indexes",
        reads: "index statistics",
        delivers: &[Capability::IndexUsage, Capability::TableStats],
    };

    #[test]
    fn merging_collection_parts_preserves_the_first_structured_repair() {
        let mut first = Coverage::default();
        first.missing_capability(MissingCapability::sql(
            Capability::QueryStore,
            "Une explication.",
            "ALTER DATABASE CURRENT SET QUERY_STORE = ON;",
        ));
        let mut later = Coverage::default();
        later.lacking(Capability::QueryStore, "Another diagnosis", None);
        let mut combined = Coverage::default();
        combined.merge(first);
        combined.merge(later);
        let (_, missing, _) = combined.finish();
        assert_eq!(missing[0].reason, "Une explication.");
        assert_eq!(missing[0].action.as_ref().unwrap().sql, "ALTER DATABASE CURRENT SET QUERY_STORE = ON;");
    }

    #[test]
    fn partial_table_notes_and_merged_reasons_render_from_their_recorded_arguments() {
        use crate::i18n::scope;
        let first = crate::message!(
            "provider.sqlite.catalog.columns_unavailable",
            table = "docs",
            error = "no such module: fts9"
        );
        let second = crate::message!("provider.sqlite.settings.partial");
        let mut coverage = Coverage::default();
        scope("fr", || {
            coverage.partial_table(&INDEXES, "main.docs", first.clone());
            let mut later = Coverage::default();
            later.partial(&INDEXES, second.clone());
            coverage.merge(later);
            coverage.completed(&INDEXES, 5);
        });
        let outcomes = coverage.outcomes();
        let (_, _, notes) = coverage.finish();
        for language in ["en", "de", "fr", "es"] {
            scope(language, || {
                let mut outcome = outcomes[0].clone();
                outcome.refresh_language();
                assert_eq!(outcome.state, SourceState::Partial);
                assert_eq!(outcome.reason, Some(format!("{} {}", first.render(), second.render())));
                let mut note = notes[0].clone();
                note.refresh_language();
                assert_eq!(note.message, first.render());
                assert_eq!(note.table.as_deref(), Some("main.docs"));
            });
        }
    }

    #[test]
    fn source_failures_keep_catalog_errors_and_hints_for_the_reader() {
        use crate::i18n::{MessageValue, scope};
        let message = crate::message!("provider.mysql.workload.digests_full", time = MessageValue::Duration(1500.0));
        let hint = crate::message!("provider.sqlite.error.interrupted.hint");
        let mut coverage = Coverage::with(vec![Capability::IndexUsage], vec![]);
        scope("fr", || {
            let error = Error::internal(message.clone()).with_hint(hint.clone());
            let json = serde_json::to_value(&error).unwrap();
            assert_eq!(json.as_object().unwrap().len(), 3, "catalog references stay out of the error wire format");
            coverage.source_failed(&INDEXES, &error);
        });
        let mut outcomes = coverage.outcomes();
        let (have, mut missing, mut notes) = coverage.finish();
        assert!(!have.contains(&Capability::IndexUsage));
        for language in ["en", "de", "fr", "es"] {
            scope(language, || {
                outcomes[0].refresh_language();
                notes[0].refresh_language();
                assert_eq!(outcomes[0].reason.as_deref(), Some(message.render().as_str()));
                assert_eq!(notes[0].message, message.render());
                for missing in &mut missing {
                    missing.render_messages();
                    assert_eq!(missing.fix.as_deref(), Some(hint.render().as_str()));
                    assert!(missing.reason.contains(&message.render()));
                }
            });
        }
    }

    #[test]
    fn empty_success_and_failure_have_different_outcomes() {
        let mut c = Coverage::default();
        assert_eq!(c.read_rows::<u8>(&INDEXES, Ok(vec![])), Some(vec![]));
        assert_eq!(c.outcomes()[0].state, SourceState::Empty);
        c.note(INDEXES.name, "This source has no visible rows.");
        assert_eq!(c.outcomes()[0].state, SourceState::Empty);
        c.source_failed(&INDEXES, &Error::internal("timeout"));
        assert_eq!(c.outcomes()[0].state, SourceState::Partial);
        c.completed(&INDEXES, 5);
        assert_eq!(c.outcomes()[0].state, SourceState::Partial, "a later successful part must not clear the failure");
    }

    #[test]
    fn parallel_source_parts_merge_conservatively_in_either_order() {
        for reverse in [false, true] {
            let mut a = Coverage::default();
            a.read_rows(&INDEXES, Ok(vec![1]));
            let mut b = Coverage::default();
            b.source_failed(&INDEXES, &Error::internal("timeout"));
            let combined = if reverse {
                b.merge(a);
                b
            } else {
                a.merge(b);
                a
            };
            assert_eq!(combined.outcomes().len(), 1);
            assert_eq!(combined.outcomes()[0].state, SourceState::Partial);
            assert_eq!(combined.outcomes()[0].reason.as_deref(), Some("timeout"));
        }
    }

    #[test]
    fn configuration_can_establish_inapplicability_but_a_failure_cannot() {
        let mut c = Coverage::default();
        c.read(&INDEXES, Ok(false));
        c.not_applicable(&INDEXES, "Feature disabled");
        assert_eq!(c.outcomes()[0].state, SourceState::NotApplicable);
        let mut failed = Coverage::default();
        failed.source_failed(&INDEXES, &Error::internal("permission denied"));
        failed.not_applicable(&INDEXES, "Feature disabled");
        assert_eq!(failed.outcomes()[0].state, SourceState::Partial);
    }

    #[test]
    fn detail_collectors_retain_their_source_account() {
        let mut s: crate::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        s.sources.retain(|o| o.source != "indexes");
        let mut main = Coverage::default();
        main.read_rows(&INDEXES, Ok(vec![1]));
        main.append_to(&mut s);
        let mut detail = Coverage::default();
        detail.source_failed(&INDEXES, &Error::internal("details timed out"));
        detail.append_to(&mut s);
        assert_eq!(s.source_outcome("indexes").unwrap().state, SourceState::Partial);
        assert!(!s.server.has(Capability::IndexUsage));
        assert!(!s.server.has(Capability::TableStats));
        assert!(
            s.server
                .missing
                .iter()
                .any(|m| m.capability == Capability::IndexUsage && m.reason.contains("details timed out"))
        );
        assert_eq!(s.notes.len(), 1);
        let saved = serde_json::to_string(&s).unwrap();
        let loaded: crate::Snapshot = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded.sources, s.sources);
        assert_eq!(loaded.source_problem(&["indexes"]).as_deref(), Some("details timed out"));
    }

    // One rule for every engine: a source that fails is a note and its capabilities locked with why, even ones the
    // probe expected, and the scan goes on.
    #[test]
    fn a_failed_source_is_a_note_and_its_capabilities_locked() {
        let mut c = Coverage::with(vec![Capability::IndexUsage], Vec::new());
        let read: Option<Vec<u8>> = c.read(&INDEXES, Err(Error::internal("statement timeout")));
        assert!(read.is_none());
        assert_eq!(c.read(&INDEXES, Ok(3)), Some(3));
        let (have, missing, notes) = c.finish();
        assert!(have.is_empty());
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].reason, "Reading index statistics failed: statement timeout");
        assert_eq!(notes.len(), 1);
        // Denied: the login lacks the permission, and the grant is the fix.
        let denied = Error::new(ErrorCode::PermissionDenied, "permission denied for pg_stat_user_indexes")
            .with_hint("GRANT pg_read_all_stats TO app;");
        let mut c = Coverage::default();
        c.source_failed(&INDEXES, &denied);
        let (_, missing, _) = c.finish();
        assert_eq!(missing[0].reason, "This login lacks the permission to read index statistics.");
        assert_eq!(missing[0].fix.as_deref(), Some("GRANT pg_read_all_stats TO app;"));
    }

    #[test]
    fn a_capability_the_probe_expected_can_be_lost() {
        let mut c = Coverage::with(vec![Capability::TableStats, Capability::IndexUsage], Vec::new());
        c.lost(Capability::IndexUsage, "Reading it failed: timeout", None);
        let (have, missing, _) = c.finish();
        assert_eq!(have, vec![Capability::TableStats]);
        assert_eq!(missing[0].capability, Capability::IndexUsage);
    }

    #[test]
    fn a_failed_source_locks_its_capability_unless_another_delivered_it() {
        let mut workload = Coverage::default();
        workload.delivered(Capability::QueryStore);
        let mut schema = Coverage::default();
        let denied = Error::new(ErrorCode::PermissionDenied, "VIEW DATABASE STATE permission denied");
        schema.failed("index usage", Capability::IndexUsage, &denied, Some("GRANT VIEW DATABASE STATE TO app;".into()));
        schema.failed("query store", Capability::QueryStore, &Error::internal("timeout"), None);
        workload.merge(schema);
        let (have, missing, notes) = workload.finish();
        assert_eq!(have, vec![Capability::QueryStore]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].capability, Capability::IndexUsage);
        assert_eq!(missing[0].reason, "This login lacks the permission to read it.");
        assert_eq!(missing[0].fix.as_deref(), Some("GRANT VIEW DATABASE STATE TO app;"));
        assert_eq!(notes.len(), 2);
    }
}
