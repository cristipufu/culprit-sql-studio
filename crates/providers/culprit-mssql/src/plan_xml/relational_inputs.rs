//! Classify only native facts that distinguish relational input kinds. Showplan's Object and ParameterList do not
//! distinguish a TVP from a typed local variable; `@` and ParameterDataType therefore never establish either kind.
//! Native schema: https://schemas.microsoft.com/sqlserver/2004/07/showplan/sql2019/showplanxml.xsd

use culprit_core::{
    PlanNode,
    plan_facts::{RelationalInput, RelationalInputKind as Kind, RelationalInputProvenance as Provenance},
};
use roxmltree::Node;

use super::LocalName;

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

/// Only preceding declarations in the very same native statement list can qualify a local input. Declarations in
/// another batch, procedure, conditional branch or dynamic SQL are not in this scope. Retained single-statement
/// plans commonly omit declarations, and then their variable-backed inputs deliberately remain unknown.
pub(super) fn qualify_local_declarations(root: &mut PlanNode, statement: Node<'_, '_>) {
    if !direct_statement_scope(statement) {
        return;
    }
    let Some(name) = statement.attribute("StatementText").and_then(culprit_core::sql::mssql_direct_table_parameter)
    else {
        return;
    };
    let Some(scope) = statement.parent().filter(|node| node.has_tag_name_local("Statements")) else { return };
    let declarations = scope
        .children()
        .filter(Node::is_element)
        .take_while(|node| *node != statement)
        .filter(|node| node.has_tag_name_local("StmtSimple"))
        .filter_map(|node| node.attribute("StatementText").and_then(culprit_core::sql::mssql_declared_table_variable))
        .filter(|declared| declared == &name)
        .count();
    if declarations == 1 {
        apply_direct_input(
            root,
            &name,
            RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration },
        );
    }
}

/// The selected SQL must have one direct relational parameter and no other expansion scope. Showplan Object's
/// TableReferenceId is a local reference, not a documented module/parameter owner ID. Do not infer that binding.
pub(super) fn qualify_module_parameters(
    root: &mut PlanNode,
    statement: Node<'_, '_>,
    context: &culprit_core::telemetry::CachedStatementContext,
    parameters: &culprit_core::telemetry::MssqlModuleParameters,
    native_statement: &str,
) {
    if !parameters.is_valid_for(context)
        || !statement.has_tag_name_local("StmtSimple")
        || statement.attribute("StatementText").map(str::trim) != Some(native_statement.trim())
        || !statement
            .attribute("QueryHash")
            .zip(context.query_hash.as_deref())
            .is_some_and(|(native, observed)| native.eq_ignore_ascii_case(observed))
        || !direct_statement_scope(statement)
    {
        return;
    }
    let Some(name) = culprit_core::sql::mssql_direct_table_parameter(native_statement) else { return };
    let Some(input) = parameters.input(context, &name) else { return };
    apply_direct_input(root, &name, input);
}

fn apply_direct_input(node: &mut PlanNode, name: &str, input: RelationalInput) {
    if matches!(node.kind, culprit_core::NodeKind::Scan | culprit_core::NodeKind::Seek)
        && node.object.as_deref() == Some(name)
        && node.facts.relational_input.is_some_and(|old| old.qualified_kind() == Kind::Unknown)
    {
        node.facts.relational_input = Some(input);
    }
    for child in &mut node.children {
        apply_direct_input(child, name, input);
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
