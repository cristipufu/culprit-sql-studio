//! A native representation difference describes this plan's parameter context, not harmful plan reuse.
use super::relational_inputs::{AccessComparison, exact_count, serial_executions};
use super::*;
use crate::plan_facts::{PlanParameter, PlanParameterComparison};

mod lookup;
pub(super) use lookup::LookupWork;
pub(crate) use lookup::{LookupReadComparison, lookup_read_comparison};

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
    let mut detail = vec![t!(
        "insight.parameter_context.detail",
        count = format::count(differences.len() as f64),
        parameters = format::list(&examples)
    )];
    if let Some(cached) = context.retrieved_from_cache {
        detail.push(if cached {
            t!("insight.parameter_context.cache.used")
        } else {
            t!("insight.parameter_context.cache.not_used")
        });
    }
    for parameter in differences.iter().take(MAX_PARAMETER_EXAMPLES) {
        if let Some(equal) = parameter.integer_values_equal {
            detail.push(if equal {
                t!("insight.parameter_context.integer.same", parameter = parameter_label(parameter))
            } else {
                t!("insight.parameter_context.integer.different", parameter = parameter_label(parameter))
            });
        }
    }
    let nodes = access_details(cx, &mut detail);
    if !nodes.is_empty() {
        concern.nodes = nodes;
    }
    if context.issues.iter().any(|issue| {
        matches!(
            issue,
            crate::plan_facts::PlanParameterIssue::UnsupportedUse
                | crate::plan_facts::PlanParameterIssue::AmbiguousUse
                | crate::plan_facts::PlanParameterIssue::UseLimit
                | crate::plan_facts::PlanParameterIssue::MetadataLimit
        )
    }) {
        detail.push(t!("insight.parameter_context.binding_withheld"));
    }
    concern.detail = detail.join(" ");
    concern.fix = t!("insight.parameter_context.fix");
    concern.verify = t!("insight.parameter_context.verify");
    vec![concern]
}

fn access_details(cx: &Cx<'_>, detail: &mut Vec<String>) -> Vec<u32> {
    let accesses = cx
        .nodes
        .iter()
        .filter_map(|node| access_mismatch(cx, node).map(|access| (*node, access)))
        .take(MAX_PARAMETER_EXAMPLES)
        .collect::<Vec<_>>();
    for (node, access) in &accesses {
        detail.push(t!(
            "insight.parameter_context.access",
            node = format::digits(node.id.to_string()),
            parameter = parameter_label(access.parameter)
        ));
        detail.push(access.comparison.detail());
    }
    accesses.iter().map(|(node, _)| node.id).collect()
}

pub(super) struct ParameterAccess<'a> {
    parameter: &'a PlanParameter,
    comparison: AccessComparison,
}

/// The use and both native counts belong to this node. This does not explain why its estimate differed.
pub(super) fn access_mismatch<'a>(cx: &Cx<'a>, node: &PlanNode) -> Option<ParameterAccess<'a>> {
    let access = bound_access_mismatch(cx, node)?;
    (access.parameter.comparison == PlanParameterComparison::DifferentRepresentation
        && access.parameter.integer_values_equal != Some(true))
    .then_some(access)
}

fn bound_access_mismatch<'a>(cx: &Cx<'a>, node: &PlanNode) -> Option<ParameterAccess<'a>> {
    if cx.plan.kind != PlanKind::Actual {
        return None;
    }
    let context = cx.plan.facts().parameter_context.as_ref()?;
    let usage = context.uses.iter().find(|usage| usage.node_id == node.id)?;
    if !context.is_valid() {
        return None;
    }
    let parameter = context.parameter(usage)?;
    if node.kind != NodeKind::Seek || node.facts().relational_input.is_some() || serial_executions(node).is_none() {
        return None;
    }
    if !cx.node_ids_unique {
        return None;
    }
    if !node.facts().access_rows?.actual_total.is_some_and(exact_count) {
        return None;
    }
    let comparison = AccessComparison::from_node(cx, node)?;
    comparison.underestimated().then_some(ParameterAccess { parameter, comparison })
}

pub(super) fn access_advice() -> String {
    t!("insight.parameter_context.access_fix")
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
