//! Retained Query Store compilation totals have their own lifetime, independent of runtime intervals.
use super::*;
use std::collections::BTreeSet;
mod comparison;
pub use comparison::{CompilationBaseline, CompilationCounterPoint, MAX_COMPILATION_BASELINE_BYTES};
mod timings;
pub use timings::{MssqlCompilationStage, MssqlCompilationTimings};

pub const MAX_QUERY_COMPILATION_OBSERVATIONS: usize = 10_000;

/// A read of retained query compilation entries. Acquisition bounds do not replace the entries' native lifetimes.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilationSnapshot {
    pub source: String,
    /// Query Store's native logical database namespace and acquisition clocks, when the held reader acquired them.
    /// Client acquisition bounds below never establish this authority or a physical compiler-memory owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<NativeReadContext>,
    pub observed_from: String,
    pub observed_until: String,
    pub observations: Vec<OperationalObservation>,
}

impl CompilationSnapshot {
    pub fn matches_database_namespace(&self, database: &crate::DatabaseIdentity) -> bool {
        database.incarnation.is_some()
            && self
                .context
                .as_ref()
                .and_then(compilation_namespace)
                .zip(database.telemetry_id().ok())
                .is_some_and(|(observed, current)| observed == current)
    }

    pub fn fits_database(&self, database: &crate::DatabaseIdentity) -> bool {
        self.context.as_ref().is_none_or(|context| native_context_fits_database(context, database))
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.context.as_ref().is_none_or(|context| {
                context.is_valid()
                    && context.target.database().is_some_and(|database| database.engine == crate::Engine::Mssql)
                    && context.partition == SourcePartition::Target
            })
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && self.observations.len() <= MAX_QUERY_COMPILATION_OBSERVATIONS
            && self.observations.iter().all(|row| {
                matches!(&row.fact, OperationalFact::MssqlQueryCompilation(_))
                    && row.is_valid()
                    && row.source == self.source
                    && keys.insert(&row.key)
            })
    }
}

fn native_context_fits_database(context: &NativeReadContext, database: &crate::DatabaseIdentity) -> bool {
    context.target.database().is_some_and(|observed| {
        if observed.incarnation.is_some() && database.incarnation.is_some() {
            observed.telemetry_id().ok().zip(database.telemetry_id().ok()).is_some_and(|(left, right)| left == right)
        } else {
            observed.comparison(database) == crate::Comparison::Compatible
        }
    })
}

/// This source's durable namespace is Query Store's logical database. Neither a compute epoch, endpoint alias
/// nor a selected runtime replica group can substitute for a missing native database incarnation.
pub fn compilation_namespace(context: &NativeReadContext) -> Option<String> {
    context.is_valid().then(|| compilation_database_namespace(&context.target, &context.partition)).flatten()
}

/// Query Store's logical namespace needs no invented acquisition context from a runtime interval.
pub fn compilation_database_namespace(target: &CollectionTarget, partition: &SourcePartition) -> Option<String> {
    let database = target.database()?;
    (*partition == SourcePartition::Target
        && database.engine == crate::Engine::Mssql
        && database.incarnation.as_ref().is_some_and(|id| !id.is_empty()))
    .then(|| target.telemetry_id().ok())
    .flatten()
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlQueryCompilation {
    /// Native references are scoped by the capture's database, not a different profile's definition ID.
    pub identity: NativeQueryIdentity,
    /// Complete bounded original definition, qualified against this exact query incarnation. It may have been
    /// read before these counters; availability is independent and a prefix cannot establish statement policy.
    pub statement: DefinitionPayload,
    pub origin: StatementOrigin,
    pub last_compile_at: String,
    pub compilations: String,
    pub average_duration_us: f64,
    /// Independent native last/stage statistics, with no synthetic total CPU or simultaneous owner.
    pub timings: MssqlCompilationTimings,
    pub memory: Option<crate::CompilationMemory>,
    /// The native clock comparison can be negative after clock skew; it is not a measured duration.
    pub seconds_since_first_compile: i64,
    pub reset: crate::ResetMarker,
}
impl MssqlQueryCompilation {
    /// The initial compile belongs to the native query incarnation; it is not duplicated as a second lifetime.
    pub fn first_compile_at(&self) -> Result<String> {
        let value = self
            .identity
            .qualifiers
            .get("initial_compile_at")
            .filter(|value| value.len() <= 64)
            .ok_or_else(|| CollectionStop::InvalidPage.error())?;
        canonical_time(value)
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Self::identity_key(&self.identity)
    }

    /// A compilation counter belongs to the complete native query incarnation, including its initial compile.
    pub fn identity_key(identity: &NativeQueryIdentity) -> Result<String> {
        if !valid_identity(identity) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut identity = identity.clone();
        let first = canonical_time(
            identity.qualifiers.get("initial_compile_at").ok_or_else(|| CollectionStop::InvalidPage.error())?,
        )?;
        identity.qualifiers.insert("initial_compile_at".into(), first);
        serde_json::to_vec(&identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        valid_identity(&self.identity)
            && self.statement.is_valid()
            && valid_counter(&self.compilations)
            && self.last_compile_at.len() <= 64
            && canonical_time(&self.last_compile_at).is_ok()
            && self.average_duration_us.is_finite()
            && self.average_duration_us >= 0.0
            && self.timings.is_valid()
            && self.memory.as_ref().is_none_or(|memory| memory.is_valid() && memory.is_measured())
            && valid_reset(&self.reset)
    }
}

fn valid_identity(identity: &NativeQueryIdentity) -> bool {
    identity.is_valid()
        && valid_counter(&identity.query_id)
        && identity.qualifiers.get("context_settings_id").is_some_and(|value| valid_counter(value))
        && identity.qualifiers.get("object_id").is_some_and(|value| value.parse::<i64>().is_ok())
        && identity
            .qualifiers
            .get("query_hash")
            .is_some_and(|value| value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && identity
            .qualifiers
            .get("initial_compile_at")
            .is_some_and(|value| value.len() <= 64 && canonical_time(value).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn observation() -> MssqlQueryCompilation {
        MssqlQueryCompilation {
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            origin: StatementOrigin::Unknown,
            identity: NativeQueryIdentity {
                query_id: "3".into(),
                qualifiers: [
                    ("context_settings_id".into(), "5".into()),
                    ("object_id".into(), "0".into()),
                    ("query_hash".into(), "0123456789ABCDEF".into()),
                    ("initial_compile_at".into(), "2020-01-01T06:00:00.1533333Z".into()),
                ]
                .into(),
            },
            last_compile_at: "2020-01-01T06:01:00Z".into(),
            compilations: "9007199254740993".into(),
            average_duration_us: 1500.5,
            timings: MssqlCompilationTimings::default(),
            memory: Some(crate::CompilationMemory {
                average_kb: Some(4096.5),
                last_kb: Some(8192.0),
                maximum_kb: Some(16384.0),
            }),
            seconds_since_first_compile: -1,
            reset: crate::ResetMarker::Unknown,
        }
    }

    #[test]
    fn native_lifetime_identity_preserves_exact_counts_and_clock_skew() {
        let value = observation();
        assert!(value.is_valid());
        let mut changed = value.clone();
        changed.compilations = "9007199254740994".into();
        assert_eq!(value.key().unwrap(), changed.key().unwrap());
        changed.memory.as_mut().unwrap().maximum_kb = Some(32768.0);
        assert_eq!(value.key().unwrap(), changed.key().unwrap());
        changed.identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T06:00:00.153333300+00:00".into());
        assert_eq!(value.key().unwrap(), changed.key().unwrap());
        changed.identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T06:00:01Z".into());
        assert_ne!(value.key().unwrap(), changed.key().unwrap());
        changed.average_duration_us = f64::NAN;
        assert!(!changed.is_valid());
    }

    #[test]
    fn complete_native_query_incarnation_distinguishes_reused_ids_and_compilation_contexts() {
        let value = observation();
        let original = value.key().unwrap();
        for (qualifier, changed) in [
            ("context_settings_id", "6"),
            ("object_id", "1999"),
            ("query_hash", "FEDCBA9876543210"),
            ("initial_compile_at", "2020-01-01T06:00:01Z"),
        ] {
            let mut other = value.clone();
            other.identity.qualifiers.insert(qualifier.into(), changed.into());
            assert!(other.is_valid(), "{qualifier}");
            assert_ne!(other.key().unwrap(), original, "{qualifier} is part of the native query identity");
        }
        let mut other = value.clone();
        other.identity.query_id = "4".into();
        assert_ne!(other.key().unwrap(), original);
        other = value;
        other.identity.qualifiers.insert("object_id".into(), "-2147483648".into());
        assert!(other.is_valid(), "native signed object identifiers must not be discarded");
    }

    #[test]
    fn missing_or_malformed_native_qualifiers_cannot_be_published_with_a_plausible_query_id() {
        for (qualifier, invalid) in [
            ("context_settings_id", "not-an-id"),
            ("object_id", "18446744073709551615"),
            ("query_hash", "short"),
            ("initial_compile_at", "2020-01-01T06:00:00"),
        ] {
            let mut row = observation();
            row.identity.qualifiers.remove(qualifier);
            assert!(!row.is_valid(), "missing {qualifier}");
            assert!(row.key().is_err());
            row.identity.qualifiers.insert(qualifier.into(), invalid.into());
            assert!(!row.is_valid(), "malformed {qualifier}");
            assert!(row.key().is_err());
        }
    }

    #[test]
    fn serialization_has_one_native_query_identity_and_one_initial_compilation_clock() {
        let row = observation();
        let json = serde_json::to_value(&row).unwrap();
        assert!(json.get("query_id").is_none());
        assert!(json.get("first_compile_at").is_none());
        assert_eq!(json["identity"]["query_id"], "3");
        assert_eq!(json["identity"]["qualifiers"]["initial_compile_at"], "2020-01-01T06:00:00.1533333Z");
        let restored: MssqlQueryCompilation = serde_json::from_value(json).unwrap();
        assert_eq!(restored, row);
        assert_eq!(restored.first_compile_at().unwrap(), "2020-01-01T06:00:00.153333300Z");
        assert_eq!(restored.key().unwrap(), row.key().unwrap());
    }

    #[test]
    fn compilation_statement_has_one_payload_path_and_redaction_preserves_native_origin_and_identity() {
        let mut native = observation();
        native.origin = StatementOrigin::Application;
        native.statement = DefinitionPayload::sql(
            "SELECT id FROM dbo.orders WHERE id=912345 OPTION (RECOMPILE)",
            crate::Engine::Mssql,
        );
        let key = native.key().unwrap();
        let mut fact = OperationalFact::MssqlQueryCompilation(Box::new(native));
        fact.redact(crate::Engine::Mssql);
        let record = TelemetryRecord::Operational(OperationalObservation {
            source: "query-compilation".into(),
            key: key.clone(),
            fact,
        });
        let fields = record.definition_payloads().collect::<Vec<_>>();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].pointer, "/record/data/fact/data/statement");
        let DefinitionPayload::Available { text, .. } = fields[0].payload else { panic!() };
        assert!(!text.contains("912345"));
        assert!(text.contains("OPTION (RECOMPILE)"));
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlQueryCompilation(native),
            ..
        }) = &record
        else {
            panic!()
        };
        assert_eq!(native.origin, StatementOrigin::Application);
        assert_eq!(native.key().unwrap(), key);
        let mut value = serde_json::to_value(native).unwrap();
        value["statement"]["sha256"] = serde_json::json!(digest(b"wrong-payload"));
        assert!(!serde_json::from_value::<MssqlQueryCompilation>(value).unwrap().is_valid());
    }

    fn captured() -> CompilationSnapshot {
        let native = observation();
        let source = "query compilations";
        CompilationSnapshot {
            context: None,
            source: source.into(),
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            observations: vec![OperationalObservation {
                source: source.into(),
                key: native.key().unwrap(),
                fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
            }],
        }
    }

    #[test]
    fn retained_compilation_lifetime_stays_independent_of_the_snapshot_read_bounds() {
        let value = captured();
        assert!(value.is_valid(), "retained compilation clocks precede the acquisition interval");
        let restored: CompilationSnapshot = serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
        assert_eq!(restored, value);
        let mut skewed = value.clone();
        let OperationalFact::MssqlQueryCompilation(native) = &mut skewed.observations[0].fact else { panic!() };
        native.last_compile_at = "2020-01-01T12:00:02Z".into();
        native.seconds_since_first_compile = -1;
        assert!(skewed.is_valid(), "native clock skew remains recorded, not replaced by the collector clock");
        let operations = crate::operations::OperationalStats { query_compilation: Some(value), ..Default::default() };
        assert!(serde_json::to_value(&operations).unwrap().get("query_compilation").is_some());
        assert!(serde_json::to_value(operations.counters_only()).unwrap().get("query_compilation").is_none());
    }

    #[test]
    fn compilation_snapshots_reject_foreign_sources_keys_and_non_compilation_facts() {
        let value = captured();
        let mut changed = value.clone();
        changed.observations[0].source = "different source".into();
        assert!(changed.observations[0].is_valid());
        assert!(!changed.is_valid());
        changed = value.clone();
        changed.observations[0].key = digest(b"foreign native key");
        assert!(!changed.is_valid());
        changed = value.clone();
        let mut later = changed.observations[0].clone();
        let OperationalFact::MssqlQueryCompilation(native) = &mut later.fact else { panic!() };
        native.compilations = "9007199254740994".into();
        assert!(later.is_valid());
        changed.observations.push(later);
        assert!(!changed.is_valid(), "changing measurements does not create another native query incarnation");
        let counters = NativeCounterSet {
            identity: NativeIdentity::new("compiler gateway"),
            scope: crate::MeasurementScope::Server,
            counters: vec![NativeCounterSample::gauge(
                "waiting",
                2,
                CounterUnit::Count,
                crate::MeasurementScope::Server,
                "gateway",
                "waiter_count",
            )],
            properties: Default::default(),
        };
        changed = value;
        changed.observations[0].key = counters.key().unwrap();
        changed.observations[0].fact = OperationalFact::CounterSet(Box::new(counters));
        assert!(changed.observations[0].is_valid());
        assert!(!changed.is_valid(), "shared compiler admission is a different native fact");
    }

    #[test]
    fn compilation_snapshot_bounds_and_population_are_bounded_without_inventing_empty_history() {
        let value = captured();
        for (from, until) in
            [("2020-01-01T12:00:00", "2020-01-01T12:00:01Z"), ("2020-01-01T12:00:02Z", "2020-01-01T12:00:01Z")]
        {
            let mut changed = value.clone();
            changed.observed_from = from.into();
            changed.observed_until = until.into();
            assert!(!changed.is_valid());
        }
        let mut changed = value.clone();
        changed.observations = vec![value.observations[0].clone(); MAX_QUERY_COMPILATION_OBSERVATIONS + 1];
        assert!(!changed.is_valid());
        changed.observations.clear();
        assert!(changed.is_valid(), "source coverage determines what an empty successful read means");
        changed.source.clear();
        assert!(!changed.is_valid());
    }
}
