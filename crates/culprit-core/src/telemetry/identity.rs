//! Export identity excludes delivery attempts, redaction, endpoint aliases and observation timestamps.
use super::{
    CaptureIdentity, CollectionDescription, NativeRecordIdentity, SourceObservation, TelemetryRecord, TelemetryStream,
    valid_key,
};
use crate::{DatabaseIdentity, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

pub const EXPORT_FORMAT_VERSION: u32 = 17;
pub const REPRESENTATION_VERSION: u32 = 2;

impl DatabaseIdentity {
    /// Stable resource identity for telemetry destinations; endpoints remain evidence when native identity is absent.
    pub fn telemetry_id(&self) -> Result<String> {
        scoped_id("database-resource-v1", self, "", &())
    }
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}
/// Canonical lowercase SHA-256 identifiers used by native export and delivery contracts.
pub fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl CollectionDescription {
    /// Stable grouping for the latest complete observation set. Captured scopes retain native generations.
    pub fn logical_source_id(&self) -> Result<String> {
        target_scoped_id("logical-source-v1", &self.target, &self.source, &())
    }
    pub fn scoped_id(&self, domain: &str, identity: &impl Serialize) -> Result<String> {
        match &self.observation {
            SourceObservation::Activity(snapshot) => target_scoped_id(
                domain,
                &self.target,
                &self.source,
                &(&snapshot.epoch, snapshot.population, snapshot.scope, identity),
            ),
            SourceObservation::FocusedPageWait { result, .. } => {
                target_scoped_id(domain, &self.target, &self.source, &(result.focus.key()?, identity))
            }
            SourceObservation::Diagnostics(description) => {
                target_scoped_id(domain, &self.target, &self.source, &(description.work_key()?, identity))
            }
            _ => target_scoped_id(domain, &self.target, &self.source, identity),
        }
    }
    pub fn captured_range(&self) -> Result<super::CapturedRange> {
        Ok(super::CapturedRange {
            scope_id: self.scoped_id("outbox-source-v1", &())?,
            range: self.observation.range()?.canonical()?,
        })
    }
}
impl super::CapturedRange {
    /// Native interval discovery and capture must use the same database/source namespace.
    pub fn native_interval(
        database: &DatabaseIdentity,
        source: &str,
        interval: &super::TelemetryInterval,
    ) -> Result<Self> {
        Ok(Self {
            scope_id: target_scoped_id(
                "outbox-source-v1",
                &super::CollectionTarget::Database(database.clone()),
                source,
                &(),
            )?,
            range: super::TelemetryRange::NativeInterval { interval: interval.clone() }.canonical()?,
        })
    }
}
fn target_scoped_id(
    domain: &str,
    target: &super::CollectionTarget,
    source: &str,
    identity: &impl Serialize,
) -> Result<String> {
    let bytes = serde_json::to_vec(&(domain, target.telemetry_id()?, source, identity))
        .map_err(|_| super::CollectionStop::InvalidPage.error())?;
    Ok(digest(&bytes))
}
fn scoped_id(domain: &str, database: &DatabaseIdentity, source: &str, identity: &impl Serialize) -> Result<String> {
    // A native server incarnation survives endpoint/alias changes. File IDs are scoped to their filesystem path.
    // Without a global native identity the observed endpoint remains
    // part of the namespace; uncertainty is retained rather than guessed away.
    #[derive(Serialize)]
    #[serde(untagged)]
    enum DatabaseScope<'a> {
        Native { engine: crate::Engine, incarnation: &'a str },
        Endpoint(&'a DatabaseIdentity),
    }
    let database = match database
        .incarnation
        .as_deref()
        .filter(|value| !value.is_empty() && !matches!(database.location, crate::DatabaseLocation::File { .. }))
    {
        Some(incarnation) => DatabaseScope::Native { engine: database.engine, incarnation },
        None => DatabaseScope::Endpoint(database),
    };
    let bytes = serde_json::to_vec(&(domain, database, source, identity))
        .map_err(|_| crate::Error::internal(crate::t!("collection.invalid_source")))?;
    Ok(digest(&bytes))
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ExportedRecord {
    pub format_version: u32,
    pub representation_version: u32,
    pub capture: CaptureIdentity,
    pub batch_id: String,
    /// Absent when the source could not resolve the native definition. Such rows cannot enter trusted totals.
    pub record_id: Option<String>,
    pub record: TelemetryRecord,
}

impl ExportedRecord {
    /// Digest of the canonical exported record, excluding its NDJSON line terminator.
    pub fn payload_sha256(&self) -> Result<String> {
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| super::CollectionStop::InvalidPage.error())
    }

    pub fn new(source: &CollectionDescription, capture: &CaptureIdentity, record: TelemetryRecord) -> Result<Self> {
        let record = record.for_export(source)?;
        let interval = source
            .query_history()
            .map(|history| {
                Ok::<_, crate::Error>((
                    history.interval.native_id.clone(),
                    canonical_time(&history.interval.from)?,
                    canonical_time(&history.interval.until)?,
                ))
            })
            .transpose()?;
        let record_id = match &record {
            TelemetryRecord::QueryDefinition(definition) => Some(definition.id.clone()),
            TelemetryRecord::PlanDefinition(definition) => Some(definition.id.clone()),
            TelemetryRecord::QueryRuntime(row) if row.key.references.is_resolved() => Some(source.scoped_id(
                "runtime-native-row-v2",
                &(interval.as_ref().ok_or_else(|| super::CollectionStop::InvalidPage.error())?, &row.key.native),
            )?),
            TelemetryRecord::QueryWaits(row) if row.key.references.is_resolved() => Some(source.scoped_id(
                "wait-native-row-v2",
                &(
                    interval.as_ref().ok_or_else(|| super::CollectionStop::InvalidPage.error())?,
                    &row.key.native,
                    &row.category,
                ),
            )?),
            TelemetryRecord::ResourceSample(row) => {
                Some(source.scoped_id("resource-sample-v1", &(&row.at, &row.replica_id))?)
            }
            TelemetryRecord::Activity(row) => {
                Some(source.scoped_id("activity-observation-v1", &(&capture.observation_id, &row.key))?)
            }
            TelemetryRecord::Deadlock(row) => {
                Some(source.scoped_id("deadlock-event-v1", &(&row.position, row.graph_ordinal))?)
            }
            TelemetryRecord::DiagnosticCheckpoint(row) => {
                Some(source.scoped_id("diagnostic-checkpoint-v1", &(&capture.observation_id, &row.id))?)
            }
            TelemetryRecord::Operational(row) => {
                Some(source.scoped_id("state-observation-v1", &(&capture.observation_id, &row.source, &row.key))?)
            }
            TelemetryRecord::WaitCounter(row) => {
                Some(source.scoped_id(
                    "wait-counter-observation-v1",
                    &(&capture.observation_id, &row.source, &row.wait_type),
                )?)
            }
            TelemetryRecord::Replication(row) => Some(
                source
                    .scoped_id("replication-observation-v1", &(&capture.observation_id, &row.source, row.fact.key()))?,
            ),
            TelemetryRecord::Setting(row) => Some(source.scoped_id(
                "setting-observation-v1",
                &(&capture.observation_id, &row.source, &row.namespace, &row.name),
            )?),
            TelemetryRecord::Tuning(row) => Some(
                source.scoped_id("tuning-observation-v1", &(&capture.observation_id, &row.source, row.fact.key()))?,
            ),
            TelemetryRecord::ObjectDefinition(row) => {
                Some(source.scoped_id("object-definition-v1", &(&row.source, &row.key, &row.version))?)
            }
            TelemetryRecord::QueryRuntime(_) | TelemetryRecord::QueryWaits(_) => None,
        };
        Ok(Self {
            format_version: EXPORT_FORMAT_VERSION,
            representation_version: REPRESENTATION_VERSION,
            capture: capture.clone(),
            batch_id: batch_id(&capture.observation_id, record.stream())?,
            record_id,
            record,
        })
    }
}

impl NativeRecordIdentity {
    pub fn stream(&self) -> TelemetryStream {
        match self {
            Self::Runtime(_) => TelemetryStream::QueryRuntime,
            Self::Waits { .. } => TelemetryStream::QueryWaits,
            Self::QueryDefinition(_) => TelemetryStream::QueryDefinitions,
            Self::PlanDefinition(_) => TelemetryStream::PlanDefinitions,
            Self::ResourceSample { .. } => TelemetryStream::ResourceSamples,
            Self::Activity(_) => TelemetryStream::ActivityObservations,
            Self::DiagnosticCheckpoint(_) => TelemetryStream::DiagnosticCheckpoints,
            Self::Deadlock { .. } => TelemetryStream::DeadlockEvents,
            Self::Operational { .. } => TelemetryStream::OperationalSamples,
            Self::WaitCounter { .. } => TelemetryStream::WaitCounterSamples,
            Self::Replication { .. } => TelemetryStream::ReplicationSamples,
            Self::Setting { .. } => TelemetryStream::SettingObservations,
            Self::Tuning { .. } => TelemetryStream::TuningObservations,
            Self::ObjectDefinition { .. } => TelemetryStream::ObjectDefinitions,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Runtime(key) => key.is_valid(),
            Self::Waits { key, category } => key.is_valid() && valid_key(category),
            Self::Activity(key) => key.is_valid(),
            Self::QueryDefinition(key) | Self::PlanDefinition(key) | Self::DiagnosticCheckpoint(key) => {
                valid_digest(key)
            }
            Self::ResourceSample { at, replica_id } => {
                valid_utc_instant(at) && replica_id.as_deref().is_none_or(valid_key)
            }
            Self::Deadlock { position, .. } => valid_key(position),
            Self::Operational { source, key }
            | Self::Replication { source, key }
            | Self::Tuning { source, key }
            | Self::ObjectDefinition { source, key } => valid_key(source) && valid_key(key),
            Self::WaitCounter { source, wait_type } => valid_key(source) && valid_key(wait_type),
            Self::Setting { source, namespace, name } => valid_key(source) && valid_key(namespace) && valid_key(name),
        }
    }
}

pub fn batch_id(observation_id: &str, stream: TelemetryStream) -> Result<String> {
    let bytes = serde_json::to_vec(&("capture-batch-v1", observation_id, stream))
        .map_err(|_| crate::Error::internal(crate::t!("collection.invalid_source")))?;
    Ok(digest(&bytes))
}

pub fn canonical_time(value: &str) -> Result<String> {
    parse_utc_instant(value).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

/// Local acquisition bounds qualify observation clocks, never native event times or an atomic source snapshot.
pub struct ObservationBounds {
    from: chrono::DateTime<chrono::Utc>,
    until: chrono::DateTime<chrono::Utc>,
}

impl ObservationBounds {
    pub fn new(from: &str, until: &str) -> Option<Self> {
        let from = parse_utc_instant(from).ok()?;
        let until = parse_utc_instant(until).ok()?;
        (from <= until).then_some(Self { from, until })
    }

    pub fn contains(&self, instant: &str) -> bool {
        parse_utc_instant(instant).is_ok_and(|instant| self.from <= instant && instant <= self.until)
    }
}

pub(super) fn valid_utc_instant(value: &str) -> bool {
    parse_utc_instant(value).is_ok()
}

fn parse_utc_instant(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    const MAX_INSTANT_BYTES: usize = 64;
    const NANOSECOND_DIGITS: usize = 9;
    // Chrono accepts excess fractional digits by truncating them. Never convert a finer native instant to zero,
    // or let normalization erase the original field's length violation.
    if value.len() > MAX_INSTANT_BYTES
        || value.split_once('.').is_some_and(|(_, fraction)| {
            fraction.bytes().take_while(u8::is_ascii_digit).skip(NANOSECOND_DIGITS).any(|digit| digit != b'0')
        })
    {
        return Err(crate::Error::invalid(crate::t!("collection.invalid_source")));
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&chrono::Utc))
        .map_err(|_| crate::Error::invalid(crate::t!("collection.invalid_source")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn database_resource_id_uses_native_incarnation_and_preserves_endpoint_uncertainty() {
        let mut original = DatabaseIdentity::server(crate::Engine::Mssql, "db", "old.invalid", 1433, None);
        let mut alias = DatabaseIdentity::server(crate::Engine::Mssql, "renamed", "new.invalid", 1433, None);
        assert_ne!(original.telemetry_id().unwrap(), alias.telemetry_id().unwrap());
        original.incarnation = Some("native-guid".into());
        alias.incarnation = original.incarnation.clone();
        assert_eq!(original.telemetry_id().unwrap(), alias.telemetry_id().unwrap());
        alias.incarnation = Some("recreated-guid".into());
        assert_ne!(original.telemetry_id().unwrap(), alias.telemetry_id().unwrap());
    }
    #[test]
    fn a_local_file_incarnation_does_not_claim_a_global_server_identity() {
        let mut first = DatabaseIdentity::file("sample", "/first/sample.sqlite".into());
        first.incarnation = Some("local-file-id".into());
        let mut second = DatabaseIdentity::file("sample", "/second/sample.sqlite".into());
        second.incarnation = first.incarnation.clone();
        assert_ne!(first.telemetry_id().unwrap(), second.telemetry_id().unwrap());
        second.location = first.location.clone();
        assert_eq!(first.telemetry_id().unwrap(), second.telemetry_id().unwrap());
    }
    use crate::telemetry::{
        NativeMeasurementKey, NativePlanIdentity, NativeQueryIdentity, SourceContinuity, TelemetryInterval,
        WaitMeasurement,
    };

    fn source() -> CollectionDescription {
        let mut database = DatabaseIdentity::server(crate::Engine::Mssql, "sample", "original.invalid", 1433, None);
        database.incarnation = Some("native-database-guid".into());
        CollectionDescription {
            target: database.into(),
            source: "query_store".into(),
            observation: crate::telemetry::SourceObservation::QueryHistory(crate::telemetry::QueryHistoryDescription {
                waits: None,
                interval: TelemetryInterval {
                    native_id: "7".into(),
                    from: "2020-01-01T06:00:00Z".into(),
                    until: "2020-01-01T07:00:00Z".into(),
                },
                dimensions: [("replica_group_id".into(), vec!["1".into()])].into(),
                native_properties: [
                    ("capture_mode".into(), "AUTO".into()),
                    ("waits_capture_enabled".into(), (true).to_string()),
                ]
                .into(),
            }),
            continuity: SourceContinuity::Unverified,
        }
    }
    fn plan() -> NativePlanIdentity {
        NativePlanIdentity {
            plan_id: "3".into(),
            query: NativeQueryIdentity {
                query_id: "2".into(),
                qualifiers: [
                    ("context_settings_id".into(), "1".into()),
                    ("object_id".into(), "0".into()),
                    ("query_hash".into(), "0123456789ABCDEF".into()),
                    ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
                ]
                .into(),
            },
            qualifiers: [
                ("plan_hash".into(), "FEDCBA9876543210".into()),
                ("initial_compile_at".into(), "2020-01-01T05:00:01Z".into()),
            ]
            .into(),
        }
    }
    fn wait(source: &CollectionDescription) -> TelemetryRecord {
        TelemetryRecord::QueryWaits(WaitMeasurement {
            key: NativeMeasurementKey {
                native: crate::telemetry::NativeIdentity::new("1")
                    .qualified("plan_id", "3")
                    .qualified("replica_group_id", "1")
                    .qualified("execution_type", (0).to_string()),
                references: crate::telemetry::StatementReferences {
                    query: Some(crate::telemetry::NativeDefinitionReference {
                        native: plan().query.native(),
                        definition_id: Some(plan().query.definition_id(source).unwrap()),
                    }),
                    plan: Some(crate::telemetry::NativeDefinitionReference {
                        native: plan().native(),
                        definition_id: Some(plan().definition_id(source).unwrap()),
                    }),
                },
            },
            category: (1).to_string(),
            unit: crate::telemetry::RuntimeMetricUnit::Milliseconds,
            total: crate::telemetry::MeasurementValue::Integer("2".into()),
            average: Some(2.0),
            minimum: Some(crate::telemetry::MeasurementValue::Integer("2".into())),
            maximum: Some(crate::telemetry::MeasurementValue::Integer("2".into())),
        })
    }

    fn export_runtime(first: Option<&str>, last: Option<&str>) -> crate::Result<ExportedRecord> {
        let source = source();
        let TelemetryRecord::QueryWaits(wait) = wait(&source) else { panic!("native identity fixture") };
        let mut key = wait.key;
        key.native.qualifiers.insert("native-local-generation".into(), "2020-01-01T05:00:00.1234567".into());
        ExportedRecord::new(
            &source,
            &CaptureIdentity { observation_id: "runtime-capture".into(), revision: None },
            TelemetryRecord::QueryRuntime(crate::telemetry::RuntimeMeasurement {
                key,
                executions: "2".into(),
                metrics: vec![crate::telemetry::RuntimeMetric::average(
                    crate::telemetry::RuntimeMetricKey::Duration,
                    crate::telemetry::RuntimeMetricUnit::Milliseconds,
                    1.25,
                )],
                first_execution_at: first.map(str::to_owned),
                last_execution_at: last.map(str::to_owned),
            }),
        )
    }

    #[test]
    fn equivalent_runtime_utc_instants_have_one_export_representation_without_rewriting_native_identity() {
        let offset =
            export_runtime(Some("2020-01-01T08:00:00.1234567+02:00"), Some("2020-01-01T08:00:01+02:00")).unwrap();
        let canonical =
            export_runtime(Some("2020-01-01T06:00:00.123456700Z"), Some("2020-01-01T06:00:01.000000000Z")).unwrap();
        assert_eq!(offset, canonical);
        assert_eq!(serde_json::to_vec(&offset).unwrap(), serde_json::to_vec(&canonical).unwrap());
        let TelemetryRecord::QueryRuntime(row) = &offset.record else { panic!("runtime") };
        assert_eq!(row.first_execution_at.as_deref(), Some("2020-01-01T06:00:00.123456700Z"));
        assert_eq!(row.last_execution_at.as_deref(), Some("2020-01-01T06:00:01.000000000Z"));
        assert_eq!(row.key.native.qualifiers["native-local-generation"], "2020-01-01T05:00:00.1234567");
        assert_eq!(
            row.key.references.query.as_ref().unwrap().native.qualifiers["initial_compile_at"],
            "2020-01-01T05:00:00Z"
        );
    }

    #[test]
    fn runtime_instant_export_preserves_nanosecond_values_instead_of_truncating_to_store_precision() {
        let exported =
            export_runtime(Some("2020-01-01T06:00:00.123456789Z"), Some("2020-01-01T06:00:01.000000001Z")).unwrap();
        let TelemetryRecord::QueryRuntime(row) = exported.record else { panic!("runtime") };
        assert_eq!(row.first_execution_at.as_deref(), Some("2020-01-01T06:00:00.123456789Z"));
        assert_eq!(row.last_execution_at.as_deref(), Some("2020-01-01T06:00:01.000000001Z"));
    }

    #[test]
    fn missing_runtime_instants_stay_missing_and_invalid_utc_fields_cannot_enter_an_export() {
        let exported = export_runtime(None, None).unwrap();
        let TelemetryRecord::QueryRuntime(row) = exported.record else { panic!("runtime") };
        assert!(row.first_execution_at.is_none() && row.last_execution_at.is_none());
        assert!(
            export_runtime(Some("2020-01-01T06:00:00"), None).is_err(),
            "a local timestamp is not an established UTC instant"
        );
        assert!(export_runtime(None, Some("invalid-native-time")).is_err());
    }

    #[test]
    fn runtime_instant_normalization_cannot_erase_excess_length_or_subnanosecond_values() {
        let too_long = format!("2020-01-01T06:00:00.{}Z", "0".repeat(50));
        assert!(too_long.len() > 64);
        for instant in [too_long.as_str(), "2020-01-01T06:00:00.0000000001Z", "2020-01-01T06:00:00.1234567891Z"] {
            assert!(canonical_time(instant).is_err());
            assert!(export_runtime(Some(instant), None).is_err());
            assert!(export_runtime(None, Some(instant)).is_err());
        }
        assert_eq!(canonical_time("2020-01-01T06:00:00.1234567890Z").unwrap(), "2020-01-01T06:00:00.123456789Z");
    }

    #[test]
    fn replay_and_an_endpoint_rename_preserve_ids_with_native_database_identity() {
        let source = source();
        let first = ExportedRecord::new(
            &source,
            &CaptureIdentity { observation_id: "first-capture".into(), revision: None },
            wait(&source),
        )
        .unwrap();
        let mut renamed = source.clone();
        let super::super::CollectionTarget::Database(database) = &mut renamed.target else { panic!("database") };
        database.database = "renamed".into();
        database.location =
            crate::DatabaseLocation::Server { host: "renamed.invalid".into(), port: 1433, instance: None };
        if let super::super::SourceObservation::QueryHistory(history) = &mut renamed.observation {
            history.interval.from = "2020-01-01T08:00:00+02:00".into();
        }
        let replay = ExportedRecord::new(
            &renamed,
            &CaptureIdentity { observation_id: "another-capture".into(), revision: None },
            wait(&renamed),
        )
        .unwrap();
        assert_eq!(first.record_id, replay.record_id);
        assert_ne!(first.capture.observation_id, replay.capture.observation_id);
        assert!(first.record_id.is_some());
    }
    #[test]
    fn reused_native_ids_cannot_hide_a_new_database_or_definition() {
        let source = source();
        let old = plan().definition_id(&source).unwrap();
        let mut changed = plan();
        changed.qualifiers.insert("initial_compile_at".into(), "2020-01-02T05:00:01Z".into());
        assert_ne!(old, changed.definition_id(&source).unwrap());
        let mut recreated = source.clone();
        let super::super::CollectionTarget::Database(database) = &mut recreated.target else { panic!("database") };
        database.incarnation = Some("different-guid".into());
        assert_ne!(old, plan().definition_id(&recreated).unwrap());
        // Native qualifier meaning and canonicalization belong to the adapter.
        changed = plan();
        changed.qualifiers.insert("initial_compile_at".into(), "2020-01-01T07:00:01+02:00".into());
        assert_ne!(old, changed.definition_id(&source).unwrap());
    }
    #[test]
    fn unresolved_measurements_do_not_receive_a_trusted_record_id() {
        let source = source();
        let mut record = wait(&source);
        if let TelemetryRecord::QueryWaits(row) = &mut record {
            row.key.references.plan.as_mut().unwrap().definition_id = None;
        }
        assert!(
            ExportedRecord::new(&source, &CaptureIdentity { observation_id: "capture".into(), revision: None }, record)
                .unwrap()
                .record_id
                .is_none()
        );
    }
}
