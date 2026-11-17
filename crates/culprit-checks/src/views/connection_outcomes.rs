//! Source-owned vocabulary interprets one retained native population without inventing failed query owners.
use culprit_core::{
    ConnectionOutcomeAvailability, ConnectionOutcomeEntry, ConnectionOutcomeMetric, ConnectionOutcomeMetricKind,
    ConnectionOutcomeReference, ConnectionOutcomeReport, ConnectionOutcomeSnapshot, MeasurementScope, ResetMarker,
    Result, SourceOutcome, SourceState, format, t,
};

pub const MAX_CONNECTION_OUTCOME_REPORT_ENTRIES: usize = 256;

/// An ordinary scan may display independently retained facts when its own namespace is unknown. It never gains
/// attribution from that display; known contradictions are rejected by the same comparison used for findings.
pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<ConnectionOutcomeReport>> {
    let Some(read) = snapshot.pressure.as_ref().and_then(|pressure| pressure.connection_outcomes.as_deref()) else {
        return Ok(None);
    };
    match read.target.snapshot_namespace_comparison(snapshot) {
        culprit_core::Comparison::Compatible => explain(read, true).map(Some),
        culprit_core::Comparison::Incompatible(culprit_core::ComparisonReason::UnknownIdentity) => explain(read, true)
            .map(|mut report| {
                report.limitations.push(t!("view.observations.limit.namespace"));
                Some(report)
            }),
        _ => Err(culprit_core::Error::invalid(t!("view.connection_outcomes.invalid"))),
    }
}

pub fn explain(snapshot: &ConnectionOutcomeSnapshot, read_complete: bool) -> Result<ConnectionOutcomeReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(culprit_core::Error::invalid(t!("view.connection_outcomes.invalid")));
    }
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut entries = Vec::new();
    let mut mapped_rows = 0;
    let mut unsupported = snapshot.sources.iter().any(|source| source.state == SourceState::NotApplicable);
    let mut unknown_lifetime = false;
    let rates = snapshot.rates();
    for row in &snapshot.observations {
        *counts.entry(&row.source).or_default() += 1;
        let Some(profile) = snapshot.profile(row) else { continue };
        mapped_rows += 1;
        unsupported |=
            profile.metrics.iter().any(|metric| metric.availability == ConnectionOutcomeAvailability::Unsupported);
        if entries.len() == MAX_CONNECTION_OUTCOME_REPORT_ENTRIES {
            continue;
        }
        let metrics = profile
            .metrics
            .iter()
            .map(|spec| {
                let native = row.counters.counters.iter().find(|counter| counter.key == spec.native_key);
                let rate = rates.get(&spec.kind).cloned();
                unknown_lifetime |= native
                    .is_some_and(|counter| counter.nature == culprit_core::telemetry::CounterNature::Cumulative)
                    && (profile.observed_at.is_none()
                        || native.is_some_and(|counter| matches!(counter.reset, ResetMarker::Unknown)));
                ConnectionOutcomeMetric {
                    kind: spec.kind,
                    native_key: spec.native_key.into(),
                    label: label(spec.kind),
                    availability: if spec.availability == ConnectionOutcomeAvailability::Supported && native.is_none() {
                        ConnectionOutcomeAvailability::Unknown
                    } else {
                        spec.availability
                    },
                    value: native.map(|counter| counter.value.clone()),
                    display_value: native.map(|counter| display(&counter.value, counter.unit)),
                    unit: native.map(|counter| counter.unit),
                    nature: native.map(|counter| counter.nature),
                    reset: native.map(|counter| counter.reset.clone()).unwrap_or(ResetMarker::Unknown),
                    rate,
                    explanation: meaning(spec.kind),
                }
            })
            .collect();
        entries.push(ConnectionOutcomeEntry {
            reference: ConnectionOutcomeReference { source: row.source.clone(), observation_key: row.key()? },
            scope: profile.scope,
            title: if profile.scope == MeasurementScope::Database {
                t!("view.connection_outcomes.entry.database")
            } else {
                t!("view.connection_outcomes.entry.shared")
            },
            explanation: if profile.scope == MeasurementScope::Database {
                t!("view.connection_outcomes.population.database")
            } else {
                t!("view.connection_outcomes.population.shared")
            },
            metrics,
            investigation: t!("view.connection_outcomes.investigation"),
            verify: t!("view.connection_outcomes.verify"),
        });
    }
    let mut limitations = vec![
        t!("view.connection_outcomes.limit.owner"),
        t!("view.connection_outcomes.limit.current"),
        t!("view.connection_outcomes.limit.overlap"),
    ];
    if snapshot.context.native().is_none() {
        limitations.push(t!("view.connection_outcomes.limit.collector_clock"));
    }
    if snapshot.context.native().is_none_or(|context| !context.has_qualified_local_owner()) {
        limitations.push(t!("view.connection_outcomes.limit.origin"));
    }
    if unknown_lifetime {
        limitations.push(t!("view.connection_outcomes.limit.lifetime"));
    }
    if unsupported {
        limitations.push(t!("view.connection_outcomes.limit.unsupported"));
    }
    if mapped_rows == 0 {
        limitations.push(t!("view.connection_outcomes.limit.no_facts"));
    }
    if !read_complete {
        limitations.push(t!("view.connection_outcomes.limit.read"));
    }
    if snapshot.sources.iter().any(|source| !source.complete_scope) {
        limitations.push(t!("view.connection_outcomes.limit.source"));
    }
    let entries_limited = mapped_rows > entries.len();
    if entries_limited {
        limitations.push(t!("view.connection_outcomes.limit.presentation"));
    }
    let sources = snapshot
        .sources
        .iter()
        .map(|source| {
            let observed = counts.get(source.source.as_str()).copied().unwrap_or_default();
            let gap = matches!(source.state, SourceState::Collected | SourceState::Partial | SourceState::Empty)
                && (!read_complete || observed != source.records as usize);
            SourceOutcome {
                source: source.source.clone(),
                state: if gap { SourceState::Partial } else { source.state },
                reason: if gap {
                    Some(t!("view.connection_outcomes.limit.read"))
                } else if source.error_code.is_some() {
                    Some(t!("view.connection_outcomes.limit.source"))
                } else {
                    None
                },
                reason_message: None,
            }
        })
        .collect();
    Ok(ConnectionOutcomeReport {
        target: snapshot.target.clone(),
        context: snapshot.context.clone(),
        sources,
        read_complete,
        native_rows: snapshot.sources.iter().map(|source| source.records).sum(),
        observed_rows: snapshot.observations.len() as u32,
        mapped_rows: mapped_rows as u32,
        entries_limited,
        title: t!("view.connection_outcomes.title"),
        summary: t!(
            "view.connection_outcomes.summary",
            count = format::digits(mapped_rows.to_string()),
            shown = format::digits(entries.len().to_string())
        ),
        limitations,
        entries,
    })
}

fn display(value: &culprit_core::telemetry::MeasurementValue, unit: culprit_core::telemetry::CounterUnit) -> String {
    use culprit_core::telemetry::{CounterUnit, MeasurementValue};
    match (value, unit) {
        (MeasurementValue::Integer(value), CounterUnit::Count) => format::digits(value.clone()),
        (MeasurementValue::Integer(value), CounterUnit::Milliseconds) => value
            .parse::<u64>()
            .map(|value| format::duration(value as f64))
            .unwrap_or_else(|_| format::digits(value.clone())),
        (MeasurementValue::Real(value), CounterUnit::Milliseconds) => format::duration(*value),
        (MeasurementValue::Integer(value), _) => format::digits(value.clone()),
        (MeasurementValue::Real(value), _) => format::count(*value),
    }
}

fn label(kind: ConnectionOutcomeMetricKind) -> String {
    use ConnectionOutcomeMetricKind::*;
    match kind {
        SessionsEstablished => t!("view.connection_outcomes.metric.sessions_established"),
        SessionsLostClient => t!("view.connection_outcomes.metric.sessions_lost_client"),
        SessionsFatal => t!("view.connection_outcomes.metric.sessions_fatal"),
        SessionsOperatorTerminated => t!("view.connection_outcomes.metric.sessions_operator_terminated"),
        SessionTime => t!("view.connection_outcomes.metric.session_time"),
        ActiveTime => t!("view.connection_outcomes.metric.active_time"),
        IdleInTransactionTime => t!("view.connection_outcomes.metric.idle_in_transaction_time"),
        ConnectionAttempts => t!("view.connection_outcomes.metric.connection_attempts"),
        FailedConnectionAttempts => t!("view.connection_outcomes.metric.failed_connection_attempts"),
        AbortedClientConnections => t!("view.connection_outcomes.metric.aborted_client_connections"),
        ConnectionErrorsAccept => t!("view.connection_outcomes.metric.errors_accept"),
        ConnectionErrorsInternal => t!("view.connection_outcomes.metric.errors_internal"),
        ConnectionErrorsMaxConnections => t!("view.connection_outcomes.metric.errors_max_connections"),
        ConnectionErrorsPeerAddress => t!("view.connection_outcomes.metric.errors_peer_address"),
        ConnectionErrorsSelect => t!("view.connection_outcomes.metric.errors_select"),
        ConnectionErrorsTcpwrap => t!("view.connection_outcomes.metric.errors_tcpwrap"),
        ThreadsCreated => t!("view.connection_outcomes.metric.threads_created"),
        ThreadsConnected => t!("view.connection_outcomes.metric.threads_connected"),
        ThreadsRunning => t!("view.connection_outcomes.metric.threads_running"),
    }
}

fn meaning(kind: ConnectionOutcomeMetricKind) -> String {
    use ConnectionOutcomeMetricKind::*;
    match kind {
        SessionsEstablished => t!("view.connection_outcomes.meaning.sessions_established"),
        SessionsLostClient => t!("view.connection_outcomes.meaning.sessions_lost_client"),
        SessionsFatal => t!("view.connection_outcomes.meaning.sessions_fatal"),
        SessionsOperatorTerminated => t!("view.connection_outcomes.meaning.sessions_operator_terminated"),
        SessionTime => t!("view.connection_outcomes.meaning.session_time"),
        ActiveTime => t!("view.connection_outcomes.meaning.active_time"),
        IdleInTransactionTime => t!("view.connection_outcomes.meaning.idle_in_transaction_time"),
        ConnectionAttempts => t!("view.connection_outcomes.meaning.connection_attempts"),
        FailedConnectionAttempts => t!("view.connection_outcomes.meaning.failed_connection_attempts"),
        AbortedClientConnections => t!("view.connection_outcomes.meaning.aborted_client_connections"),
        ConnectionErrorsAccept
        | ConnectionErrorsInternal
        | ConnectionErrorsMaxConnections
        | ConnectionErrorsPeerAddress
        | ConnectionErrorsSelect
        | ConnectionErrorsTcpwrap => t!("view.connection_outcomes.meaning.connection_errors"),
        ThreadsCreated => t!("view.connection_outcomes.meaning.threads_created"),
        ThreadsConnected | ThreadsRunning => t!("view.connection_outcomes.meaning.thread_gauges"),
    }
}

#[cfg(test)]
pub(crate) mod tests;
