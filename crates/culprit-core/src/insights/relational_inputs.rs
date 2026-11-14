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
        Some(Self { estimated, actual, difference })
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
}

fn differs_materially(left: f64, right: f64) -> bool {
    let smaller = left.min(right);
    let larger = left.max(right);
    larger > 0.0 && (smaller == 0.0 || larger / smaller >= crate::plan::MISESTIMATE_FACTOR)
}
