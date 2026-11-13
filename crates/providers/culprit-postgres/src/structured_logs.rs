//! Acquisition-independent decoding of supplied PostgreSQL structured logs. This module opens no files or
//! connections and never enables logging. Feed authentication, namespace qualification and history coverage belong
//! to the caller; parsing a payload cannot establish them.
//!
//! Native fields and SQLSTATE meanings:
//! https://www.postgresql.org/docs/current/runtime-config-logging.html
//! https://www.postgresql.org/docs/current/errcodes-appendix.html

mod csv;
mod json;
mod record;
#[cfg(test)]
mod tests;

pub use culprit_core::{NativeLogIssue, NativeLogLimits, NativeLogProblem};
pub use record::{NativeSessionLine, PostgresFailureKind, PostgresLogRecord};

/// CSV log columns are positional. Require the declared layout instead of inferring a layout from row contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// PostgreSQL 15 and later `jsonlog`: one native JSON object per record.
    Json,
    /// PostgreSQL 13's documented 24-column `csvlog`, without leader PID and query ID.
    Csv13,
    /// PostgreSQL 14 and later's documented 26-column `csvlog`, including leader PID and query ID.
    Csv,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedLogBatch {
    /// Native failures retain input order and duplicate rows. Source qualification is required before any dedup.
    pub records: Vec<PostgresLogRecord>,
    pub issues: Vec<NativeLogIssue>,
    pub records_examined: u32,
    pub non_failure_records: u32,
    /// A framing or budget stop means the supplied payload was not fully examined.
    pub stopped: Option<NativeLogProblem>,
}
impl ParsedLogBatch {
    fn new() -> Self {
        Self { records: vec![], issues: vec![], records_examined: 0, non_failure_records: 0, stopped: None }
    }
    /// Only the supplied payload's parsing. It makes no claim about lost log records or historical coverage.
    pub fn input_complete(&self) -> bool {
        self.stopped.is_none() && self.issues.is_empty()
    }
    /// A native database name is affected context, not proof of feed/resource ownership or a current session join.
    pub fn database_context<'a>(&'a self, database: &'a str) -> impl Iterator<Item = &'a PostgresLogRecord> {
        self.records.iter().filter(move |record| record.database.as_deref() == Some(database))
    }
    fn accept(&mut self, decoded: std::result::Result<Option<PostgresLogRecord>, NativeLogProblem>) {
        let position = self.records_examined;
        self.records_examined += 1;
        match decoded {
            Ok(Some(mut record)) => {
                record.position = position;
                self.records.push(record);
            }
            Ok(None) => self.non_failure_records += 1,
            Err(problem) => self.issues.push(NativeLogIssue { position, problem }),
        }
    }
}

/// Parse a bounded supplied payload through the same decoder regardless of its independently approved acquisition.
/// Invalid native fields remain issues; valid earlier failures survive a later malformed or bounded record.
pub fn parse(
    input: &[u8],
    format: LogFormat,
    limits: NativeLogLimits,
) -> std::result::Result<ParsedLogBatch, NativeLogProblem> {
    if !limits.is_valid() {
        return Err(NativeLogProblem::InvalidLimits);
    }
    if input.len() > limits.input_bytes {
        return Err(NativeLogProblem::InputLimit);
    }
    let input = std::str::from_utf8(input).map_err(|_| NativeLogProblem::InvalidUtf8)?;
    Ok(match format {
        LogFormat::Json => json::parse(input, limits),
        LogFormat::Csv13 | LogFormat::Csv => csv::parse(input, format, limits),
    })
}
