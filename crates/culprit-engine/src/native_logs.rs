//! One bounded local intake over the provider's acquisition-independent parser. No workspace, credentials or source
//! database are touched, and a native database name never establishes resource ownership or feed publication.
use chrono::{SecondsFormat, Utc};
use culprit_core::{
    Error, ErrorCode, NativeLogAnalysis, NativeLogArtifact, NativeLogFormat, NativeLogIssue, NativeLogLimits,
    NativeLogProblem, Result, SourceOutcome, SourceState, message, t,
    telemetry::{NativeFailureRecord, NativeFailureSnapshot, digest},
};
use culprit_postgres::structured_logs::{self, LogFormat, ParsedLogBatch};
use std::{fs::File, io::Read, path::Path};

const SOURCE: &str = "supplied-postgres-log";

pub fn analyze_bytes(input: &[u8], format: NativeLogFormat, limits: NativeLogLimits) -> Result<NativeLogAnalysis> {
    validate_limits(limits)?;
    let started = utc_now();
    let prefix = &input[..input.len().min(limits.input_bytes)];
    analyze(prefix, format, limits, started, input.len() <= limits.input_bytes, None)
}

/// Inspect an explicitly selected regular local file. The size-plus-one read is bounded even if the file grows.
pub fn analyze_file(path: &Path, format: NativeLogFormat, limits: NativeLogLimits) -> Result<NativeLogAnalysis> {
    validate_limits(limits)?;
    let started = utc_now();
    let initial = std::fs::metadata(path).map_err(read_failed)?;
    if !initial.is_file() {
        return Err(not_regular_file());
    }
    let file = File::open(path).map_err(read_failed)?;
    let opened = file.metadata().map_err(read_failed)?;
    if !opened.is_file() {
        return Err(not_regular_file());
    }
    let mut bytes = Vec::new();
    let mut bounded = file.take(limits.input_bytes as u64 + 1);
    bounded.read_to_end(&mut bytes).map_err(read_failed)?;
    let after = bounded.get_ref().metadata().map_err(read_failed)?;
    let changed = opened.len() != after.len() || opened.modified().ok() != after.modified().ok();
    let complete = !changed && bytes.len() <= limits.input_bytes && bytes.len() as u64 == opened.len();
    bytes.truncate(limits.input_bytes);
    analyze(&bytes, format, limits, started, complete, changed.then_some(NativeLogProblem::InputChanged))
}

fn validate_limits(limits: NativeLogLimits) -> Result<()> {
    if limits.is_valid() { Ok(()) } else { Err(Error::invalid(t!("native_logs.invalid_limits"))) }
}

fn analyze(
    input: &[u8],
    format: NativeLogFormat,
    limits: NativeLogLimits,
    started: String,
    complete_input: bool,
    input_problem: Option<NativeLogProblem>,
) -> Result<NativeLogAnalysis> {
    let artifact = NativeLogArtifact {
        format,
        sha256: digest(input),
        bytes: input.len() as u64,
        complete: complete_input,
        limitation: if complete_input { None } else { Some(input_problem.unwrap_or(NativeLogProblem::InputLimit)) },
    };
    // Only a byte-budget cut may drop an incomplete trailing UTF-8 scalar before framing. Invalid UTF-8 elsewhere
    // remains an explicit parse stop, and the artifact digest always covers the exact observed prefix bytes.
    let parse_input = match std::str::from_utf8(input) {
        Err(error) if !complete_input && error.error_len().is_none() => &input[..error.valid_up_to()],
        _ => input,
    };
    let parsed = structured_logs::parse(parse_input, provider_format(format), limits);
    let decoded = match parsed {
        Ok(batch) => safe_rows(batch),
        Err(NativeLogProblem::InvalidLimits) => return Err(Error::invalid(t!("native_logs.invalid_limits"))),
        Err(problem) => DecodedLog {
            observations: vec![],
            issues: vec![],
            stopped: Some(problem),
            records_examined: 0,
            non_failure_records: 0,
        },
    };
    let partial = !complete_input || decoded.stopped.is_some() || !decoded.issues.is_empty();
    let reason = partial.then(|| message!("native_logs.partial"));
    let source = SourceOutcome {
        source: SOURCE.into(),
        state: if partial {
            SourceState::Partial
        } else if decoded.observations.is_empty() {
            SourceState::Empty
        } else {
            SourceState::Collected
        },
        reason: reason.as_ref().map(|message| message.render()),
        reason_message: reason,
    };
    let evidence = NativeFailureSnapshot {
        source: SOURCE.into(),
        observed_from: started,
        observed_until: utc_now(),
        resource: None,
        observations: decoded.observations,
    };
    if !evidence.is_valid() {
        return Err(Error::invalid(t!("native_logs.invalid_capture")));
    }
    let report = culprit_checks::native_failures::explain(&evidence, Some(&source))?;
    Ok(NativeLogAnalysis {
        input: artifact,
        evidence,
        source,
        records_examined: decoded.records_examined,
        non_failure_records: decoded.non_failure_records,
        issues: decoded.issues,
        stopped: decoded.stopped,
        report,
    })
}

struct DecodedLog {
    observations: Vec<NativeFailureRecord>,
    issues: Vec<NativeLogIssue>,
    stopped: Option<NativeLogProblem>,
    records_examined: u32,
    non_failure_records: u32,
}

fn safe_rows(batch: ParsedLogBatch) -> DecodedLog {
    let mut issues = batch.issues;
    let mut observations = vec![];
    for row in batch.records {
        let position = row.position;
        match row.into_observation() {
            Ok(row) => observations.push(NativeFailureRecord::PostgresLog(Box::new(row))),
            Err(_) => issues.push(NativeLogIssue { position, problem: NativeLogProblem::NativeShape }),
        }
    }
    issues.sort_by_key(|issue| issue.position);
    DecodedLog {
        observations,
        issues,
        stopped: batch.stopped,
        records_examined: batch.records_examined,
        non_failure_records: batch.non_failure_records,
    }
}

fn provider_format(format: NativeLogFormat) -> LogFormat {
    match format {
        NativeLogFormat::PostgresJson => LogFormat::Json,
        NativeLogFormat::PostgresCsv13 => LogFormat::Csv13,
        NativeLogFormat::PostgresCsv => LogFormat::Csv,
    }
}
fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}
fn not_regular_file() -> Error {
    Error::invalid(t!("native_logs.not_regular_file"))
}
fn read_failed(error: std::io::Error) -> Error {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        _ => ErrorCode::InvalidInput,
    };
    Error::new(code, t!("native_logs.read_failed"))
}

#[cfg(test)]
mod tests;
