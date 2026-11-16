//! Classify only native facts that distinguish relational input kinds. Showplan's Object and ParameterList do not
//! distinguish a TVP from a typed local variable; `@` and ParameterDataType therefore never establish either kind.
//! Native schema: https://schemas.microsoft.com/sqlserver/2004/07/showplan/sql2019/showplanxml.xsd

use culprit_core::{
    PlanNode, Result,
    plan_facts::{RelationalInput, RelationalInputKind as Kind, RelationalInputProvenance as Provenance},
};
use roxmltree::Node;
use std::collections::BTreeMap;

use super::LocalName;
mod bindings;

pub(super) fn from_operator(operator: Node<'_, '_>) -> Option<RelationalInput> {
    if operator.has_tag_name_local("TableValuedFunction") {
        return Some(RelationalInput { kind: Kind::TableValuedFunction, provenance: Provenance::NativePlan });
    }
    let object = super::child(operator, "Object")?;
    let name = object.attribute("Table").map(super::unbracket)?;
    if name.starts_with('@') {
        Some(RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved })
    } else if name.starts_with('#') {
        Some(if object.attribute("Database").map(super::unbracket).as_deref() == Some("tempdb") {
            RelationalInput { kind: Kind::TemporaryTable, provenance: Provenance::NativePlan }
        } else {
            RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved }
        })
    } else {
        None
    }
}

/// Bind the complete original relation inventory before assigning any variable kind. Native Object aliases and
/// NodeIds select the operator; neither a token elsewhere in SQL nor a detached display name establishes ownership.
pub(super) fn qualify(
    root: &mut PlanNode,
    statement: Node<'_, '_>,
    context: Option<&super::CachedInputContext<'_>>,
) -> Result<()> {
    let inputs = classify(statement, context)?;
    apply_inputs(root, &inputs);
    Ok(())
}

pub(super) fn classify(
    statement: Node<'_, '_>,
    context: Option<&super::CachedInputContext<'_>>,
) -> Result<BTreeMap<u32, RelationalInput>> {
    let proof = context.and_then(|context| context.origin.retained_proof());
    let namespace = if proof.is_some() {
        bindings::Namespace::Retained
    } else if let Some(target) = context.and_then(|context| context.origin.original_target()) {
        bindings::Namespace::Original(target)
    } else {
        bindings::Namespace::Unknown
    };
    let bound = direct_statement_scope(statement).then(|| bindings::parameters(statement, namespace)).flatten();
    let inputs: BTreeMap<_, _> = bound
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, name)| {
            let local = local_declaration(statement, &name);
            let parameter = context.and_then(|context| module_parameter(statement, context, &name));
            match (local, parameter) {
                (Some(input), None) | (None, Some(input)) => Some((id, input)),
                _ => None,
            }
        })
        .collect();
    if let Some(proof) = proof {
        let observed = inputs
            .iter()
            .map(|(node_id, input)| culprit_core::telemetry::MssqlRelationalInputBinding {
                node_id: *node_id,
                input: *input,
            })
            .collect::<Vec<_>>();
        if !proof.is_valid() || proof.bindings != observed {
            return Err(culprit_core::telemetry::DeliveryProblem::Integrity.error());
        }
    }
    Ok(inputs)
}

/// Only preceding declarations in the very same native statement list can qualify a local input. Declarations in
/// another batch, procedure, conditional branch or dynamic SQL are not in this scope.
fn local_declaration(statement: Node<'_, '_>, name: &str) -> Option<RelationalInput> {
    let scope = statement.parent().filter(|node| node.has_tag_name_local("Statements"))?;
    let declarations = scope
        .children()
        .filter(Node::is_element)
        .take_while(|node| *node != statement)
        .filter(|node| node.has_tag_name_local("StmtSimple"))
        .filter_map(|node| node.attribute("StatementText").and_then(culprit_core::sql::mssql_declared_table_variable))
        .filter(|declared| declared.as_str() == name)
        .count();
    (declarations == 1)
        .then_some(RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration })
}

/// Original module proof remains useful without a reader namespace for a closed all-variable query. Namespace-
/// sensitive named relations have already been refused by the shared binding pass when that context is absent.
fn module_parameter(
    statement: Node<'_, '_>,
    context: &super::CachedInputContext<'_>,
    name: &str,
) -> Option<RelationalInput> {
    let parameters = context.module_parameters?;
    let native_statement = context.text?;
    if !parameters.is_valid_for(context.statement)
        || !statement.has_tag_name_local("StmtSimple")
        || statement.attribute("StatementText").map(str::trim) != Some(native_statement.trim())
        || !statement
            .attribute("QueryHash")
            .zip(context.statement.query_hash.as_deref())
            .is_some_and(|(native, observed)| native.eq_ignore_ascii_case(observed))
    {
        return None;
    }
    parameters.input(context.statement, name)
}

fn apply_inputs(node: &mut PlanNode, inputs: &BTreeMap<u32, RelationalInput>) {
    if matches!(node.kind, culprit_core::NodeKind::Scan | culprit_core::NodeKind::Seek)
        && node.facts.relational_input.is_some_and(|old| old.qualified_kind() == Kind::Unknown)
        && let Some(input) = inputs.get(&node.id)
    {
        node.facts.relational_input = Some(*input);
    }
    for child in &mut node.children {
        apply_inputs(child, inputs);
    }
}

fn direct_statement_scope(statement: Node<'_, '_>) -> bool {
    let function_scope = |node: Node<'_, '_>| {
        matches!(
            node.tag_name().name(),
            "UDF" | "StoredProc" | "UserDefinedFunction" | "UserDefinedAggregate" | "TableValuedFunction"
        )
    };
    if statement.ancestors().filter(Node::is_element).any(|node| {
        !matches!(node.tag_name().name(), "StmtSimple" | "Statements" | "Batch" | "BatchSequence" | "ShowPlanXML")
    }) {
        return false;
    }
    !statement.descendants().filter(Node::is_element).any(|node| {
        function_scope(node)
            || node != statement
                && (node.has_tag_name_local("Statements") || node.tag_name().name().starts_with("Stmt"))
            || node.attribute("ContainsInlineScalarTsqlUdfs").is_some_and(|value| !matches!(value, "0" | "false"))
            || node.has_tag_name_local("InternalInfo")
                && node.descendants().any(|descendant| descendant.has_tag_name_local("RelOp"))
    })
}

#[cfg(test)]
mod tests;
