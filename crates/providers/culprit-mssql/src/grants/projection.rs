use culprit_core::{GrantStat, Result, telemetry::*};
use std::collections::BTreeMap;

pub(super) struct Summary {
    pub grants: Vec<GrantStat>,
    pub complete: bool,
}
pub(super) fn summarize(rows: &[StatementGrantStatistics]) -> Result<Summary> {
    let mut groups: BTreeMap<String, Vec<&StatementGrantStatistics>> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| !row.context.origin.is_culprit() && row.context.origin != StatementOrigin::EngineInternal)
    {
        if let Some(hash) = &row.context.query_hash {
            groups.entry(hash.to_uppercase()).or_default().push(row);
        }
    }
    let mut summary = Summary { grants: Vec::new(), complete: true };
    for (hash, rows) in groups {
        let mut executions = 0u128;
        let mut granted = 0u128;
        let mut used = 0u128;
        let mut maximum = 0u64;
        let mut known = true;
        for row in &rows {
            if !row.context.identity.has_plan_handle() {
                known = false;
                continue;
            }
            executions = executions.checked_add(executions_of(row)? as u128).ok_or_else(crate::read::invalid_data)?;
            match (&row.total_grant_kibibytes, &row.total_used_kibibytes, &row.max_grant_kibibytes) {
                (Some(g), Some(u), Some(m)) => {
                    granted = granted.checked_add(counter(g)? as u128).ok_or_else(crate::read::invalid_data)?;
                    used = used.checked_add(counter(u)? as u128).ok_or_else(crate::read::invalid_data)?;
                    maximum = maximum.max(counter(m)?);
                }
                _ => known = false,
            }
        }
        if !known {
            summary.complete = false;
            continue;
        }
        if executions == 0 {
            continue;
        }
        let (last_granted, last_used) = last_grants(&rows)?;
        summary.grants.push(GrantStat {
            query_hash: format!("0x{}", &hash[2..]),
            executions: executions as f64,
            avg_granted_kb: granted as f64 / executions as f64,
            avg_used_kb: used as f64 / executions as f64,
            max_granted_kb: maximum as f64,
            last_granted_kb: last_granted.map(|v| v as f64),
            last_used_kb: last_used.map(|v| v as f64),
        });
    }
    summary.grants.sort_by(|a, b| (b.avg_granted_kb - b.avg_used_kb).total_cmp(&(a.avg_granted_kb - a.avg_used_kb)));
    Ok(summary)
}
fn executions_of(row: &StatementGrantStatistics) -> Result<u64> {
    counter(row.executions.as_deref().ok_or_else(crate::read::invalid_data)?)
}

fn counter(value: &str) -> Result<u64> {
    value.parse().map_err(|_| crate::read::invalid_data())
}
fn last_grants(rows: &[&StatementGrantStatistics]) -> Result<(Option<u64>, Option<u64>)> {
    // The native timestamp is a start time, so it cannot order concurrent completions across entries.
    // A shared value is established only by one executed entry or agreement between every executed entry.
    let mut agreed = None;
    for row in rows {
        if executions_of(row)? == 0 {
            continue;
        }
        let values = (
            row.last_grant_kibibytes.as_deref().map(counter).transpose()?,
            row.last_used_kibibytes.as_deref().map(counter).transpose()?,
        );
        if agreed.is_some_and(|before| before != values) {
            return Ok((None, None));
        }
        agreed = Some(values);
    }
    Ok(agreed.unwrap_or((None, None)))
}
