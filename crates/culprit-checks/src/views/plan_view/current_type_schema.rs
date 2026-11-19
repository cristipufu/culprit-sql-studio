//! Current catalog metadata contextualizes a retained input; it cannot establish its compilation schema or cause.
use culprit_core::{Plan, format, plan_view::PlanCurrentTypeSchemaView, t, telemetry::MssqlCurrentTypeSchemaStatus};

pub fn explain(plan: &Plan) -> Option<PlanCurrentTypeSchemaView> {
    let schema = plan.facts().current_type_schema.as_deref().filter(|schema| schema.is_valid())?;
    let summary = match schema.status {
        MssqlCurrentTypeSchemaStatus::Available => t!(
            "view.plan_type_schema.available",
            n = schema.types.len(),
            count = format::count(schema.types.len() as f64),
            at = &schema.observed_at
        ),
        MssqlCurrentTypeSchemaStatus::Restricted => t!("view.plan_type_schema.restricted"),
        MssqlCurrentTypeSchemaStatus::NotCollected => t!("view.plan_type_schema.not_collected"),
        MssqlCurrentTypeSchemaStatus::Oversized => t!("view.plan_type_schema.oversized"),
        MssqlCurrentTypeSchemaStatus::Changed => t!("view.plan_type_schema.changed"),
    };
    Some(PlanCurrentTypeSchemaView {
        title: t!("view.plan_type_schema.title"),
        summary,
        limitations: vec![t!("view.plan_type_schema.limit.current")],
    })
}

#[cfg(test)]
mod tests;
