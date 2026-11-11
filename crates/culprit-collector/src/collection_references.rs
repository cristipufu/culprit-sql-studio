//! Definition closure belongs to an observation; native IDs and optional plans have no engine-specific meaning here.
use culprit_core::telemetry::{
    CollectionDescription, DefinitionCoverage, NativeDefinitionReference, NativeIdentity, ReferenceCoverage,
    TelemetryRecord,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct RequiredDefinitions(BTreeMap<NativeIdentity, Option<String>>);
impl RequiredDefinitions {
    fn accepts(&self, reference: &NativeDefinitionReference) -> bool {
        self.0.get(&reference.native).is_none_or(|before| *before == reference.definition_id)
    }
    fn record(&mut self, reference: &NativeDefinitionReference) {
        self.0.insert(reference.native.clone(), reference.definition_id.clone());
    }
    fn coverage(&self, available: impl Fn(&NativeIdentity, &str) -> bool, unidentified: usize) -> DefinitionCoverage {
        let resolved =
            self.0.iter().filter(|(native, id)| id.as_deref().is_some_and(|id| available(native, id))).count() as u32;
        let required = (self.0.len() + unidentified) as u32;
        DefinitionCoverage { required, resolved, unresolved: required - resolved }
    }
}
struct ObservedPlan {
    native: NativeIdentity,
    query_native: NativeIdentity,
    query_definition_id: Option<String>,
}
#[derive(Default)]
pub(crate) struct ObservationReferences {
    required_queries: RequiredDefinitions,
    required_plans: RequiredDefinitions,
    unidentified_queries: BTreeSet<NativeIdentity>,
    plans: BTreeMap<String, ObservedPlan>,
    plan_queries: BTreeMap<NativeIdentity, Option<NativeDefinitionReference>>,
    queries: BTreeMap<String, NativeIdentity>,
    unavailable_payloads: u32,
    state_rows: BTreeMap<String, u32>,
    pub(crate) activity: crate::collection_activity::ActivityRecords,
}
impl ObservationReferences {
    pub(crate) fn accepts_page(&self, records: &[TelemetryRecord]) -> bool {
        let mut queries = RequiredDefinitions::default();
        let mut plans = RequiredDefinitions::default();
        let mut plan_queries = BTreeMap::new();
        for key in records.iter().filter_map(TelemetryRecord::measurement_key) {
            if let Some(plan) = &key.references.plan {
                let query = &key.references.query;
                if self.plan_queries.get(&plan.native).is_some_and(|before| before != query)
                    || plan_queries.get(&plan.native).is_some_and(|before| before != query)
                {
                    return false;
                }
                plan_queries.insert(plan.native.clone(), query.clone());
            }
            for (reference, known, page) in [
                (key.references.query.as_ref(), &self.required_queries, &mut queries),
                (key.references.plan.as_ref(), &self.required_plans, &mut plans),
            ] {
                if let Some(reference) = reference {
                    if !known.accepts(reference) || !page.accepts(reference) {
                        return false;
                    }
                    page.record(reference);
                }
            }
        }
        true
    }
    pub(crate) fn record(&mut self, record: &TelemetryRecord) {
        self.activity.record(record);
        if !matches!(record, TelemetryRecord::Incident(_) | TelemetryRecord::ActivityCheckpoint(_)) {
            self.unavailable_payloads +=
                record.definition_payloads().map(|field| u32::from(!field.payload.is_available())).sum::<u32>();
        }
        match record {
            TelemetryRecord::QueryDefinition(row) => {
                self.queries.insert(row.id.clone(), row.identity.native());
            }
            TelemetryRecord::PlanDefinition(row) => {
                self.plans.insert(
                    row.id.clone(),
                    ObservedPlan {
                        native: row.identity.native(),
                        query_native: row.identity.query.native(),
                        query_definition_id: row.query_definition_id.clone(),
                    },
                );
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
                    if let Some(query) = &key.references.query {
                        self.required_queries.record(query);
                    } else {
                        self.unidentified_queries.insert(key.native.clone());
                    }
                    if let Some(plan) = &key.references.plan {
                        self.required_plans.record(plan);
                        self.plan_queries.insert(plan.native.clone(), key.references.query.clone());
                    }
                }
            }
        }
    }
    fn plan_is_resolved(&self, native: &NativeIdentity, id: &str) -> bool {
        let Some(plan) = self.plans.get(id) else { return false };
        let Some(query_definition_id) = plan.query_definition_id.as_ref() else { return false };
        let Some(query) = self.plan_queries.get(native).and_then(Option::as_ref) else { return false };
        plan.native == *native
            && plan.query_native == query.native
            && query.definition_id.as_ref() == Some(query_definition_id)
            && self.queries.get(query_definition_id) == Some(&query.native)
    }
    pub(crate) fn coverage(&self, source_rechecked: bool, source: Option<&CollectionDescription>) -> ReferenceCoverage {
        let queries = self
            .required_queries
            .coverage(|native, id| self.queries.get(id) == Some(native), self.unidentified_queries.len());
        let plans = self.required_plans.coverage(|native, id| self.plan_is_resolved(native, id), 0);
        let activity =
            source.and_then(|source| source.observation.activity()).map(|snapshot| self.activity.coverage(snapshot));
        ReferenceCoverage {
            complete: source_rechecked
                && queries.complete()
                && plans.complete()
                && source.and_then(|source| source.observation.snapshot()).is_none_or(|snapshot| {
                    snapshot
                        .sources
                        .iter()
                        .all(|source| self.state_rows.get(&source.source).copied().unwrap_or(0) == source.records)
                })
                && activity.as_ref().is_none_or(|activity| activity.complete),
            queries,
            plans,
            unavailable_payloads: self.unavailable_payloads,
            activity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::*;
    fn query(id: &str) -> QueryDefinition {
        QueryDefinition {
            id: digest(id.as_bytes()),
            origin: StatementOrigin::Unknown,
            monitoring_template_id: None,
            identity: NativeQueryIdentity { query_id: id.into(), qualifiers: Default::default() },
            context: Default::default(),
            context_available: false,
            source_text_bytes: None,
            sql: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        }
    }
    fn reference(query: &QueryDefinition) -> NativeDefinitionReference {
        NativeDefinitionReference { native: query.identity.native(), definition_id: Some(query.id.clone()) }
    }
    fn measurement(query: &QueryDefinition, plan: Option<NativeDefinitionReference>) -> TelemetryRecord {
        TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new("row"),
                references: StatementReferences { query: Some(reference(query)), plan },
            },
            executions: "1".into(),
            metrics: vec![],
            first_execution_at: None,
            last_execution_at: None,
        })
    }
    #[test]
    fn query_only_measurements_require_the_exact_query_definition() {
        let mut references = ObservationReferences::default();
        let query = query("opaque-query");
        references.record(&measurement(&query, None));
        assert!(!references.coverage(true, None).complete);
        references.record(&TelemetryRecord::QueryDefinition(query));
        let coverage = references.coverage(true, None);
        assert!(coverage.complete);
        assert_eq!(coverage.queries.required, 1);
        assert_eq!(coverage.plans.required, 0);
    }
    #[test]
    fn an_unlinked_native_plan_cannot_close_a_runtime_reference() {
        let query = query("native-query");
        let plan = PlanDefinition {
            id: digest(b"independent-cache-plan"),
            identity: NativePlanIdentity {
                plan_id: "plan".into(),
                query: query.identity.clone(),
                qualifiers: Default::default(),
            },
            query_definition_id: None,
            engine_version: None,
            native_properties: Default::default(),
            source_text_bytes: None,
            plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        };
        let required =
            NativeDefinitionReference { native: plan.identity.native(), definition_id: Some(plan.id.clone()) };
        let mut references = ObservationReferences::default();
        for record in [
            measurement(&query, Some(required)),
            TelemetryRecord::QueryDefinition(query),
            TelemetryRecord::PlanDefinition(plan),
        ] {
            references.record(&record);
        }
        assert_eq!(references.coverage(true, None).plans.unresolved, 1);
    }
    #[test]
    fn an_unrelated_query_definition_cannot_close_a_plan_reference() {
        let expected = query("expected");
        let other = query("other");
        let plan = PlanDefinition {
            id: digest(b"plan"),
            identity: NativePlanIdentity {
                plan_id: "plan".into(),
                query: other.identity.clone(),
                qualifiers: Default::default(),
            },
            query_definition_id: Some(other.id.clone()),
            engine_version: None,
            native_properties: Default::default(),
            source_text_bytes: None,
            plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        };
        let required =
            NativeDefinitionReference { native: plan.identity.native(), definition_id: Some(plan.id.clone()) };
        let row = measurement(&expected, Some(required.clone()));
        let mut references = ObservationReferences::default();
        assert!(references.accepts_page(std::slice::from_ref(&row)));
        for record in [
            row,
            TelemetryRecord::QueryDefinition(expected),
            TelemetryRecord::QueryDefinition(other.clone()),
            TelemetryRecord::PlanDefinition(plan),
        ] {
            references.record(&record);
        }
        let coverage = references.coverage(true, None);
        assert!(coverage.queries.complete());
        assert_eq!(coverage.plans.unresolved, 1);
        assert!(!coverage.complete);
        let changed = measurement(&other, Some(required));
        assert!(!references.accepts_page(&[changed]), "a native plan cannot change its parent during a capture");
    }
}
