use super::{LogFormat, NativeLogLimits, NativeLogProblem, ParsedLogBatch, record};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldState {
    Start,
    Unquoted,
    Quoted,
    AfterQuote,
}

/// One bounded CSV record. Quoted newlines are data; neither line splitting nor SQL text determines framing.
struct Record {
    columns: &'static [&'static str],
    fields: Vec<String>,
    field: String,
    state: FieldState,
    bytes: usize,
}
impl Record {
    fn new(format: LogFormat) -> Self {
        Self {
            columns: record::csv_fields(format),
            fields: vec![],
            field: String::new(),
            state: FieldState::Start,
            bytes: 0,
        }
    }
    fn push(&mut self, value: char, limit: usize) -> std::result::Result<(), NativeLogProblem> {
        if self.field.len() + value.len_utf8() > limit {
            return Err(NativeLogProblem::FieldBytes);
        }
        self.field.push(value);
        Ok(())
    }
    fn end_field(&mut self) -> std::result::Result<(), NativeLogProblem> {
        if self.fields.len() >= self.columns.len() {
            return Err(NativeLogProblem::CsvColumnCount);
        }
        self.fields.push(std::mem::take(&mut self.field));
        self.state = FieldState::Start;
        Ok(())
    }
    fn finish(mut self) -> std::result::Result<Option<super::PostgresLogRecord>, NativeLogProblem> {
        if self.state == FieldState::Quoted {
            return Err(NativeLogProblem::MalformedRecord);
        }
        self.end_field()?;
        if self.fields.len() != self.columns.len() {
            return Err(NativeLogProblem::CsvColumnCount);
        }
        let fields: BTreeMap<_, _> = self
            .columns
            .iter()
            .copied()
            .zip(self.fields)
            .map(|(key, value)| (key.to_owned(), Value::String(value)))
            .collect();
        record::decode(fields)
    }
    fn character(&mut self, value: char, limits: NativeLogLimits) -> std::result::Result<(), NativeLogProblem> {
        self.bytes += value.len_utf8();
        if self.bytes > limits.record_bytes {
            return Err(NativeLogProblem::RecordBytes);
        }
        match (self.state, value) {
            (FieldState::Start, '"') => self.state = FieldState::Quoted,
            (FieldState::Quoted, '"') => self.state = FieldState::AfterQuote,
            (FieldState::AfterQuote, '"') => {
                self.push('"', limits.field_bytes)?;
                self.state = FieldState::Quoted;
            }
            (FieldState::Start | FieldState::Unquoted | FieldState::AfterQuote, ',') => self.end_field()?,
            (FieldState::AfterQuote, _) | (FieldState::Unquoted, '"') => return Err(NativeLogProblem::MalformedRecord),
            (state, value) => {
                self.push(value, limits.field_bytes)?;
                if state == FieldState::Start {
                    self.state = FieldState::Unquoted;
                }
            }
        }
        Ok(())
    }
}

pub(super) fn parse(input: &str, format: LogFormat, limits: NativeLogLimits) -> ParsedLogBatch {
    let mut batch = ParsedLogBatch::new();
    let mut record = Record::new(format);
    let mut characters = input.chars().peekable();
    while let Some(value) = characters.next() {
        if batch.records_examined as usize == limits.records {
            batch.stopped = Some(NativeLogProblem::RecordLimit);
            return batch;
        }
        let record_end = record.state != FieldState::Quoted && matches!(value, '\n' | '\r');
        if record_end {
            if value == '\r' && characters.next_if_eq(&'\n').is_none() {
                batch.stopped = Some(NativeLogProblem::MalformedRecord);
                return batch;
            }
            batch.accept(record.finish());
            record = Record::new(format);
        } else if let Err(problem) = record.character(value, limits) {
            // CSV quoting can span lines. After a framing failure there is no safe guessed newline resync.
            batch.stopped = Some(problem);
            return batch;
        }
    }
    if record.bytes > 0 {
        batch.accept(record.finish());
    }
    batch
}
