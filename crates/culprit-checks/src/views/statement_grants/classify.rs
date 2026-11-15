use culprit_core::{
    Result,
    telemetry::{StatementGrantSnapshot, StatementGrantStatistics},
};

const MIN_RECORDED_EXECUTIONS: u64 = 5;
const MIN_AVERAGE_GRANT_KIBIBYTES: u128 = 50 * 1024;
const LOW_USE_DENOMINATOR: u128 = 4;

pub(crate) struct GrantCase<'a> {
    pub snapshot: &'a StatementGrantSnapshot,
    pub row: &'a StatementGrantStatistics,
    pub snapshot_key: String,
    pub observation_key: String,
    pub entry_key: String,
    pub usage: GrantUse,
}

#[derive(Clone, Copy)]
pub(crate) struct GrantAverages {
    pub granted_kibibytes: f64,
    pub used_kibibytes: f64,
}

#[derive(Clone, Copy)]
pub(crate) enum GrantUse {
    RecordedImbalance(GrantAverages),
    RecordedUse(GrantAverages),
    CountersUnavailable,
}

/// Keep every read separate. Even matching cache identities do not make overlapping reads additive.
pub(crate) fn read_cases(snapshot: &StatementGrantSnapshot, read_complete: bool) -> Result<Vec<GrantCase<'_>>> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(super::invalid());
    }
    let snapshot_key = snapshot.key()?;
    snapshot
        .observations
        .iter()
        .map(|row| {
            Ok(GrantCase {
                snapshot,
                row,
                snapshot_key: snapshot_key.clone(),
                observation_key: row.context.identity.key()?,
                entry_key: snapshot.entry_key(row)?,
                usage: usage(row),
            })
        })
        .collect()
}

fn usage(row: &StatementGrantStatistics) -> GrantUse {
    let Some((executions, granted, used)) = measured_totals(row) else { return GrantUse::CountersUnavailable };
    let averages = GrantAverages {
        granted_kibibytes: granted as f64 / executions as f64,
        used_kibibytes: used as f64 / executions as f64,
    };
    // Decide on exact native integers. Floating-point means are presentation only, never threshold authority.
    if executions >= MIN_RECORDED_EXECUTIONS
        && u128::from(granted) >= MIN_AVERAGE_GRANT_KIBIBYTES * u128::from(executions)
        && u128::from(used) * LOW_USE_DENOMINATOR <= u128::from(granted)
    {
        GrantUse::RecordedImbalance(averages)
    } else {
        GrantUse::RecordedUse(averages)
    }
}

fn measured_totals(row: &StatementGrantStatistics) -> Option<(u64, u64, u64)> {
    let executions = row.executions.as_deref()?.parse().ok().filter(|count| *count > 0)?;
    let granted = row.total_grant_kibibytes.as_deref()?.parse().ok()?;
    let used = row.total_used_kibibytes.as_deref()?.parse().ok()?;
    Some((executions, granted, used))
}
