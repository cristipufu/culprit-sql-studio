//! Shared tempdb log points stay independent of data-file allocations, application SQL and transaction owners.
use super::*;

pub(super) fn free_bytes(space: &OperationalSpace) -> Option<String> {
    let total = space.total_bytes.as_deref()?.parse::<u64>().ok()?;
    let used = space.used_bytes.as_deref()?.parse::<u64>().ok()?;
    total.checked_sub(used).map(|free| free.to_string())
}

pub(super) fn explanation(space: &OperationalSpace) -> String {
    let investigation = match space.reuse_wait.as_deref() {
        Some("ACTIVE_TRANSACTION") => t!("view.storage.temp_log.active"),
        Some("CHECKPOINT") => t!("view.storage.temp_log.checkpoint"),
        Some("NOTHING") => t!("view.storage.temp_log.nothing"),
        _ => t!("view.storage.temp_log.other"),
    };
    [t!("view.storage.population.temp_log"), t!("view.storage.temp_log.limit"), investigation].join(" ")
}

#[cfg(test)]
mod tests;
