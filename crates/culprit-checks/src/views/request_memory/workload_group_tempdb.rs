//! Group measurements remain their own shared population; an admitted current request is context, not an allocator
//! or a request identified by a historical group quota-abort counter.
use super::*;

pub(super) fn explain(participant: &Participant<'_>) -> Option<String> {
    participant.qualified_request()?;
    participant.resource()?;
    if !participant.has_shared_admission_scope() {
        return None;
    }
    let group = participant.observation().workload_group_tempdb.as_ref()?;
    let mut words = vec![t!("view.request_memory.tempdb.context", group = format::digits(group.group_id.clone()))];
    words.push(group.current_kibibytes.as_ref().map_or_else(
        || t!("view.request_memory.tempdb.current_unknown"),
        |value| t!("view.request_memory.tempdb.current", value = format::digits(value.clone())),
    ));
    if let Some(value) = &group.peak_kibibytes {
        words.push(t!("view.request_memory.tempdb.peak", value = format::digits(value.clone())));
    }
    if let Some(count) = &group.limit_violations {
        words.push(t!("view.request_memory.tempdb.violations", count = format::digits(count.clone())));
    }
    words.push(group.statistics_started_without_offset.as_ref().map_or_else(
        || t!("view.request_memory.tempdb.statistics_unknown"),
        |at| t!("view.request_memory.tempdb.statistics", at = at),
    ));
    match &group.stored_limits {
        None => words.push(t!("view.request_memory.tempdb.limits_unavailable")),
        Some(limits) => {
            if let Some(value) = &limits.fixed_mebibytes {
                words.push(t!("view.request_memory.tempdb.fixed", value = format::digits(value.clone())));
            }
            if let Some(value) = &limits.percent {
                words.push(t!("view.request_memory.tempdb.percent", value = format::digits(value.clone())));
            }
            if limits.fixed_mebibytes.is_none() && limits.percent.is_none() {
                words.push(t!("view.request_memory.tempdb.no_stored_limit"));
            }
        }
    }
    Some(words.join(" "))
}

#[cfg(test)]
mod tests;
