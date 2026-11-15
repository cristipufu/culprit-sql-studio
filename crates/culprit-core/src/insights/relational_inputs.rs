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
    estimated_output: f64,
    actual_output: f64,
    difference: AccessDifference,
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
        let expected_output = node.est_rows * executions;
        let output = node.act_rows?;
        if ![estimated, actual, expected_output, output].into_iter().all(|value| value.is_finite() && value >= 0.0)
            || expected_output > estimated
            || output > actual
        {
            return None;
        }
        let volume = differs_materially(estimated, actual);
        let selectivity =
            estimated > 0.0 && actual > 0.0 && differs_materially(expected_output / estimated, output / actual);
        let difference = match (volume, selectivity) {
            (true, false) => AccessDifference::Volume,
            (false, true) => AccessDifference::Selectivity,
            (true, true) => AccessDifference::Both,
            (false, false) => AccessDifference::Undetermined,
        };
        Some(Self { estimated, actual, estimated_output: expected_output, actual_output: output, difference })
    }

    pub(super) fn detail(&self) -> String {
        let counts = t!(
            "insight.relational_input.access_counts",
            estimated = rows_text(self.estimated),
            actual = rows_text(self.actual)
        );
        let distinction = match self.difference {
            AccessDifference::Volume => t!("insight.relational_input.access_volume"),
            AccessDifference::Selectivity => t!("insight.relational_input.selectivity"),
            AccessDifference::Both => t!("insight.relational_input.access_and_selectivity"),
            AccessDifference::Undetermined => t!("insight.relational_input.access_undetermined"),
        };
        format!("{counts} {distinction}")
    }

    pub(super) fn investigation(&self) -> String {
        match self.difference {
            AccessDifference::Volume => t!("insight.relational_input.investigate_volume"),
            AccessDifference::Selectivity => t!("insight.relational_input.investigate_selectivity"),
            AccessDifference::Both | AccessDifference::Undetermined => t!("insight.relational_input.investigate_both"),
        }
    }

    fn underestimated(&self) -> bool {
        self.actual > self.estimated
            && self.actual >= crate::plan::MISESTIMATE_MIN_ROWS
            && differs_materially(self.estimated, self.actual)
    }

    fn output_detail(&self) -> String {
        t!(
            "insight.relational_input_access.output_counts",
            estimated = rows_text(self.estimated_output),
            actual = rows_text(self.actual_output)
        )
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
    concern.detail = format!("{} {}", access.detail(), access.output_detail());
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
