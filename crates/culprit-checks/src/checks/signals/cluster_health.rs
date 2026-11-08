use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::ClusterHealth;
use culprit_core::{Actionability, t};

pub fn cluster_health_info() -> CheckInfo {
    CheckInfo {
        id: "cluster-health".into(),
        title: t!("check.cluster_health.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mysql],
        requires: vec![],
        description: t!("check.cluster_health.info.description"),
    }
}

check!(
    ClusterHealthCheck: cluster_health_info,
    decide = cluster_node,
    render = cluster_health_finding,
    case<'s> = ClusterNode<'s>,
    evidence = &[Evidence::Observed(Observation::Cluster, &[culprit_core::source_id::CLUSTER_HEALTH])],
);

/// A Galera node that says it can't serve, or that recorded a tenth or more of its time paused for flow control.
pub struct ClusterNode<'s> {
    health: &'s ClusterHealth,
    /// Not ready, disconnected or outside the primary component, by its own account.
    unavailable: bool,
    paused: Option<f64>,
}

fn cluster_node(s: &Snapshot) -> Vec<ClusterNode<'_>> {
    let Some(c) = s.operations.cluster.as_ref() else { return vec![] };
    let unavailable = c.ready == Some(false) || c.connected == Some(false) || c.primary_component == Some(false);
    let paused = c.flow_control_paused.filter(|p| p.is_finite() && (0.0..=1.0).contains(p));
    if !unavailable && paused.is_none_or(|p| p < 0.1) {
        return vec![];
    }
    vec![ClusterNode { health: c, unavailable, paused }]
}

fn cluster_health_finding(s: &Snapshot, node: ClusterNode<'_>) -> Finding {
    let c = node.health;
    let mut f = finding(
        "cluster-health",
        &s.server.database,
        Category::Configuration,
        subject(SubjectKind::Database, &s.server.database),
    );
    f.title = if node.unavailable {
        t!("check.cluster_health.title.unavailable")
    } else {
        t!("check.cluster_health.title.flow_control")
    };
    f.summary = [
        t!(
            "check.cluster_health.summary.status",
            ready = flag(c.ready),
            connected = flag(c.connected),
            primary = flag(c.primary_component)
        ),
        match c.local_state.as_deref() {
            Some(state) => t!("check.cluster_health.summary.local_state", state = state),
            None => t!("check.cluster_health.summary.local_state_unknown"),
        },
        if node.unavailable {
            t!("check.cluster_health.summary.unavailable")
        } else {
            t!("check.cluster_health.summary.flow_control")
        },
    ]
    .join(" ");
    f.severity = if node.unavailable { Severity::Medium } else { Severity::Low };
    f.actionability = if node.unavailable { Actionability::NeedsAttention } else { Actionability::Information };
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(if node.unavailable {
        t!("check.cluster_health.impact.unavailable")
    } else {
        t!("check.cluster_health.impact.flow_control")
    });
    if let Some(p) = node.paused {
        f.metrics.push(Metric::said(culprit_core::key!("check.cluster_health.metric.paused"), p, Unit::Ratio));
    }
    f.fix = Some(Fix { summary: t!("check.cluster_health.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.cluster_health.verify")];
    f
}

/// A status the node reports, as the summary's `select` reads it.
fn flag(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}
