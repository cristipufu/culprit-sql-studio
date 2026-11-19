//! Original integer runtime attributes are compared while both selected native documents are borrowed.
use super::*;
use crate::plan_xml::{self, LocalName, StatementSelection, query_plan_of, select_native_statement};
use culprit_core::{
    Engine, Plan, PlanKind, PlanNode,
    plan_compare::{NativeRuntimeInputPair, NativeRuntimeInputRelation as Relation},
};
use roxmltree::Document;

const MAX_COMPARISON_XML_BYTES: usize = 32 * 1024 * 1024;
const MAX_COMPARISON_NODES: usize = 1024;

pub(in crate::plan_xml) fn compare<'a>(a: &'a Plan, b: &'a Plan) -> NativeRuntimeInputPair<'a> {
    let relation = relation(a, b).unwrap_or(Relation::Unavailable);
    NativeRuntimeInputPair::from_native(a, b, relation)
}

fn relation(a: &Plan, b: &Plan) -> Option<Relation> {
    if a.engine != Engine::Mssql || b.engine != a.engine || a.kind != PlanKind::Actual || b.kind != a.kind {
        return None;
    }
    let (raw_a, raw_b) = (a.raw.as_deref()?, b.raw.as_deref()?);
    if raw_a.len() > MAX_COMPARISON_XML_BYTES || raw_b.len() > MAX_COMPARISON_XML_BYTES {
        return None;
    }
    culprit_core::xml::read(raw_a, |document_a| {
        let document_a = document_a.ok()?;
        let statement_a = selected(&document_a, a)?;
        culprit_core::xml::read(raw_b, |document_b| {
            let document_b = document_b.ok()?;
            let statement_b = selected(&document_b, b)?;
            if statement_a.attribute("StatementText")?.trim() != statement_b.attribute("StatementText")?.trim()
                || !statement_a.attribute("QueryHash")?.eq_ignore_ascii_case(statement_b.attribute("QueryHash")?)
            {
                return None;
            }
            let a = runtime_parameters(query_plan_of(statement_a)?)?;
            let b = runtime_parameters(query_plan_of(statement_b)?)?;
            if a.len() != b.len() {
                return None;
            }
            let mut same = true;
            for (name, declaration, runtime) in &a {
                let (_, other_type, other_value) = b.iter().find(|(other, _, _)| other == name)?;
                if !declaration.eq_ignore_ascii_case(other_type) {
                    return None;
                }
                same &= integers::equal(Some(declaration), Some(runtime), Some(other_value))?;
            }
            Some(if same { Relation::Same } else { Relation::Different })
        })
    })
}

fn selected<'a, 'input>(document: &'a Document<'input>, supplied: &Plan) -> Option<Node<'a, 'input>> {
    if !document.root_element().has_tag_name_local("ShowPlanXML") {
        return None;
    }
    // A second native statement is an unavailable population, not an invitation to choose the highest cost/hash.
    let statement = select_native_statement(document, None).ok()?;
    let query_plan = query_plan_of(statement)?;
    let relations = culprit_core::sql::mssql_relational_references(statement.attribute("StatementText")?)?;
    let native_nodes =
        query_plan.descendants().filter(|node| node.has_tag_name_local("RelOp")).take(MAX_COMPARISON_NODES + 1).count();
    if !plan_xml::relational_inputs::direct_statement_scope(statement)
        || statement.attribute("QueryVariantID").is_some()
        || relations.len() != 1
        || !matches!(&relations[0].name, culprit_core::sql::MssqlRelationName::Named(parts)
            if parts.iter().all(|part| !part.starts_with(['@', '#'])))
        || children(statement, "QueryPlan").count() != 1
        || children(query_plan, "RelOp").count() != 1
        || native_nodes > MAX_COMPARISON_NODES
        || query_plan.descendants().any(|node| node != query_plan && node.has_tag_name_local("QueryPlan"))
        || query_plan.descendants().filter(Node::is_element).any(|node| {
            node.attribute("QueryVariantID").is_some()
                || matches!(
                    node.tag_name().name(),
                    "Dispatcher" | "ParameterSensitivePredicate" | "OptionalParameterPredicate"
                )
        })
        || query_plan.descendants().filter(|node| node.has_tag_name_local("RunTimeCountersPerThread")).any(|thread| {
            ["ActualLogicalReads", "ActualRowsRead"]
                .iter()
                .any(|name| thread.attribute(*name).is_some_and(|value| value.parse::<u64>().is_err()))
        })
    {
        return None;
    }
    let native =
        plan_xml::read_statement(document, supplied.raw.as_deref()?, StatementSelection::Native(None), None).ok()?;
    if native.kind != PlanKind::Actual
        || native.statement != supplied.statement
        || native.facts().query_hash != supplied.facts().query_hash
        || native.facts().parameter_context != supplied.facts().parameter_context
        || !same_native_tree(&native.root, &supplied.root)
    {
        return None;
    }
    let nodes = native.nodes();
    let mut ids = BTreeSet::new();
    (nodes.len() == native_nodes && nodes.iter().all(|node| ids.insert(node.id))).then_some(statement)
}

fn same_native_tree(native: &PlanNode, supplied: &PlanNode) -> bool {
    native.id == supplied.id
        && native.op == supplied.op
        && native.kind == supplied.kind
        && native.object == supplied.object
        && native.index == supplied.index
        && native.predicate == supplied.predicate
        && native.seek_predicate == supplied.seek_predicate
        && native.est_rows == supplied.est_rows
        && native.est_executions == supplied.est_executions
        && native.act_rows == supplied.act_rows
        && native.act_executions == supplied.act_executions
        && native.rows_read == supplied.rows_read
        && native.runtime == supplied.runtime
        && native.facts() == supplied.facts()
        && native.children.len() == supplied.children.len()
        && native.children.iter().zip(&supplied.children).all(|(native, supplied)| same_native_tree(native, supplied))
}

/// Borrow values only inside this native comparison; the returned evidence contains the relation alone.
fn runtime_parameters<'a, 'input>(query_plan: Node<'a, 'input>) -> Option<Vec<(&'a str, &'a str, &'a str)>> {
    let context = read(query_plan)?;
    if !context.is_valid()
        || context.parameters.iter().any(|parameter| parameter.runtime != State::Recorded)
        || context.issues.iter().any(|issue| *issue != Issue::UnsupportedUse)
        || query_plan.descendants().filter(|node| node.has_tag_name_local("ParameterList")).count() != 1
    {
        return None;
    }
    for reference in query_plan.descendants().filter(|node| {
        node.has_tag_name_local("ColumnReference")
            && node.attribute("Column").is_some_and(|name| name.starts_with('@'))
            && !node.ancestors().any(|parent| parent.has_tag_name_local("ParameterList"))
    }) {
        uses::parameter_index(reference, &context.parameters)?;
    }
    let mut lists = children(query_plan, "ParameterList");
    let list = lists.next()?;
    if lists.next().is_some() {
        return None;
    }
    let mut parameters = Vec::new();
    let mut names = BTreeSet::new();
    for native in list.children().filter(Node::is_element) {
        if !native.has_tag_name_local("ColumnReference")
            || parameters.len() == MAX_PLAN_PARAMETERS
            || ["Server", "Database", "Schema", "Table", "Alias"].iter().any(|name| native.attribute(*name).is_some())
        {
            return None;
        }
        let name = native.attribute("Column")?;
        let declaration = native.attribute("ParameterDataType")?;
        let runtime = native.attribute("ParameterRuntimeValue")?;
        if !parameter_name(name)
            || !names.insert(name.to_lowercase())
            || !integers::supports(Some(declaration))
            || value_state(Some(runtime)) != State::Recorded
            || integers::equal(Some(declaration), Some(runtime), Some(runtime)) != Some(true)
        {
            return None;
        }
        parameters.push((name, declaration, runtime));
    }
    (!parameters.is_empty() && parameters.len() == context.parameters.len()).then_some(parameters)
}

#[cfg(test)]
mod tests;
