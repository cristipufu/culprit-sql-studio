//! Parameter representations are compared only while their selected native XML is in memory. Values are discarded.
use super::{LocalName, child, children};
use culprit_core::plan_facts::{
    MAX_PLAN_PARAMETERS, PlanParameter, PlanParameterComparison as Comparison, PlanParameterContext,
    PlanParameterIssue as Issue, PlanParameterValueState as State, parameter_name_is_valid, parameter_type_is_valid,
};
use roxmltree::Node;
use std::collections::BTreeSet;

const MAX_PARAMETER_REPRESENTATION_BYTES: usize = 16 * 1024;

pub(super) fn read(query_plan: Node<'_, '_>) -> Option<PlanParameterContext> {
    child(query_plan, "ParameterList")?;
    let lists = || children(query_plan, "ParameterList");
    let ambiguous = lists().take(2).count() > 1;
    let runtime = !ambiguous && has_runtime_evidence(query_plan);
    let mut issues = BTreeSet::new();
    if ambiguous {
        issues.insert(Issue::InvalidMetadata);
    }
    let mut parameters = Vec::new();
    for native in lists().flat_map(|list| children(list, "ColumnReference")).take(MAX_PLAN_PARAMETERS + 1) {
        if parameters.len() == MAX_PLAN_PARAMETERS {
            issues.insert(Issue::ParameterLimit);
            break;
        }
        parameters.push(parameter(native, runtime, &mut issues));
    }
    withhold_duplicate_comparisons(&mut parameters, &mut issues);
    if issues.contains(&Issue::ParameterLimit) {
        for parameter in &mut parameters {
            parameter.comparison = Comparison::Unavailable;
        }
    }
    Some(PlanParameterContext { parameters, issues: issues.into_iter().collect() })
}

fn parameter(native: Node<'_, '_>, runtime: bool, issues: &mut BTreeSet<Issue>) -> PlanParameter {
    let name = metadata(native.attribute("Column"), parameter_name, issues);
    if name.is_none() {
        issues.insert(Issue::InvalidMetadata);
    }
    let data_type = metadata(native.attribute("ParameterDataType"), parameter_type_is_valid, issues);
    let compiled_value = native.attribute("ParameterCompiledValue");
    let runtime_value = native.attribute("ParameterRuntimeValue");
    let compiled = value_state(compiled_value);
    let mut runtime_state = value_state(runtime_value);
    if runtime_state == State::Recorded && !runtime {
        runtime_state = State::Unqualified;
    }
    let comparison = if name.is_some() && compiled == State::Recorded && runtime_state == State::Recorded {
        if compiled_value == runtime_value {
            Comparison::SameRepresentation
        } else {
            Comparison::DifferentRepresentation
        }
    } else {
        Comparison::Unavailable
    };
    PlanParameter { name, data_type, compiled, runtime: runtime_state, comparison }
}

fn parameter_name(name: &str) -> bool {
    parameter_name_is_valid(name) && name.starts_with('@') && !name.starts_with("@@") && name.len() > 1
}

fn metadata(value: Option<&str>, valid: fn(&str) -> bool, issues: &mut BTreeSet<Issue>) -> Option<String> {
    value.and_then(|value| {
        if valid(value) {
            Some(value.to_string())
        } else {
            issues.insert(Issue::InvalidMetadata);
            None
        }
    })
}

fn value_state(value: Option<&str>) -> State {
    let Some(value) = value else { return State::NotRecorded };
    if value.len() > MAX_PARAMETER_REPRESENTATION_BYTES
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
    {
        return State::Unqualified;
    }
    let lower = value.trim().to_ascii_lowercase();
    let marker = lower.trim_matches(['(', ')', ' ']);
    let marker = marker
        .strip_prefix("n'")
        .or_else(|| marker.strip_prefix('\''))
        .map_or(marker, |literal| literal.strip_suffix('\'').unwrap_or(literal));
    if value.contains('?')
        || matches!(marker, "[redacted]" | "<redacted>" | "redacted" | "<hidden>" | "hidden" | "***" | "...")
    {
        State::Redacted
    } else {
        State::Recorded
    }
}

fn withhold_duplicate_comparisons(parameters: &mut [PlanParameter], issues: &mut BTreeSet<Issue>) {
    let duplicates: BTreeSet<_> = {
        let mut names = BTreeSet::new();
        parameters
            .iter()
            .filter_map(|parameter| parameter.name.as_deref())
            .filter(|name| !names.insert(*name))
            .map(str::to_string)
            .collect()
    };
    if duplicates.is_empty() {
        return;
    }
    issues.insert(Issue::DuplicateName);
    for parameter in parameters {
        if parameter.name.as_ref().is_some_and(|name| duplicates.contains(name)) {
            parameter.comparison = Comparison::Unavailable;
        }
    }
}

fn has_runtime_evidence(query_plan: Node<'_, '_>) -> bool {
    query_plan.descendants().any(|node| {
        node.has_tag_name_local("RunTimeCountersPerThread")
            && node.ancestors().find(|ancestor| ancestor.has_tag_name_local("QueryPlan")) == Some(query_plan)
            && number(node, "ActualRows").is_some_and(|rows| rows >= 0.0)
            && number(node, "ActualExecutions").is_some_and(|executions| executions >= 1.0 && executions.fract() == 0.0)
    })
}

fn number(node: Node<'_, '_>, attribute: &str) -> Option<f64> {
    node.attribute(attribute)?.parse::<f64>().ok().filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests;
