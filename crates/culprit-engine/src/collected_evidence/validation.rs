use super::*;
use culprit_collector::analytical::evidence::EvidenceCapture;
use culprit_core::{DatabaseIdentity, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn capture(capture: &EvidenceCapture) -> Result<DatabaseIdentity> {
    let source = capture.source();
    let CollectionTarget::Database(database) = &source.target else { return Err(invalid()) };
    if !source.target.is_valid() || !valid_digest(capture.range_id()) || !valid_digest(capture.manifest_sha256()) {
        return Err(invalid());
    }
    let history = source.query_history().ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    if !history.is_valid() || hours(&history.interval.from, &history.interval.until)? <= 0.0 {
        return Err(invalid());
    }
    if time(capture.collected_from())? > time(capture.collected_until())?
        || time(&history.interval.until)? > time(capture.collected_until())?
    {
        return Err(invalid());
    }
    let mut rows = BTreeSet::new();
    for record in capture.records() {
        let row = record.row();
        if row.capture != *capture.capture()
            || record.row_ordinal() == 0
            || !valid_digest(record.native_payload_sha256())
            || !rows.insert((record.batch_id(), record.row_ordinal()))
            || culprit_collector::delivery::evidence::RowReceipt::from_export(row)?.payload_digest
                != record.native_payload_sha256()
            || ExportedRecord::new(source, capture.capture(), row.record.clone())? != *row
        {
            return Err(invalid());
        }
    }
    Ok(database.clone())
}

pub(super) fn hours(from: &str, until: &str) -> Result<f64> {
    let elapsed = time(until)?.signed_duration_since(time(from)?);
    let elapsed = elapsed.to_std().map_err(|_| invalid())?;
    let hours = elapsed.as_secs_f64() / 3600.0;
    if hours <= 0.0 {
        return Err(invalid());
    }
    Ok(hours)
}

pub(super) fn time(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).map_err(|_| invalid())
}

pub(super) struct Definitions<'a> {
    source: &'a CollectionDescription,
    queries: BTreeMap<&'a str, &'a QueryDefinition>,
    plans: BTreeMap<&'a str, &'a PlanDefinition>,
}
pub(super) struct Joined<'a> {
    pub query: Option<&'a QueryDefinition>,
    pub plan: Option<&'a PlanDefinition>,
}
impl<'a> Definitions<'a> {
    pub fn new(capture: &'a EvidenceCapture) -> Result<Self> {
        let mut definitions = Self { source: capture.source(), queries: BTreeMap::new(), plans: BTreeMap::new() };
        for record in capture.records() {
            match &record.row().record {
                TelemetryRecord::QueryDefinition(query) => {
                    insert_unique(&mut definitions.queries, &query.id, query)?;
                }
                TelemetryRecord::PlanDefinition(plan) => {
                    insert_unique(&mut definitions.plans, &plan.id, plan)?;
                }
                _ => (),
            }
        }
        Ok(definitions)
    }

    pub fn join(&self, references: &StatementReferences) -> Result<Joined<'a>> {
        let Some(query_reference) = &references.query else { return Ok(Joined { query: None, plan: None }) };
        let query_identity = NativeQueryIdentity {
            query_id: query_reference.native.id.clone(),
            qualifiers: query_reference.native.qualifiers.clone(),
        };
        let Some(query_id) = &query_reference.definition_id else { return Ok(Joined { query: None, plan: None }) };
        if query_identity.definition_id(self.source)? != *query_id {
            return Err(invalid());
        }
        let query = self.queries.get(query_id.as_str()).copied();
        if query.is_some_and(|query| query.identity != query_identity) {
            return Err(invalid());
        }
        let Some(plan_reference) = &references.plan else { return Ok(Joined { query, plan: None }) };
        let Some(plan_id) = &plan_reference.definition_id else { return Ok(Joined { query, plan: None }) };
        let identity = NativePlanIdentity {
            plan_id: plan_reference.native.id.clone(),
            query: query_identity,
            qualifiers: plan_reference.native.qualifiers.clone(),
        };
        if identity.definition_id(self.source)? != *plan_id {
            return Err(invalid());
        }
        let plan = self.plans.get(plan_id.as_str()).copied();
        if plan.is_some_and(|plan| {
            plan.identity != identity || plan.query_definition_id.as_ref().is_some_and(|parent| parent != query_id)
        }) {
            return Err(invalid());
        }
        Ok(Joined { query, plan: plan.filter(|plan| plan.query_definition_id.as_ref() == Some(query_id)) })
    }
}

fn insert_unique<'a, T>(map: &mut BTreeMap<&'a str, &'a T>, id: &'a str, record: &'a T) -> Result<()> {
    if map.insert(id, record).is_some() {
        return Err(invalid());
    }
    Ok(())
}
