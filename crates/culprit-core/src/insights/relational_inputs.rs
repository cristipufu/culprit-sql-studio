//! Discriminate native access volume from rows kept without treating operator output as parameter cardinality.
use super::*;

mod grant;
pub(super) use grant::oversized_grant_input;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessDifference {
    Volume,
    Selectivity,
    Both,
    Undetermined,
}

pub(super) struct AccessComparison {
    estimated: f64,
    actual: f64,
    estimated_output: Option<f64>,
    actual_output: Option<f64>,
    difference: Option<AccessDifference>,
    completed_input: bool,
}

impl AccessComparison {
    pub(super) fn from_node(cx: &Cx, node: &PlanNode) -> Option<Self> {
        let access = node.facts().access_rows.filter(|access| access.is_valid())?;
        let executions =
            node.act_executions.filter(|value| value.is_finite() && *value >= 1.0 && value.fract() == 0.0)?;
        // Worker totals remain useful raw evidence, but repeated parallel execution scope is not established by
        // summing worker invocation counts. Do not normalize that population as serial operator executions.
        if !matches!(node.kind, NodeKind::Scan | NodeKind::Seek) || node.facts().parallel_threads.is_some() {
            return None;
        }
        let estimated = access.estimated_per_execution? * executions;
        let actual = access.actual_total?;
        let estimated_output = (!node.facts().estimated_rows_unavailable).then_some(node.est_rows * executions);
        let actual_output = node.act_rows;
        let invalid_output = |output: Option<f64>, accessed: f64| {
            output.is_some_and(|value| !value.is_finite() || value < 0.0 || value > accessed)
        };
        if ![estimated, actual].into_iter().all(|value| value.is_finite() && value >= 0.0)
            || invalid_output(estimated_output, estimated)
            || invalid_output(actual_output, actual)
        {
            return None;
        }
        let difference = estimated_output.zip(actual_output).map(|(expected, output)| {
            let volume = differs_materially(estimated, actual);
            let selectivity =
                estimated > 0.0 && actual > 0.0 && differs_materially(expected / estimated, output / actual);
            match (volume, selectivity) {
                (true, false) => AccessDifference::Volume,
                (false, true) => AccessDifference::Selectivity,
                (true, true) => AccessDifference::Both,
                (false, false) => AccessDifference::Undetermined,
            }
        });
        let completed_input = completed_tvp_input(cx, node, actual, actual_output);
        Some(Self { estimated, actual, estimated_output, actual_output, difference, completed_input })
    }

    pub(super) fn output_counts(&self) -> Option<(f64, f64)> {
        self.estimated_output.zip(self.actual_output)
    }

    pub(super) fn actual_accessed(&self) -> f64 {
        self.actual
    }

    pub(super) fn overestimated_complete_input(&self) -> bool {
        self.completed_input
            && self.estimated >= crate::plan::MISESTIMATE_MIN_ROWS
            && self.estimated > self.actual
            && differs_materially(self.estimated, self.actual)
    }

    pub(super) fn detail(&self) -> String {
        let counts = if self.completed_input {
            t!(
                "insight.relational_input.completed_input_counts",
                estimated = rows_text(self.estimated),
                actual = rows_text(self.actual)
            )
        } else {
            t!(
                "insight.relational_input.access_counts",
                estimated = rows_text(self.estimated),
                actual = rows_text(self.actual)
            )
        };
        let distinction = match self.difference {
            Some(AccessDifference::Volume) if self.completed_input => {
                t!("insight.relational_input.completed_input_volume")
            }
            Some(AccessDifference::Undetermined) if self.completed_input => {
                t!("insight.relational_input.completed_input_undetermined")
            }
            Some(AccessDifference::Volume) => t!("insight.relational_input.access_volume"),
            Some(AccessDifference::Selectivity) => t!("insight.relational_input.selectivity"),
            Some(AccessDifference::Both) => t!("insight.relational_input.access_and_selectivity"),
            Some(AccessDifference::Undetermined) => t!("insight.relational_input.access_undetermined"),
            None => self.unavailable_output_detail(),
        };
        format!("{counts} {distinction}")
    }

    pub(super) fn investigation(&self) -> String {
        match self.difference {
            Some(AccessDifference::Volume) if self.completed_input => {
                t!("insight.relational_input.investigate_completed_input")
            }
            Some(AccessDifference::Volume) => t!("insight.relational_input.investigate_volume"),
            Some(AccessDifference::Selectivity) => t!("insight.relational_input.investigate_selectivity"),
            Some(AccessDifference::Both | AccessDifference::Undetermined) | None => {
                t!("insight.relational_input.investigate_both")
            }
        }
    }

    pub(super) fn underestimated(&self) -> bool {
        self.actual > self.estimated
            && self.actual >= crate::plan::MISESTIMATE_MIN_ROWS
            && differs_materially(self.estimated, self.actual)
    }

    fn output_detail(&self) -> Option<String> {
        let (estimated, actual) = self.estimated_output.zip(self.actual_output)?;
        Some(t!(
            "insight.relational_input_access.output_counts",
            estimated = rows_text(estimated),
            actual = rows_text(actual)
        ))
    }

    pub(super) fn root_output_detail(&self, cx: &Cx, node: &PlanNode) -> Option<String> {
        (self.completed_input && cx.plan.root.id != node.id)
            .then_some(cx.plan.root.act_rows)
            .flatten()
            .filter(|value| exact_observed_rows(*value))
            .map(|rows| t!("insight.relational_input.root_output", actual = rows_text(rows)))
    }

    fn unavailable_output_detail(&self) -> String {
        match (self.estimated_output, self.actual_output) {
            (Some(estimated), None) => {
                t!("insight.relational_input_access.actual_output_unavailable", estimated = rows_text(estimated))
            }
            (None, Some(actual)) => {
                t!("insight.relational_input_access.estimated_output_unavailable", actual = rows_text(actual))
            }
            _ => t!("insight.relational_input_access.output_unavailable"),
        }
    }
}

fn exact_observed_rows(rows: f64) -> bool {
    rows.is_finite() && rows >= 0.0 && rows <= (1u64 << 53) as f64 && rows.fract() == 0.0
}

/// EOF proves only this native operator exhausted its stream. Input size additionally needs one complete serial
/// TVP table scan, actual before-filter counts, and an enclosing path without repetition or early-stop semantics.
fn completed_tvp_input(cx: &Cx, node: &PlanNode, accessed: f64, output: Option<f64>) -> bool {
    use crate::plan_facts::RelationalInputKind;
    let mut identities = std::collections::BTreeSet::new();
    cx.plan.kind == PlanKind::Actual
        && cx.nodes.iter().all(|node| identities.insert(node.id))
        && node
            .facts()
            .relational_input
            .is_some_and(|input| input.qualified_kind() == RelationalInputKind::TableValuedParameter)
        && node.kind == NodeKind::Scan
        && node.index.is_none()
        && node.seek_predicate.is_none()
        && node.children.is_empty()
        && node.act_executions == Some(1.0)
        && node.facts().estimated_rows_without_goal.is_none()
        && node.facts().scan_completion.as_ref().is_some_and(|completion| completion.is_single_completed_serial_scan())
        && exact_observed_rows(accessed)
        && output.is_some_and(exact_observed_rows)
        && cx.ancestors(node).all(|parent| {
            matches!(parent.kind, NodeKind::Sort | NodeKind::Aggregate | NodeKind::Compute)
                && parent.facts().estimated_rows_without_goal.is_none()
                && serial_executions(parent) == Some(1.0)
        })
}

const MAX_EXACT_COUNT: u64 = (1 << 53) - 1;

/// The source-issued count is exact native text. Binary64 totals are comparable only within its integer range,
/// and an embedder's contradictory worker/output facts cannot turn it into stronger serial evidence.
pub(super) fn serial_executions(node: &PlanNode) -> Option<f64> {
    let native = node.facts().serial_execution_count.as_deref()?;
    let count = native.parse::<u64>().ok()?;
    (count > 0
        && count <= MAX_EXACT_COUNT
        && count.to_string() == native
        && node.act_executions == Some(count as f64)
        && node.facts().parallel_threads.is_none()
        && node.act_rows.is_some_and(exact_count))
    .then_some(count as f64)
}

pub(super) fn exact_count(value: f64) -> bool {
    value.is_finite() && value >= 0.0 && value <= MAX_EXACT_COUNT as f64 && value.fract() == 0.0
}

const MAX_ACCESS_CONCERNS: usize = 2;

/// Output can match its estimate while unexpectedly high access is offset by a lower kept fraction. Only actual
/// underestimation is evidence here: fewer accessed rows can follow early termination of a correctly estimated run.
pub(super) fn access_misestimates(cx: &Cx, output_concerns: &[Draft]) -> Vec<Draft> {
    if cx.plan.kind != PlanKind::Actual {
        return Vec::new();
    }
    let mut candidates: Vec<_> = cx
        .nodes
        .iter()
        .copied()
        .filter(|node| !output_concerns.iter().any(|concern| concern.nodes.contains(&node.id)))
        .filter_map(|node| {
            let kind = relational_input_kind(node)?;
            let access = AccessComparison::from_node(cx, node)?;
            access.underestimated().then_some((node, kind, access))
        })
        .collect();
    candidates.sort_by(|left, right| right.2.actual.total_cmp(&left.2.actual));
    candidates
        .into_iter()
        .take(MAX_ACCESS_CONCERNS)
        .map(|(node, kind, access)| access_concern(cx, node, kind, &access))
        .collect()
}

fn access_concern(
    cx: &Cx,
    node: &PlanNode,
    kind: crate::plan_facts::RelationalInputKind,
    access: &AccessComparison,
) -> Draft {
    let input = what(node);
    let mut concern = Draft::new("relational-input-access", InsightArea::Statistics, cx.share(node), vec![node.id]);
    concern.cap = Severity::Medium;
    concern.title = t!(
        "insight.relational_input_access.title",
        input = &input,
        estimated = rows_text(access.estimated),
        actual = rows_text(access.actual)
    );
    let mut detail = vec![access.detail()];
    detail.extend(access.output_detail());
    detail.extend(access.root_output_detail(cx, node));
    concern.detail = detail.join(" ");
    concern.fix =
        format!("{} {}", access.investigation(), statistics::relational_input_advice(kind, &table_leaf(&input)));
    concern.verify = t!("insight.relational_input.verify");
    concern
}

fn differs_materially(left: f64, right: f64) -> bool {
    let smaller = left.min(right);
    let larger = left.max(right);
    larger > 0.0 && (smaller == 0.0 || larger / smaller >= crate::plan::MISESTIMATE_FACTOR)
}
