//! Plan definitions are analyzed inside their authenticated capture, without live metadata or counter joins.
use super::*;
use culprit_core::{
    CachedPlanEntry, CachedPlanIdentity, CachedPlanLimit, CachedPlanLimitation, CachedPlanPayload, CachedPlanReport,
    CollectedCachedPlanAnalysis, Plan, PlanKind, format, plan_facts::RelationalInputKind,
};
use std::collections::BTreeSet;

pub fn cached_plans(capture: &EvidenceCapture) -> Result<CollectedCachedPlanAnalysis> {
    let SourceObservation::Snapshot(native) = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !matches!(native.dataset, SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let inputs = CapturedState::sources_for(capture, native.dataset, EvidenceFamily::ObjectDefinitions)?;
    let mut keys = BTreeSet::new();
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    for input in inputs {
        input.database()?;
        input.bounds()?;
        let definitions = input.definitions()?;
        sources.push(input.outcome(definitions.len())?);
        for definition in definitions {
            if !keys.insert((&definition.source, &definition.key)) {
                return Err(invalid());
            }
            entries.push(explain(capture, native.dataset, definition)?);
        }
    }
    entries.sort_by(|left, right| {
        (&left.source, &left.definition_key, &left.definition_version).cmp(&(
            &right.source,
            &right.definition_key,
            &right.definition_version,
        ))
    });
    let report = CachedPlanReport {
        sources,
        title: culprit_core::t!("engine.cached_plans.title"),
        summary: culprit_core::t!("engine.cached_plans.summary", count = format::count(entries.len() as f64)),
        limitations: vec![culprit_core::t!("engine.cached_plans.limitation.population")],
        entries,
    };
    Ok(CollectedCachedPlanAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn explain(
    capture: &EvidenceCapture,
    dataset: SnapshotDataset,
    definition: &ObjectDefinitionRecord,
) -> Result<CachedPlanEntry> {
    let mut entry = CachedPlanEntry {
        source: definition.source.clone(),
        definition_key: definition.key.clone(),
        definition_version: definition.version.clone(),
        identity: None,
        plan: None,
        limitations: Vec::new(),
    };
    match (&definition.fact, dataset) {
        (ObjectDefinitionFact::MssqlCachedDefinition(native), SnapshotDataset::CachedDefinitions) => {
            require_mssql(capture)?;
            entry.identity = Some(CachedPlanIdentity::Mssql(native.context.identity.clone()));
            payload_available(&native.statement.payload, CachedPlanPayload::Statement, &mut entry);
            if payload_available(&native.estimated_plan.payload, CachedPlanPayload::EstimatedPlan, &mut entry) {
                attach_mssql(&mut entry, || parse_cached(native));
            }
        }
        (ObjectDefinitionFact::MssqlRetainedPlan(native), SnapshotDataset::RetainedPlans) => {
            require_mssql(capture)?;
            entry.identity = Some(CachedPlanIdentity::Mssql(native.context.identity.clone()));
            if payload_available(&native.batch_plan.payload, CachedPlanPayload::RetainedPlan, &mut entry) {
                attach_mssql(&mut entry, || parse_retained(native));
            }
        }
        (
            ObjectDefinitionFact::RetainedPlan(native),
            SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans,
        ) => {
            if native.definition.id != native.definition.identity.definition_id(capture.source())? {
                return Err(invalid());
            }
            entry.identity = Some(CachedPlanIdentity::Native(native.definition.identity.clone()));
            if payload_available(&native.definition.plan, CachedPlanPayload::RetainedPlan, &mut entry) {
                attach_native(&mut entry, native, capture.source().target.engine());
            }
        }
        _ => add_limit(&mut entry, CachedPlanLimit::UnsupportedDefinition),
    }
    if let Some(plan) = &mut entry.plan {
        plan.facts_mut().constants_redacted = true;
        culprit_core::insights::annotate(plan);
        // A captured definition has no current schema, indexes or settings. Keep explanatory advice, without
        // making an old optimizer request into an executable change against an unobserved current database.
        for insight in plan.insights.as_mut().into_iter().flat_map(|insights| &mut insights.items) {
            insight.fix_sql = None;
        }
        plan.raw = None;
        plan.statement = None;
        if plan.kind != PlanKind::Actual {
            add_limit(&mut entry, CachedPlanLimit::MissingActualMeasurements);
        }
        if entry.plan.as_ref().is_some_and(has_unknown_input) {
            add_limit(&mut entry, CachedPlanLimit::UnknownRelationalInput);
        }
        add_limit(&mut entry, CachedPlanLimit::MissingCurrentPrerequisites);
    }
    Ok(entry)
}

fn require_mssql(capture: &EvidenceCapture) -> Result<()> {
    if capture.source().target.engine() != Engine::Mssql {
        return Err(invalid());
    }
    Ok(())
}

fn payload_available(payload: &DefinitionPayload, field: CachedPlanPayload, entry: &mut CachedPlanEntry) -> bool {
    match payload {
        DefinitionPayload::Available { .. } => true,
        DefinitionPayload::Unavailable { reason } => {
            add_limit(entry, CachedPlanLimit::PayloadUnavailable { payload: field, reason: *reason });
            false
        }
    }
}

fn attach_mssql(entry: &mut CachedPlanEntry, parse: impl FnOnce() -> Result<Plan>) {
    if !crate::plans::PlanFormat::ShowplanXml.is_available() {
        add_limit(entry, CachedPlanLimit::NativeParserUnavailable);
    } else {
        match parse() {
            Ok(plan) => entry.plan = Some(plan),
            Err(_) => add_limit(entry, CachedPlanLimit::UnusablePlan),
        }
    }
}

fn parse_cached(native: &MssqlCachedDefinition) -> Result<Plan> {
    #[cfg(feature = "mssql")]
    {
        culprit_mssql::plan_xml::parse_cached_definition(native)
    }
    #[cfg(not(feature = "mssql"))]
    {
        let _ = native;
        Err(DeliveryProblem::UnsupportedSchema.error())
    }
}

fn parse_retained(native: &MssqlRetainedPlanDefinition) -> Result<Plan> {
    #[cfg(feature = "mssql")]
    {
        culprit_mssql::plan_xml::parse_retained_definition(native)
    }
    #[cfg(not(feature = "mssql"))]
    {
        let _ = native;
        Err(DeliveryProblem::UnsupportedSchema.error())
    }
}

fn attach_native(entry: &mut CachedPlanEntry, native: &NativeRetainedPlan, engine: Engine) {
    let DefinitionPayload::Available { text, .. } = &native.definition.plan else { return };
    let Some(format) = crate::plans::detect_native(text, engine) else {
        add_limit(entry, CachedPlanLimit::UnusablePlan);
        return;
    };
    if !format.is_available() {
        add_limit(entry, CachedPlanLimit::NativeParserUnavailable);
        return;
    }
    // A generic batch XML has no SQL Server statement-offset context. Its heaviest statement cannot stand in for
    // this native definition; the source-owned SQL Server definition above supplies strict selection instead.
    if format == crate::plans::PlanFormat::ShowplanXml {
        add_limit(entry, CachedPlanLimit::UnusablePlan);
        return;
    }
    match crate::plans::parse_native_plan(text, engine) {
        Ok(plan) => entry.plan = Some(plan),
        _ => add_limit(entry, CachedPlanLimit::UnusablePlan),
    }
}

fn has_unknown_input(plan: &Plan) -> bool {
    plan.nodes().iter().any(|node| {
        node.facts().relational_input.is_some_and(|input| input.qualified_kind() == RelationalInputKind::Unknown)
    })
}

fn add_limit(entry: &mut CachedPlanEntry, reason: CachedPlanLimit) {
    if entry.limitations.iter().any(|limit| limit.reason == reason) {
        return;
    }
    let explanation = match reason {
        CachedPlanLimit::UnsupportedDefinition => culprit_core::t!("engine.cached_plans.limit.unsupported"),
        CachedPlanLimit::NativeParserUnavailable => culprit_core::t!("engine.cached_plans.limit.parser"),
        CachedPlanLimit::PayloadUnavailable { payload, reason } => {
            let payload = match payload {
                CachedPlanPayload::Statement => culprit_core::t!("engine.cached_plans.payload.statement"),
                CachedPlanPayload::EstimatedPlan => culprit_core::t!("engine.cached_plans.payload.estimated"),
                CachedPlanPayload::RetainedPlan => culprit_core::t!("engine.cached_plans.payload.retained"),
            };
            culprit_core::t!("engine.cached_plans.limit.payload", payload = payload, reason = payload_reason(reason))
        }
        CachedPlanLimit::UnusablePlan => culprit_core::t!("engine.cached_plans.limit.unusable"),
        CachedPlanLimit::MissingActualMeasurements => culprit_core::t!("engine.cached_plans.limit.actual"),
        CachedPlanLimit::UnknownRelationalInput => culprit_core::t!("engine.cached_plans.limit.input"),
        CachedPlanLimit::MissingCurrentPrerequisites => culprit_core::t!("engine.cached_plans.limit.prerequisites"),
    };
    entry.limitations.push(CachedPlanLimitation { reason, explanation });
}

fn payload_reason(reason: PayloadUnavailable) -> String {
    match reason {
        PayloadUnavailable::Missing => culprit_core::t!("engine.cached_plans.reason.missing"),
        PayloadUnavailable::Restricted => culprit_core::t!("engine.cached_plans.reason.restricted"),
        PayloadUnavailable::Encrypted => culprit_core::t!("engine.cached_plans.reason.encrypted"),
        PayloadUnavailable::Oversized => culprit_core::t!("engine.cached_plans.reason.oversized"),
        PayloadUnavailable::RedactionFailed => culprit_core::t!("engine.cached_plans.reason.redaction"),
        PayloadUnavailable::NotCollected => culprit_core::t!("engine.cached_plans.reason.not_collected"),
        PayloadUnavailable::SourceUnavailable => culprit_core::t!("engine.cached_plans.reason.source"),
    }
}
