//! One interpreter for an authenticated observer's group view; no database/query cause or reset is inferred.
use culprit_core::{
    GroupReplicationOrigin, GroupReplicationReport, MeasurementScope, ReplicationEntry, ReplicationField,
    ReplicationFieldGroup, ReplicationReference, Result, SourceOutcome, SourceState, format, t, telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_GROUP_REPLICATION_ENTRIES: usize = 32;
pub struct GroupReplicationEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<GroupReplicationReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(GroupReplicationEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: GroupReplicationEvidence<'_>) -> Result<GroupReplicationReport> {
    let context = group_replication_context(evidence.source)?;
    let SourceObservation::GroupReplication { snapshot, metadata } = &evidence.source.observation else {
        return Err(invalid());
    };
    let mut counts = BTreeMap::<&str, u32>::new();
    let mut keys = BTreeSet::new();
    let mut members = BTreeMap::new();
    let mut statistics = BTreeMap::new();
    for &row in evidence.observations {
        let OperationalFact::MysqlGroupReplication(point) = &row.fact else { return Err(invalid()) };
        let coverage = snapshot.sources.iter().find(|source| source.source == row.source).ok_or_else(invalid)?;
        if !row.is_valid()
            || !point.fits_source(evidence.source)
            || !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
            || !keys.insert((&row.source, &row.key))
        {
            return Err(invalid());
        }
        *counts.entry(&row.source).or_default() += 1;
        let key = (point.channel(), point.member_id());
        match point.as_ref() {
            MysqlGroupReplicationPoint::Member(member) => {
                members.insert(key, (row, member.as_ref()));
            }
            MysqlGroupReplicationPoint::Statistics(stats) => {
                statistics.insert(key, (row, stats.as_ref()));
            }
        }
    }
    let mut sources = Vec::new();
    for source in &snapshot.sources {
        let count = counts.get(source.source.as_str()).copied().unwrap_or(0);
        if count > source.records || evidence.read_complete && count != source.records {
            return Err(invalid());
        }
        let missing = !evidence.read_complete || count != source.records;
        sources.push(SourceOutcome {
            source: source.source.clone(),
            state: if missing { SourceState::Partial } else { source.state },
            reason: if missing || source.state == SourceState::Partial {
                Some(t!("view.group_replication.partial"))
            } else if source.error_code.is_some() || source.stopped.is_some() {
                Some(t!("view.group_replication.unavailable"))
            } else {
                None
            },
            reason_message: None,
        });
    }
    let selected: BTreeSet<_> = members.keys().chain(statistics.keys()).copied().collect();
    let entries_limited = selected.len() > MAX_GROUP_REPLICATION_ENTRIES;
    let entries = selected
        .into_iter()
        .take(MAX_GROUP_REPLICATION_ENTRIES)
        .map(|key| {
            let member = members.get(&key).copied();
            let stats = statistics.get(&key).copied();
            entry(member, stats)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut limitations = limitations();
    if !evidence.read_complete {
        limitations.push(t!("view.group_replication.partial"));
    }
    if entries_limited {
        limitations.push(t!("view.replication.limit.presentation"));
    }
    Ok(GroupReplicationReport {
        original: Some(GroupReplicationOrigin { context, metadata: metadata.as_ref().clone() }),
        sources,
        read_complete: evidence.read_complete,
        native_rows: Some(snapshot.records),
        observed_rows: evidence.observations.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited,
        title: t!("view.group_replication.title"),
        summary: t!(
            "view.group_replication.summary",
            n = entries.len(),
            count = format::digits(entries.len().to_string())
        ),
        limitations,
        entries,
    })
}
fn entry(
    member: Option<(&OperationalObservation, &MysqlGroupMember)>,
    stats: Option<(&OperationalObservation, &MysqlGroupStatistics)>,
) -> Result<ReplicationEntry> {
    let (row, member_id, channel) = member
        .map(|(row, member)| (row, member.member_id.as_str(), member.channel.as_str()))
        .or_else(|| stats.map(|(row, stats)| (row, stats.member_id.as_str(), stats.channel.as_str())))
        .ok_or_else(invalid)?;
    let member = member.map(|(_, member)| member);
    let stats = stats.map(|(_, stats)| stats);
    let explanation = match member.map(|member| member.state.as_str()) {
        Some("ERROR") => t!("view.group_replication.member_error"),
        Some("UNREACHABLE") => t!("view.group_replication.member_unreachable"),
        Some("RECOVERING") => t!("view.group_replication.member_recovering"),
        _ if stats.is_some_and(|stats| {
            [&stats.certification_queue, &stats.applier_queue]
                .into_iter()
                .flatten()
                .any(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
        }) =>
        {
            t!("view.group_replication.queued")
        }
        _ => t!("view.group_replication.member_context"),
    };
    Ok(ReplicationEntry {
        reference: ReplicationReference { source: row.source.clone(), observation_key: row.key.clone() },
        scope: MeasurementScope::Server,
        conditions: Vec::new(),
        title: if member.is_some() {
            t!("view.group_replication.member_title")
        } else {
            t!("view.group_replication.statistics_title")
        },
        explanation,
        fields: vec![
            field("member_id", culprit_core::key!("view.group_replication.field.member"), Some(member_id)),
            field("channel", culprit_core::key!("view.group_replication.field.channel"), Some(channel)),
            field(
                "state",
                culprit_core::key!("view.group_replication.field.state"),
                member.map(|member| member.state.as_str()),
            ),
            field(
                "role",
                culprit_core::key!("view.group_replication.field.role"),
                member.and_then(|member| member.role.as_deref()),
            ),
            field(
                "version",
                culprit_core::key!("view.group_replication.field.version"),
                member.and_then(|member| member.version.as_deref()),
            ),
            field(
                "member_observed_at",
                culprit_core::key!("view.group_replication.field.member_time"),
                member.map(|member| member.observed_at.as_str()),
            ),
            field(
                "statistics_observed_at",
                culprit_core::key!("view.group_replication.field.statistics_time"),
                stats.map(|stats| stats.observed_at.as_str()),
            ),
            field(
                "view_id",
                culprit_core::key!("view.group_replication.field.view"),
                stats.and_then(|stats| stats.view_id.as_deref()),
            ),
        ],
        groups: vec![
            ReplicationFieldGroup {
                title: t!("view.group_replication.gauges"),
                fields: vec![
                    quantity(
                        "certification_queue",
                        culprit_core::key!("view.group_replication.field.certification_queue"),
                        stats.and_then(|s| s.certification_queue.as_deref()),
                    ),
                    quantity(
                        "applier_queue",
                        culprit_core::key!("view.group_replication.field.applier_queue"),
                        stats.and_then(|s| s.applier_queue.as_deref()),
                    ),
                    quantity(
                        "rows_validating",
                        culprit_core::key!("view.group_replication.field.rows_validating"),
                        stats.and_then(|s| s.rows_validating.as_deref()),
                    ),
                ],
            },
            ReplicationFieldGroup {
                title: t!("view.group_replication.totals"),
                fields: vec![
                    quantity(
                        "checked",
                        culprit_core::key!("view.group_replication.field.checked"),
                        stats.and_then(|s| s.checked.as_deref()),
                    ),
                    quantity(
                        "conflicts",
                        culprit_core::key!("view.group_replication.field.conflicts"),
                        stats.and_then(|s| s.conflicts.as_deref()),
                    ),
                    quantity(
                        "remote_applied",
                        culprit_core::key!("view.group_replication.field.remote_applied"),
                        stats.and_then(|s| s.remote_applied.as_deref()),
                    ),
                    quantity(
                        "local_proposed",
                        culprit_core::key!("view.group_replication.field.local_proposed"),
                        stats.and_then(|s| s.local_proposed.as_deref()),
                    ),
                    quantity(
                        "local_rollback",
                        culprit_core::key!("view.group_replication.field.local_rollback"),
                        stats.and_then(|s| s.local_rollback.as_deref()),
                    ),
                ],
            },
        ],
        session: None,
        unresolved_session_id: None,
        investigation: t!("view.group_replication.investigate"),
        verify: t!("view.group_replication.verify"),
    })
}
fn field(key: &str, label: &'static str, value: Option<&str>) -> ReplicationField {
    ReplicationField { key: key.into(), label: culprit_core::i18n::t(label, &[]), value: value.map(String::from) }
}
fn quantity(key: &str, label: &'static str, value: Option<&str>) -> ReplicationField {
    let mut field = field(key, label, value);
    field.value = field.value.map(format::digits);
    field
}
fn limitations() -> Vec<String> {
    vec![
        t!("view.group_replication.limit.scope"),
        t!("view.group_replication.limit.propagation"),
        t!("view.group_replication.limit.reset"),
        t!("view.group_replication.limit.history"),
        t!("view.group_replication.limit.coverage"),
    ]
}
pub fn explain_recorded(recorded: &RecordedOperationalRead) -> Result<GroupReplicationReport> {
    match recorded {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> GroupReplicationReport {
    GroupReplicationReport {
        original: None,
        sources: [MYSQL_GROUP_MEMBERS_SOURCE, MYSQL_GROUP_STATISTICS_SOURCE]
            .into_iter()
            .map(|source| SourceOutcome {
                source: source.into(),
                state: SourceState::Unavailable,
                reason: Some(reason.render()),
                reason_message: Some(reason.clone()),
            })
            .collect(),
        read_complete: false,
        native_rows: None,
        observed_rows: 0,
        presented_rows: 0,
        entries_limited: false,
        title: t!("view.group_replication.title"),
        summary: reason.render(),
        limitations: limitations(),
        entries: Vec::new(),
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
