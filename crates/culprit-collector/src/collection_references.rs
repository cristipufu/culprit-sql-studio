//! Reference closure belongs to an observation, separately from file I/O and source cursor ownership.
use culprit_core::telemetry::{CollectionDescription, ReferenceCoverage, TelemetryRecord};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct ObservationReferences {
    required: BTreeMap<String, Option<String>>,
    plans: BTreeMap<String, (String, String)>,
    queries: BTreeSet<String>,
    unavailable_payloads: u32,
    state_rows: BTreeMap<String, u32>,
    pub(crate) activity: crate::collection_activity::ActivityRecords,
}

impl ObservationReferences {
    pub(crate) fn accepts_page(&self, records: &[TelemetryRecord]) -> bool {
        let mut page = BTreeMap::new();
        for key in records.iter().filter_map(TelemetryRecord::measurement_key) {
            if self.required.get(&key.plan_id).is_some_and(|before| *before != key.plan_definition_id)
                || page
                    .insert(&key.plan_id, &key.plan_definition_id)
                    .is_some_and(|before| *before != key.plan_definition_id)
            {
                return false;
            }
        }
        true
    }
    pub(crate) fn record(&mut self, record: &TelemetryRecord) {
        self.activity.record(record);
        // Derived incident/checkpoint copies do not count the native payload a second time.
        if !matches!(record, TelemetryRecord::Incident(_) | TelemetryRecord::ActivityCheckpoint(_)) {
            self.unavailable_payloads +=
                record.definition_payloads().map(|field| u32::from(!field.payload.is_available())).sum::<u32>();
        }
        match record {
            TelemetryRecord::QueryDefinition(row) => {
                self.queries.insert(row.id.clone());
            }
            TelemetryRecord::PlanDefinition(row) => {
                self.plans.insert(row.id.clone(), (row.identity.plan_id.clone(), row.query_definition_id.clone()));
            }
            TelemetryRecord::Operational(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            TelemetryRecord::ObjectDefinition(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            TelemetryRecord::WaitCounter(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            TelemetryRecord::Replication(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            TelemetryRecord::Tuning(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            TelemetryRecord::Setting(row) => {
                *self.state_rows.entry(row.source.clone()).or_default() += 1;
            }
            _ => {
                if let Some(key) = record.measurement_key() {
                    self.required.insert(key.plan_id.clone(), key.plan_definition_id.clone());
                }
            }
        }
    }
    pub(crate) fn coverage(&self, source_rechecked: bool, source: Option<&CollectionDescription>) -> ReferenceCoverage {
        let resolved = self
            .required
            .iter()
            .filter(|(native_id, definition)| {
                definition
                    .as_ref()
                    .and_then(|id| self.plans.get(id))
                    .is_some_and(|(plan_id, query)| plan_id == *native_id && self.queries.contains(query))
            })
            .count() as u32;
        let required = self.required.len() as u32;
        let activity =
            source.and_then(|source| source.observation.activity()).map(|snapshot| self.activity.coverage(snapshot));
        ReferenceCoverage {
            required_plans: required,
            resolved_plans: resolved,
            unresolved_plans: required - resolved,
            unavailable_payloads: self.unavailable_payloads,
            complete: source_rechecked
                && required == resolved
                && source.and_then(|source| source.observation.snapshot()).is_none_or(|snapshot| {
                    snapshot
                        .sources
                        .iter()
                        .all(|source| self.state_rows.get(&source.source).copied().unwrap_or(0) == source.records)
                })
                && activity.as_ref().is_none_or(|activity| activity.complete),
            activity,
        }
    }
}
