use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::SpaceUsage;
use culprit_core::{Actionability, FileScope, t};

const CAPACITY_WARNING_PERCENT: u32 = 90;
const CAPACITY_ATTENTION_PERCENT: u32 = 95;

pub fn storage_capacity_info() -> CheckInfo {
    CheckInfo {
        id: "storage-capacity".into(),
        title: t!("check.storage_capacity.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.storage_capacity.info.description"),
    }
}

check!(
    StorageCapacityCheck: storage_capacity_info,
    decide = full_spaces,
    render = storage_capacity_finding,
    case<'s> = FullSpace<'s>,
    evidence = &[Evidence::Observed(
        Observation::Spaces,
        &[
            culprit_core::source_id::LOG_CAPACITY,
            culprit_core::source_id::FILE_CAPACITY,
            culprit_core::source_id::TEMP_CAPACITY,
        ],
    )],
);

/// What bounds a space's capacity, as its title names it.
#[derive(Clone, Copy)]
pub enum Allocation {
    /// The files can't grow past it (growth off, or a max size at the current size).
    Available,
    /// A max size above the current allocation.
    Configured,
    /// Room to grow with no max size: what is allocated now.
    Allocated,
}

/// A file or log allocation 90% or more occupied, measured when the scan ran.
pub struct FullSpace<'s> {
    space: &'s SpaceUsage,
    used: f64,
    capacity: f64,
    free: f64,
    share: f64,
    allocation: Allocation,
    /// Shared by the whole instance (tempdb): not this database's to grade.
    shared: bool,
    /// Why the log can't be reused, when SQL Server reports a reason.
    reuse: Option<&'s str>,
    actionable: bool,
}

struct Occupancy {
    used: f64,
    capacity: f64,
    free: f64,
    share: f64,
    constrained: bool,
    attention: bool,
    allocation: Allocation,
}

fn allocation(constrained: bool, can_allocate_more: bool) -> Allocation {
    if constrained {
        Allocation::Available
    } else if can_allocate_more {
        Allocation::Configured
    } else {
        Allocation::Allocated
    }
}

fn exact_occupancy(space: &culprit_core::telemetry::OperationalSpace) -> Option<Occupancy> {
    let total = space.total_bytes.as_deref()?.parse::<u64>().ok()?;
    let used = space.used_bytes.as_deref()?.parse::<u64>().ok()?;
    if total == 0 || used > total {
        return None;
    }
    let limit = space.limit_bytes.as_deref().map(str::parse::<u64>).transpose().ok()?;
    let hard = space.growth_enabled == Some(false);
    let capacity = if hard { total } else { limit.filter(|limit| *limit >= total).unwrap_or(total) };
    let at_least = |percent: u32| u128::from(used) * 100 >= u128::from(capacity) * u128::from(percent);
    if !at_least(CAPACITY_WARNING_PERCENT) {
        return None;
    }
    let constrained = hard || limit.is_some_and(|limit| limit <= total);
    Some(Occupancy {
        used: used as f64,
        capacity: capacity as f64,
        free: (capacity - used) as f64,
        share: used as f64 / capacity as f64,
        constrained,
        attention: at_least(CAPACITY_ATTENTION_PERCENT),
        allocation: allocation(constrained, capacity > total),
    })
}

fn chart_occupancy(space: &SpaceUsage) -> Option<Occupancy> {
    if !space.total_bytes.is_finite()
        || !space.used_bytes.is_finite()
        || space.total_bytes <= 0.0
        || space.used_bytes < 0.0
        || space.used_bytes > space.total_bytes
    {
        return None;
    }
    let hard = space.growth_enabled == Some(false);
    let capacity = if hard {
        space.total_bytes
    } else {
        space.limit_bytes.filter(|limit| *limit >= space.total_bytes).unwrap_or(space.total_bytes)
    };
    let share = space.used_bytes / capacity;
    if share < f64::from(CAPACITY_WARNING_PERCENT) / 100.0 {
        return None;
    }
    let constrained = hard || space.limit_bytes.is_some_and(|limit| limit <= space.total_bytes);
    Some(Occupancy {
        used: space.used_bytes,
        capacity,
        free: (capacity - space.used_bytes).max(0.0),
        share,
        constrained,
        attention: share >= f64::from(CAPACITY_ATTENTION_PERCENT) / 100.0,
        allocation: allocation(constrained, capacity > space.total_bytes),
    })
}

fn full_spaces(s: &Snapshot) -> Vec<FullSpace<'_>> {
    let Ok(evidence) = crate::storage::AllocationEvidence::from_snapshot(s) else { return Vec::new() };
    s.operations
        .spaces
        .iter()
        .filter_map(|space| {
            let (occupied, reuse_wait) = if let Some(native) = evidence.space(space) {
                (exact_occupancy(native)?, native.reuse_wait.as_deref())
            } else {
                (chart_occupancy(space)?, space.reuse_wait.as_deref())
            };
            let shared = space.scope == FileScope::Server;
            let reuse = reuse_wait.filter(|wait| *wait != "NOTHING");
            // A nearly occupied log with autogrowth room is ordinary operation, not a full-disk diagnosis.
            let actionable = !shared
                && occupied.constrained
                && occupied.attention
                && (space.kind != FileKind::Log || reuse.is_some());
            Some(FullSpace {
                space,
                used: occupied.used,
                capacity: occupied.capacity,
                free: occupied.free,
                share: occupied.share,
                allocation: occupied.allocation,
                shared,
                reuse,
                actionable,
            })
        })
        .collect()
}

fn storage_capacity_finding(_: &Snapshot, full: FullSpace<'_>) -> Finding {
    let space = full.space;
    let mut f = finding(
        "storage-capacity",
        &culprit_core::redact::shareable_name(&space.name),
        Category::Configuration,
        subject(SubjectKind::Database, &space.name),
    );
    let (name, used, capacity) = (&space.name, format::bytes(full.used), format::bytes(full.capacity));
    f.title = match full.allocation {
        Allocation::Available => {
            t!("check.storage_capacity.title.available", name = name, used = used, capacity = capacity)
        }
        Allocation::Configured => {
            t!("check.storage_capacity.title.configured", name = name, used = used, capacity = capacity)
        }
        Allocation::Allocated => {
            t!("check.storage_capacity.title.allocated", name = name, used = used, capacity = capacity)
        }
    };
    let free = full.free;
    let mut summary = vec![t!("check.storage_capacity.summary.measured", free = format::bytes(free))];
    if full.shared {
        summary.push(t!("check.storage_capacity.summary.shared"));
    }
    summary.push(match full.allocation {
        Allocation::Available => t!("check.storage_capacity.summary.constrained"),
        Allocation::Configured | Allocation::Allocated => t!("check.storage_capacity.summary.can_grow"),
    });
    if let Some(reason) = full.reuse {
        summary.push(t!("check.storage_capacity.summary.reuse", reason = reason));
    }
    // Two spaces before the last sentence, as this summary has always read.
    f.summary = format!("{} {}", summary.join(" "), t!("check.storage_capacity.summary.not_measured"));
    f.severity = if full.actionable { Severity::Medium } else { Severity::Low };
    f.actionability = if full.actionable { Actionability::NeedsAttention } else { Actionability::Information };
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(t!("check.storage_capacity.impact", share = format::percent(full.share)));
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.storage_capacity.metric.used"), full.used, Unit::Bytes),
        Metric::said(culprit_core::key!("check.storage_capacity.metric.allocation"), full.capacity, Unit::Bytes),
        Metric::said(culprit_core::key!("check.storage_capacity.metric.free"), free, Unit::Bytes),
    ];
    f.metrics.extend(
        space
            .parts
            .iter()
            .filter(|p| p.bytes.is_finite() && p.bytes >= 0.0)
            .map(|p| Metric::new(&p.name, p.bytes, Unit::Bytes)),
    );
    f.fix = Some(Fix {
        summary: if space.kind == FileKind::Log {
            t!("check.storage_capacity.fix.log")
        } else if full.shared {
            t!("check.storage_capacity.fix.shared")
        } else {
            t!("check.storage_capacity.fix.data")
        },
        sql: None,
        cost: None,
    });
    f.verify = vec![t!("check.storage_capacity.verify")];
    f
}

#[cfg(test)]
mod tests;
