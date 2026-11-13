//! Source-owned error identifiers explain retained diagnostics without assigning a current query or resource owner.
use culprit_core::{
    DeadlockPattern, Error, Result, RetainedFailureExplanation, RetainedFailureReport, SourceOutcome, SourceState,
    format, t,
    telemetry::{NativeFailureRecord, NativeFailureSnapshot, PostgresFailureKind},
};

pub fn explain(evidence: &NativeFailureSnapshot, source: Option<&SourceOutcome>) -> Result<RetainedFailureReport> {
    validate_input(evidence, source)?;
    let mut limitations = vec![
        t!("view.native_failures.limit.coverage"),
        t!("view.native_failures.limit.attribution"),
        t!("view.native_failures.limit.messages"),
    ];
    if evidence.resource.is_none() {
        limitations.push(t!("view.memory_failures.limit.unowned"));
    }
    if evidence.observations.iter().any(|row| matches!(row, NativeFailureRecord::PostgresLog(_))) {
        limitations.push(t!("view.native_failures.postgres.limit.identity"));
    }
    if evidence
        .observations
        .iter()
        .any(|row| matches!(row, NativeFailureRecord::PostgresLog(row) if row.timestamp_utc.is_none()))
    {
        limitations.push(t!("view.native_failures.postgres.limit.clock"));
    }
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.memory_failures.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.memory_failures.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.memory_failures.limit.source_unavailable"));
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    Ok(RetainedFailureReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.native_failures.title"),
        summary: if evidence.observations.is_empty() {
            t!("view.native_failures.summary.empty")
        } else {
            t!(
                "view.native_failures.summary.retained",
                n = evidence.observations.len(),
                count = format::count(evidence.observations.len() as f64)
            )
        },
        limitations,
        entries: evidence.observations.iter().map(NativeFailureCase::of).map(render).collect(),
    })
}

fn validate_input(evidence: &NativeFailureSnapshot, source: Option<&SourceOutcome>) -> Result<()> {
    let inconsistent_source = source.is_some_and(|source| {
        source.source != evidence.source
            || (!evidence.observations.is_empty()
                && matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable))
    });
    if !evidence.is_valid() || inconsistent_source {
        return Err(Error::invalid(t!("view.native_failures.invalid_capture")));
    }
    Ok(())
}

struct NativeFailureCase {
    position: u32,
    meaning: NativeFailureMeaning,
}

impl NativeFailureCase {
    fn of(observation: &NativeFailureRecord) -> Self {
        match observation {
            NativeFailureRecord::MysqlErrorLog(row) => {
                Self { position: row.position, meaning: NativeFailureMeaning::mysql(row.error_code.as_deref()) }
            }
            NativeFailureRecord::PostgresLog(row) => Self {
                position: row.position,
                meaning: NativeFailureMeaning::Postgres {
                    kind: row.kind(),
                    deadlock_pattern: row.deadlock.as_ref().map(|graph| graph.pattern),
                },
            },
        }
    }
}

/// Native log identifiers have their own namespace. Client numeric errors and free-form text cannot substitute.
/// https://dev.mysql.com/doc/mysql-errors/8.4/en/server-error-reference.html
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeFailureMeaning {
    Allocation,
    Resources,
    SortMemory,
    Postgres { kind: PostgresFailureKind, deadlock_pattern: Option<DeadlockPattern> },
    Unclassified,
}

impl NativeFailureMeaning {
    fn mysql(code: Option<&str>) -> Self {
        match code {
            Some("MY-010935") => Self::Allocation,
            Some("MY-010934") => Self::Resources,
            Some("MY-013131") => Self::SortMemory,
            _ => Self::Unclassified,
        }
    }
}

fn render(case: NativeFailureCase) -> RetainedFailureExplanation {
    let (title, explanation, investigation) = match case.meaning {
        NativeFailureMeaning::Allocation => (
            t!("view.native_failures.cause.allocation.title"),
            t!("view.native_failures.cause.allocation.explanation"),
            t!("view.native_failures.cause.allocation.investigation"),
        ),
        NativeFailureMeaning::Resources => (
            t!("view.native_failures.cause.resources.title"),
            t!("view.native_failures.cause.resources.explanation"),
            t!("view.native_failures.cause.resources.investigation"),
        ),
        NativeFailureMeaning::SortMemory => (
            t!("view.native_failures.cause.sort.title"),
            t!("view.native_failures.cause.sort.explanation"),
            t!("view.native_failures.cause.sort.investigation"),
        ),
        NativeFailureMeaning::Postgres { kind, deadlock_pattern } => postgres_explanation(kind, deadlock_pattern),
        NativeFailureMeaning::Unclassified => (
            t!("view.native_failures.cause.unknown.title"),
            t!("view.native_failures.cause.unknown.explanation"),
            t!("view.native_failures.cause.unknown.investigation"),
        ),
    };
    RetainedFailureExplanation {
        position: case.position,
        title,
        explanation,
        investigation,
        verify: t!("view.native_failures.verify"),
    }
}

fn postgres_explanation(kind: PostgresFailureKind, pattern: Option<DeadlockPattern>) -> (String, String, String) {
    match kind {
        PostgresFailureKind::AllocationFailure => (
            t!("view.native_failures.cause.allocation.title"),
            t!("view.native_failures.cause.allocation.explanation"),
            t!("view.native_failures.cause.allocation.investigation"),
        ),
        PostgresFailureKind::Deadlock => (
            t!("view.native_failures.postgres.deadlock.title"),
            match pattern {
                Some(pattern) => t!("view.native_failures.postgres.deadlock.graph", pattern = pattern.label()),
                None => t!("view.native_failures.postgres.deadlock.explanation"),
            },
            t!("view.native_failures.postgres.deadlock.investigation"),
        ),
        PostgresFailureKind::QueryCanceled => (
            t!("view.native_failures.postgres.canceled.title"),
            t!("view.native_failures.postgres.canceled.explanation"),
            t!("view.native_failures.postgres.canceled.investigation"),
        ),
        PostgresFailureKind::InsufficientResources => (
            t!("view.native_failures.postgres.resources.title"),
            t!("view.native_failures.postgres.resources.explanation"),
            t!("view.native_failures.postgres.resources.investigation"),
        ),
        PostgresFailureKind::DiskFull => (
            t!("view.native_failures.postgres.disk.title"),
            t!("view.native_failures.postgres.disk.explanation"),
            t!("view.native_failures.postgres.disk.investigation"),
        ),
        PostgresFailureKind::TooManyConnections => (
            t!("view.native_failures.postgres.connections.title"),
            t!("view.native_failures.postgres.connections.explanation"),
            t!("view.native_failures.postgres.connections.investigation"),
        ),
        PostgresFailureKind::ConfigurationLimit => (
            t!("view.native_failures.postgres.configuration.title"),
            t!("view.native_failures.postgres.configuration.explanation"),
            t!("view.native_failures.postgres.configuration.investigation"),
        ),
        PostgresFailureKind::ProgramLimit => (
            t!("view.native_failures.postgres.program.title"),
            t!("view.native_failures.postgres.program.explanation"),
            t!("view.native_failures.postgres.program.investigation"),
        ),
        PostgresFailureKind::LockNotAvailable => (
            t!("view.native_failures.postgres.lock.title"),
            t!("view.native_failures.postgres.lock.explanation"),
            t!("view.native_failures.postgres.lock.investigation"),
        ),
        PostgresFailureKind::AdminShutdown => (
            t!("view.native_failures.postgres.admin.title"),
            t!("view.native_failures.postgres.admin.explanation"),
            t!("view.native_failures.postgres.admin.investigation"),
        ),
        PostgresFailureKind::CrashShutdown => (
            t!("view.native_failures.postgres.crash.title"),
            t!("view.native_failures.postgres.crash.explanation"),
            t!("view.native_failures.postgres.crash.investigation"),
        ),
        PostgresFailureKind::CannotConnectNow => (
            t!("view.native_failures.postgres.unavailable.title"),
            t!("view.native_failures.postgres.unavailable.explanation"),
            t!("view.native_failures.postgres.unavailable.investigation"),
        ),
        PostgresFailureKind::DatabaseDropped => (
            t!("view.native_failures.postgres.dropped.title"),
            t!("view.native_failures.postgres.dropped.explanation"),
            t!("view.native_failures.postgres.dropped.investigation"),
        ),
        PostgresFailureKind::IdleSessionTimeout => (
            t!("view.native_failures.postgres.idle_session.title"),
            t!("view.native_failures.postgres.idle_session.explanation"),
            t!("view.native_failures.postgres.idle_session.investigation"),
        ),
        PostgresFailureKind::IdleTransactionTimeout => (
            t!("view.native_failures.postgres.idle_transaction.title"),
            t!("view.native_failures.postgres.idle_transaction.explanation"),
            t!("view.native_failures.postgres.idle_transaction.investigation"),
        ),
        PostgresFailureKind::TransactionTimeout => (
            t!("view.native_failures.postgres.transaction_timeout.title"),
            t!("view.native_failures.postgres.transaction_timeout.explanation"),
            t!("view.native_failures.postgres.transaction_timeout.investigation"),
        ),
        PostgresFailureKind::Unclassified => (
            t!("view.native_failures.cause.unknown.title"),
            t!("view.native_failures.cause.unknown.explanation"),
            t!("view.native_failures.cause.unknown.investigation"),
        ),
    }
}

#[cfg(test)]
mod tests;
