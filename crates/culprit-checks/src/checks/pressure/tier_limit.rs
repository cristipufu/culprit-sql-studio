//! Measured Azure SQL Database resource use near its tier's limit, over the available selected-period samples.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn tier_limit_info() -> CheckInfo {
    CheckInfo {
        id: "tier-limit".into(),
        title: t!("check.tier_limit.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.tier_limit.info.description"),
    }
}

check!(
    TierLimitCheck: tier_limit_info,
    decide = tier_limits,
    render = tier_limit_finding,
    case<'s> = TierLimit,
    evidence = &[Evidence::Pressure { signal: None, timed_files: false, complete: false }],
);

/// The tier-limit findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn tier_limit(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&TierLimitCheck, snapshot)
}

/// A resource Azure caps per tier.
#[derive(Clone, Copy)]
enum Resource {
    LogWrite,
    Cpu,
    DataIo,
    Workers,
}

impl Resource {
    /// In the order their findings are listed.
    const ALL: [Resource; 4] = [Resource::LogWrite, Resource::Cpu, Resource::DataIo, Resource::Workers];

    /// Its finding's key.
    fn key(self) -> &'static str {
        match self {
            Resource::LogWrite => "log-write",
            Resource::Cpu => "cpu",
            Resource::DataIo => "data-io",
            Resource::Workers => "workers",
        }
    }

    fn pick(self) -> super::health::ResourcePick {
        match self {
            Resource::LogWrite => |s| s.log_write_pct,
            Resource::Cpu => |s| s.cpu_pct,
            Resource::DataIo => |s| s.data_io_pct,
            Resource::Workers => |s| s.workers_pct,
        }
    }
}

/// A resource persistently near its cap over the selected period's samples.
pub struct TierLimit {
    resource: Resource,
    /// The share of samples at 95% of the cap or more.
    at_limit: f64,
    /// The highest sample, in percent of the cap.
    peak: f64,
    /// Hours the samples cover.
    hours: f64,
    /// High when it is at its cap in 30% of the samples or more.
    severity: Severity,
}

fn tier_limits(snapshot: &Snapshot) -> Vec<TierLimit> {
    let samples = &snapshot.resources;
    if samples.len() < 8 {
        return Vec::new();
    }
    Resource::ALL
        .into_iter()
        .filter_map(|resource| {
            let usage = super::health::resource_usage(samples, resource.pick())?;
            if !usage.measured || usage.at_limit < 0.10 {
                return None;
            }
            Some(TierLimit {
                resource,
                at_limit: usage.at_limit,
                peak: usage.peak,
                hours: usage.hours,
                severity: if usage.at_limit >= 0.3 { Severity::High } else { Severity::Medium },
            })
        })
        .collect()
}

fn tier_limit_finding(snapshot: &Snapshot, l: TierLimit) -> Finding {
    let mut f = finding(
        "tier-limit",
        l.resource.key(),
        Category::Configuration,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let span = culprit_core::last_span(l.hours);
    let share = format::percent(l.at_limit);
    let (name, caps, effect, fix, verify) = match l.resource {
        Resource::LogWrite => (
            t!("check.tier_limit.log_write.name"),
            t!("check.tier_limit.log_write.caps"),
            t!("check.tier_limit.log_write.effect"),
            t!("check.tier_limit.log_write.fix"),
            t!(
                "check.tier_limit.log_write.verify",
                health = culprit_core::t!("core.dashboard.tile.health"),
                dashboard = culprit_core::t!("ui.common.view.dashboard")
            ),
        ),
        Resource::Cpu => (
            t!("check.tier_limit.cpu.name"),
            t!("check.tier_limit.cpu.caps"),
            t!("check.tier_limit.cpu.effect"),
            t!(
                "check.tier_limit.cpu.fix",
                waits = culprit_core::t!("core.dashboard.tile.waits"),
                cpu = culprit_core::t!("core.waits.category.cpu.label")
            ),
            t!(
                "check.tier_limit.cpu.verify",
                health = culprit_core::t!("core.dashboard.tile.health"),
                dashboard = culprit_core::t!("ui.common.view.dashboard")
            ),
        ),
        Resource::DataIo => (
            t!("check.tier_limit.data_io.name"),
            t!("check.tier_limit.data_io.caps"),
            t!("check.tier_limit.data_io.effect"),
            t!("check.tier_limit.data_io.fix"),
            t!(
                "check.tier_limit.data_io.verify",
                health = culprit_core::t!("core.dashboard.tile.health"),
                dashboard = culprit_core::t!("ui.common.view.dashboard")
            ),
        ),
        Resource::Workers => (
            t!("check.tier_limit.workers.name"),
            t!("check.tier_limit.workers.caps"),
            t!("check.tier_limit.workers.effect"),
            t!("check.tier_limit.workers.fix"),
            t!(
                "check.tier_limit.workers.verify",
                health = culprit_core::t!("core.dashboard.tile.health"),
                dashboard = culprit_core::t!("ui.common.view.dashboard")
            ),
        ),
    };
    f.title = t!("check.tier_limit.title", resource = name, share = &share, span = &span);
    f.summary = [
        caps,
        t!("check.tier_limit.summary.samples", span = &span, share = &share, peak = format!("{:.0}", l.peak)),
        effect,
        // Throttled resources make requests wait; the worker limit refuses them.
        match l.resource {
            Resource::Workers => t!("check.tier_limit.summary.refused"),
            _ => t!("check.tier_limit.summary.waits"),
        },
        t!("check.tier_limit.summary.not_proof"),
    ]
    .join(" ");
    f.severity = l.severity;
    f.impact.score = base_score(f.severity) + l.at_limit * 30.0;
    f.impact.set_label(t!("check.tier_limit.impact", share = &share));
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.tier_limit.metric.at_limit"), l.at_limit, Unit::Ratio),
        Metric::said(culprit_core::key!("check.tier_limit.metric.peak"), l.peak / 100.0, Unit::Ratio),
    ];
    f.fix = Some(Fix { summary: fix, sql: None, cost: None });
    f.verify = vec![verify];
    f
}
