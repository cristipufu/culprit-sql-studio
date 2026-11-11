//! Retained native events describe what was observable, independently from historical completeness.
use super::*;
use crate::{DeadlockPattern, DeadlockProcess, DeadlockResource};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct EventHistoryDescription {
    pub from: String,
    pub until: String,
    pub observed_at: String,
    /// Earliest guaranteed retained timestamp. An earliest returned event does not establish this value.
    pub retained_from: Option<String>,
    /// False unless the native source establishes continuity throughout the requested history.
    pub history_complete: bool,
}
impl EventHistoryDescription {
    pub(super) fn is_valid(&self) -> bool {
        let time = |value: &str| DateTime::parse_from_rfc3339(value).ok();
        matches!((time(&self.from),time(&self.until),time(&self.observed_at)),(Some(from),Some(until),Some(at)) if from<until && until<=at)
            && self.retained_from.as_deref().is_none_or(|value| time(value).is_some())
            && (!self.history_complete
                || self
                    .retained_from
                    .as_deref()
                    .and_then(time)
                    .zip(time(&self.from))
                    .is_some_and(|(retained, from)| retained <= from))
    }
    pub(super) fn accepts(&self, at: &str) -> bool {
        let time = |value: &str| DateTime::parse_from_rfc3339(value).ok();
        matches!((time(&self.from),time(at),time(&self.until)),(Some(from),Some(at),Some(until)) if from<=at && at<until)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct DeadlockEvent {
    /// Native file/buffer/event digest, established before redaction. Separate graphs have separate ordinals.
    pub position: String,
    pub file_id: String,
    pub file_offset: String,
    pub graph_ordinal: u32,
    pub at: String,
    pub processes: Vec<DeadlockProcess>,
    pub resources: Vec<DeadlockResource>,
    pub pattern: DeadlockPattern,
}
impl DeadlockEvent {
    pub fn redact(&mut self, engine: crate::Engine) {
        for process in &mut self.processes {
            process.statement = process.statement.as_deref().map(|sql| crate::redact::redact_sql(sql, engine));
            process.login = None;
            process.host = None;
            process.application = None;
        }
    }
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.position)
            && valid_digest(&self.file_id)
            && self.file_offset.parse::<u64>().is_ok()
            && self.file_offset.len() <= 20
            && DateTime::parse_from_rfc3339(&self.at).is_ok()
            && !self.processes.is_empty()
            && self.processes.len() <= 4096
            && self.resources.len() <= 4096
    }
}
