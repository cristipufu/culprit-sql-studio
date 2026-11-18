//! Parameter representations are compared only while their selected native XML is in memory. Values are discarded.
use super::{child, children, query_plan_has_runtime};
use culprit_core::plan_facts::{
    MAX_PLAN_PARAMETERS, PlanParameter, PlanParameterComparison as Comparison, PlanParameterContext,
    PlanParameterIssue as Issue, PlanParameterValueState as State, parameter_name_is_valid, parameter_type_is_valid,
};
use roxmltree::Node;
use std::collections::BTreeSet;
mod integers;
mod uses;

const MAX_PARAMETER_REPRESENTATION_BYTES: usize = 16 * 1024;

pub(super) fn withhold_statement_binding(context: &mut PlanParameterContext) {
    context.retrieved_from_cache = None;
    context.uses.clear();
    context.issues.push(Issue::UnboundStatement);
    for parameter in &mut context.parameters {
        parameter.comparison = Comparison::Unavailable;
        parameter.integer_values_equal = None;
        if parameter.runtime == State::Recorded {
            parameter.runtime = State::Unqualified;
        }
    }
}

pub(super) fn restore(plan: &mut culprit_core::Plan, context: &PlanParameterContext) -> culprit_core::Result<()> {
    let invalid = || culprit_core::telemetry::DeliveryProblem::Integrity.error();
    if !context.is_valid()
        || context.parameters.iter().any(|parameter| {
            parameter.integer_values_equal.is_some() && !integers::supports(parameter.data_type.as_deref())
        })
    {
        return Err(invalid());
    }
    if plan
        .facts()
        .parameter_context
        .as_ref()
        .is_none_or(|parsed| parsed.retrieved_from_cache != context.retrieved_from_cache)
    {
        return Err(invalid());
    }
    if !context.issues.contains(&Issue::MetadataLimit) {
        let parsed = plan.facts().parameter_context.as_ref().ok_or_else(invalid)?;
        if context.uses != parsed.uses
            || context.parameters.len() != parsed.parameters.len()
            || context.parameters.iter().zip(&parsed.parameters).any(|(native, retained)| {
                native.name != retained.name
                    || native.data_type != retained.data_type
                    || !attribute_presence_matches(native.compiled, retained.compiled)
                    || !attribute_presence_matches(native.runtime, retained.runtime)
            })
        {
            return Err(invalid());
        }
    }
    plan.facts_mut().parameter_context = Some(context.clone());
    Ok(())
}

// Redaction may change Recorded to Redacted/Unqualified; it does not add or remove native value attributes.
fn attribute_presence_matches(original: State, reparsed: State) -> bool {
    (original == State::NotRecorded) == (reparsed == State::NotRecorded)
}

pub(super) fn read(query_plan: Node<'_, '_>) -> Option<PlanParameterContext> {
    child(query_plan, "ParameterList")?;
    let lists = || children(query_plan, "ParameterList");
    let ambiguous = lists().take(2).count() > 1
        || query_plan.parent().is_some_and(|parent| children(parent, "QueryPlan").take(2).count() > 1);
    let runtime = !ambiguous && query_plan_has_runtime(query_plan);
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
            parameter.integer_values_equal = None;
        }
    }
    let supported_scope = query_plan.parent().is_some_and(super::relational_inputs::direct_statement_scope);
    if !supported_scope {
        issues.insert(Issue::UnsupportedUse);
    }
    let uses = if ambiguous || !supported_scope || issues.contains(&Issue::ParameterLimit) {
        vec![]
    } else {
        uses::read(query_plan, &parameters, &mut issues)
    };
    // BaseStmtInfoType declares RetrievedFromCache as an optional string. Only the engine's exact true/false
    // tokens on this selected actual statement are supported; estimated/unbound/ambiguous scopes stay unknown.
    let retrieved_from_cache = (runtime && supported_scope)
        .then(|| query_plan.parent().and_then(|statement| statement.attribute("RetrievedFromCache")))
        .flatten()
        .and_then(|value| match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        });
    Some(PlanParameterContext { retrieved_from_cache, parameters, uses, issues: issues.into_iter().collect() })
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
    let integer_values_equal = (comparison != Comparison::Unavailable)
        .then(|| integers::equal(data_type.as_deref(), compiled_value, runtime_value))
        .flatten();
    PlanParameter { name, data_type, compiled, runtime: runtime_state, comparison, integer_values_equal }
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
            parameter.integer_values_equal = None;
        }
    }
}

#[cfg(test)]
mod tests;
