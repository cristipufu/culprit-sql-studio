//! One retained conversion supplies its native verdict, expression and operator relationship together.
use super::conditions;
use super::predicates::{
    column_reference, convert_calls, current_conversions, native_conversion_owner, native_conversion_references,
};
use crate::{Engine, NodeKind, Plan, PlanConversion, PlanConversionIssue, PlanNode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionEffect {
    BlocksSeek,
    SeekObserved,
    Estimates,
    OtherWarning,
    PerRow,
}

impl ConversionEffect {
    pub fn warning_title(self, column: &str) -> Option<String> {
        match self {
            Self::SeekObserved => Some(crate::t!("insight.implicit_conversion.title.seek_observed", column = column)),
            Self::OtherWarning => Some(crate::t!("insight.implicit_conversion.title.other_warning", column = column)),
            _ => None,
        }
    }
    pub fn consequence(self) -> Option<String> {
        match self {
            Self::SeekObserved => Some(crate::t!("insight.implicit_conversion.consequence.seek_observed")),
            Self::OtherWarning => Some(crate::t!("insight.implicit_conversion.consequence.other_warning")),
            _ => None,
        }
    }
}

pub struct ConversionEvidence<'a> {
    node: Option<&'a PlanNode>,
    issue: Option<PlanConversionIssue>,
    expression: String,
    column: String,
    target: String,
}

impl<'a> ConversionEvidence<'a> {
    pub fn node(&self) -> Option<&'a PlanNode> {
        self.node
    }
    pub fn expression(&self) -> &str {
        &self.expression
    }
    pub fn column(&self) -> &str {
        &self.column
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn has_native_warning(&self) -> bool {
        self.issue.is_some()
    }
    pub fn blocks_seek(&self) -> bool {
        self.effect() == ConversionEffect::BlocksSeek
    }
    pub fn effect(&self) -> ConversionEffect {
        match &self.issue {
            Some(PlanConversionIssue::SeekPlan) if self.node.is_some_and(|node| node.kind == NodeKind::Seek) => {
                ConversionEffect::SeekObserved
            }
            Some(PlanConversionIssue::SeekPlan) => ConversionEffect::BlocksSeek,
            Some(PlanConversionIssue::CardinalityEstimate) => ConversionEffect::Estimates,
            Some(PlanConversionIssue::Other(_)) => ConversionEffect::OtherWarning,
            None => ConversionEffect::PerRow,
        }
    }
}

/// Statement and operator warnings share one selection. An ambiguous owner keeps the engine's verdict without
/// borrowing a different operator's access path. Unwarned conditions retain only their observed per-row conversion.
pub fn conversion_evidence(plan: &Plan) -> Option<ConversionEvidence<'_>> {
    if plan.engine != Engine::Mssql {
        return None;
    }
    let nodes = plan.nodes();
    let mut identities = std::collections::BTreeSet::new();
    let unique_ids = nodes.iter().all(|node| identities.insert(node.id));
    let warnings: Vec<_> = plan
        .facts()
        .conversions
        .iter()
        .chain(nodes.iter().flat_map(|node| &node.facts().conversions))
        .filter(|warning| !current_conversions(plan, &warning.expression).is_empty())
        .collect();
    if let Some(warning) =
        warnings.iter().find(|warning| warning.issue == PlanConversionIssue::SeekPlan).or(warnings.first())
    {
        let (column, target) = current_conversions(plan, &warning.expression).into_iter().next()?;
        return Some(ConversionEvidence {
            node: unique_ids.then(|| warning_owner(plan, &nodes, warning, &column, &target)).flatten(),
            issue: Some(warning.issue.clone()),
            expression: warning.expression.clone(),
            column,
            target,
        });
    }
    let node = nodes
        .into_iter()
        .filter(|node| !current_conversions(plan, &conditions(node)).is_empty())
        .max_by(|left, right| left.self_cost.total_cmp(&right.self_cost))?;
    let expression = conditions(node);
    let (column, target) = current_conversions(plan, &expression).into_iter().next()?;
    Some(ConversionEvidence { node: unique_ids.then_some(node), issue: None, expression, column, target })
}

fn warning_owner<'a>(
    plan: &Plan,
    nodes: &[&'a PlanNode],
    warning: &PlanConversion,
    column: &str,
    target: &str,
) -> Option<&'a PlanNode> {
    let declared: Vec<_> = nodes.iter().copied().filter(|node| node.facts().conversions.contains(warning)).collect();
    if !declared.is_empty() {
        return sole(declared.into_iter());
    }
    let operands: Vec<_> = convert_calls(&warning.expression)
        .into_iter()
        .filter(|(to, operand)| {
            *to == target && column_reference(plan, operand).is_some_and(|reference| reference.label == column)
        })
        .filter_map(|(_, operand)| scoped_references(plan, operand, None))
        .flatten()
        .collect();
    sole(nodes.iter().copied().filter(|node| {
        convert_calls(&conditions(node)).into_iter().any(|(to, operand)| {
            to == target
                && scoped_references(plan, operand, Some(node))
                    .is_some_and(|references| references.iter().any(|reference| operands.contains(reference)))
        })
    }))
}

fn scoped_references(plan: &Plan, operand: &str, node: Option<&PlanNode>) -> Option<Vec<Vec<String>>> {
    let references = native_conversion_references(plan, operand)?;
    if references.len() > 1 {
        let owner = native_conversion_owner(plan, operand)?;
        if node.is_some_and(|node| owner.id != node.id) {
            return None;
        }
    }
    Some(references)
}

fn sole<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let value = values.next()?;
    values.next().is_none().then_some(value)
}
