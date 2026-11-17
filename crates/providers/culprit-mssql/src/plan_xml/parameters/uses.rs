//! Bind only direct native equality seek keys, never ScalarString text or another QueryPlan's parameters.
use super::super::LocalName;
use super::*;
use culprit_core::plan_facts::{MAX_PLAN_PARAMETER_USES, PlanParameterUse, PlanParameterUseKind};

pub(super) fn read(
    query_plan: Node<'_, '_>,
    parameters: &[PlanParameter],
    issues: &mut BTreeSet<Issue>,
) -> Vec<PlanParameterUse> {
    let nodes = query_plan.descendants().filter(|node| {
        node.has_tag_name_local("RelOp")
            && node.ancestors().skip(1).find(|parent| parent.has_tag_name_local("QueryPlan")) == Some(query_plan)
    });
    let mut identities = BTreeSet::new();
    let mut uses = Vec::new();
    for node in nodes {
        let Some(id) = node.attribute("NodeId").and_then(|value| value.parse::<u32>().ok()) else {
            issues.insert(Issue::AmbiguousUse);
            return vec![];
        };
        if !identities.insert(id) {
            issues.insert(Issue::AmbiguousUse);
            return vec![];
        }
        let references = own_parameter_references(node);
        if references.is_empty() {
            continue;
        }
        let binding = (references.len() == 1)
            .then(|| direct_seek_parameter(node))
            .flatten()
            .filter(|reference| *reference == references[0])
            .and_then(|reference| parameter_index(reference, parameters));
        let Some(parameter_index) = binding else {
            issues.insert(Issue::UnsupportedUse);
            continue;
        };
        uses.push(PlanParameterUse { parameter_index, node_id: id, kind: PlanParameterUseKind::EqualitySeek });
        if uses.len() > MAX_PLAN_PARAMETER_USES {
            issues.insert(Issue::UseLimit);
            return vec![];
        }
    }
    uses.sort_by_key(|usage| usage.node_id);
    uses
}

fn own_parameter_references<'a, 'input>(node: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    let mut pending = node.children().filter(Node::is_element).collect::<Vec<_>>();
    let mut references = Vec::new();
    while let Some(reference) = pending.pop() {
        if reference.has_tag_name_local("RelOp") || reference.has_tag_name_local("QueryPlan") {
            continue;
        }
        if reference.has_tag_name_local("ColumnReference")
            && reference.attribute("Column").is_some_and(|name| name.starts_with('@'))
        {
            references.push(reference);
            if references.len() == 2 {
                break;
            }
        }
        pending.extend(reference.children().filter(Node::is_element));
    }
    references
}

fn direct_seek_parameter<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    if !matches!(node.attribute("PhysicalOp"), Some("Index Seek" | "Clustered Index Seek")) {
        return None;
    }
    let scan = one(children(node, "IndexScan"))?;
    if scan.children().any(|child| {
        child.has_tag_name_local("Predicate")
            || child.has_tag_name_local("Residual")
            || child.has_tag_name_local("ProbeResidual")
    }) {
        return None;
    }
    let seeks = one(children(scan, "SeekPredicates"))?;
    let predicate = only_element(seeks, "SeekPredicateNew")?;
    let keys = only_element(predicate, "SeekKeys")?;
    let prefix = only_element(keys, "Prefix")?;
    if prefix.attribute("ScanType") != Some("EQ") || prefix.children().filter(Node::is_element).count() != 2 {
        return None;
    }
    let columns = one(children(prefix, "RangeColumns"))?;
    only_element(columns, "ColumnReference")?;
    let expressions = one(children(prefix, "RangeExpressions"))?;
    let scalar = only_element(expressions, "ScalarOperator")?;
    let identifier = only_element(scalar, "Identifier")?;
    only_element(identifier, "ColumnReference")
}

fn parameter_index(reference: Node<'_, '_>, parameters: &[PlanParameter]) -> Option<u16> {
    if ["Server", "Database", "Schema", "Table", "Alias"].iter().any(|name| reference.attribute(*name).is_some()) {
        return None;
    }
    let name = reference.attribute("Column")?;
    let (index, _) =
        one(parameters.iter().enumerate().filter(|(_, parameter)| parameter.name.as_deref() == Some(name)))?;
    index.try_into().ok()
}

fn only_element<'a, 'input>(parent: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    one(parent.children().filter(Node::is_element)).filter(|node| node.has_tag_name_local(name))
}

fn one<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

#[cfg(test)]
mod tests;
