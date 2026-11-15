//! Discriminate native access volume from rows kept without treating operator output as parameter cardinality.
use super::*;

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
}

impl AccessComparison {
    pub(super) fn from_node(node: &PlanNode) -> Option<Self> {
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
        Some(Self { estimated, actual, estimated_output, actual_output, difference })
    }

    pub(super) fn detail(&self) -> String {
        let counts = t!(
            "insight.relational_input.access_counts",
            estimated = rows_text(self.estimated),
            actual = rows_text(self.actual)
        );
        let distinction = match self.difference {
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
            Some(AccessDifference::Volume) => t!("insight.relational_input.investigate_volume"),
            Some(AccessDifference::Selectivity) => t!("insight.relational_input.investigate_selectivity"),
            Some(AccessDifference::Both | AccessDifference::Undetermined) | None => {
                t!("insight.relational_input.investigate_both")
            }
        }
    }

    fn underestimated(&self) -> bool {
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
            let access = AccessComparison::from_node(node)?;
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
