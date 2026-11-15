//! A native representation difference describes this plan's parameter context, not harmful plan reuse.
use super::*;
use crate::plan_facts::{PlanParameter, PlanParameterComparison};

const MAX_PARAMETER_EXAMPLES: usize = 4;

pub(super) fn context(cx: &Cx) -> Vec<Draft> {
    let Some(context) = cx.plan.facts().parameter_context.as_ref().filter(|context| context.is_valid()) else {
        return Vec::new();
    };
    if cx.plan.kind != PlanKind::Actual || !has_runtime_evidence(cx) {
        return Vec::new();
    }
    let differences: Vec<_> = context
        .parameters
        .iter()
        .filter(|parameter| parameter.comparison == PlanParameterComparison::DifferentRepresentation)
        .collect();
    if differences.is_empty() {
        return Vec::new();
    }
    let examples: Vec<_> =
        differences.iter().take(MAX_PARAMETER_EXAMPLES).map(|parameter| parameter_label(parameter)).collect();
    let mut concern = Draft::new("parameter-context", InsightArea::Query, 0.0, vec![cx.plan.root.id]);
    concern.fixed_weight = true;
    concern.cap = Severity::Low;
    concern.title = t!("insight.parameter_context.title");
    concern.detail = t!(
        "insight.parameter_context.detail",
        count = format::count(differences.len() as f64),
        parameters = format::list(&examples)
    );
    concern.fix = t!("insight.parameter_context.fix");
    concern.verify = t!("insight.parameter_context.verify");
    vec![concern]
}

fn parameter_label(parameter: &PlanParameter) -> String {
    let name = parameter.name.as_deref().expect("a valid comparison requires its native parameter name");
    match parameter.data_type.as_deref() {
        Some(data_type) => t!("insight.parameter_context.named_type", parameter = name, data_type = data_type),
        None => t!("insight.parameter_context.unknown_type", parameter = name),
    }
}

fn has_runtime_evidence(cx: &Cx) -> bool {
    cx.nodes.iter().any(|node| {
        node.act_rows.is_some_and(|rows| rows.is_finite() && rows >= 0.0)
            && node
                .act_executions
                .is_some_and(|executions| executions.is_finite() && executions >= 1.0 && executions.fract() == 0.0)
    })
}
