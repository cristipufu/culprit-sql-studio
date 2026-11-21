//! A bounded observer of existing connection/read owners; guards add no scheduling or native work.
use super::*;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
#[derive(Clone, Default)]
pub struct ReadConnectionRecorder {
    state: Arc<Mutex<State>>,
}
#[derive(Default)]
struct State {
    work: Option<ReadConnectionWork>,
}
impl State {
    fn attempt_mut(&mut self, index: Option<usize>) -> Option<&mut ReadConnectionAttempt> {
        self.work.as_mut()?.attempts.get_mut(index?)
    }
}
#[derive(Clone)]
pub struct ReadConnectionAttemptRecorder {
    recorder: ReadConnectionRecorder,
    index: Option<usize>,
}
pub struct ReadConnectionPhaseGuard {
    attempt: ReadConnectionAttemptRecorder,
    index: Option<usize>,
    started: Instant,
    finished: bool,
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}
impl ReadConnectionRecorder {
    pub fn begin(&self) -> ReadConnectionAttemptRecorder {
        let index = self.state.lock().ok().and_then(|mut state| {
            let work = state.work.get_or_insert_with(|| ReadConnectionWork {
                attempted: 0,
                omitted_attempts: 0,
                omitted_phases: 0,
                active_phases: 0,
                unconfirmed_phases: 0,
                attempts: vec![],
            });
            work.attempted = work.attempted.saturating_add(1);
            if work.attempts.len() == MAX_READ_CONNECTION_ATTEMPTS {
                work.omitted_attempts = work.omitted_attempts.saturating_add(1);
                return None;
            }
            let index = work.attempts.len();
            work.attempts.push(ReadConnectionAttempt {
                ordinal: work.attempted,
                connection_use: ReadConnectionUse::Unknown,
                reader_generation: None,
                phases: vec![],
                omitted_phases: 0,
            });
            Some(index)
        });
        ReadConnectionAttemptRecorder { recorder: self.clone(), index }
    }
    pub fn snapshot(&self) -> Option<ReadConnectionWork> {
        self.state.lock().ok()?.work.clone()
    }
}
impl ReadConnectionAttemptRecorder {
    pub fn connection_use(&self, value: ReadConnectionUse) {
        if let Ok(mut state) = self.recorder.state.lock() {
            if let Some(attempt) = state.attempt_mut(self.index) {
                if attempt.connection_use == ReadConnectionUse::Unknown {
                    attempt.connection_use = value;
                }
            }
        }
    }
    pub fn acquired(&self, generation: u64) {
        if generation == 0 {
            return;
        }
        if let Ok(mut state) = self.recorder.state.lock() {
            if let Some(attempt) = state.attempt_mut(self.index) {
                if attempt.reader_generation.is_none() {
                    attempt.reader_generation = Some(generation.to_string())
                }
            }
        }
    }
    pub fn phase(&self, stage: ReadConnectionStage, template_id: Option<String>) -> ReadConnectionPhaseGuard {
        let index = self.recorder.state.lock().ok().and_then(|mut state| {
            let work = state.work.as_mut()?;
            work.active_phases = work.active_phases.saturating_add(1);
            let attempt = match self.index {
                Some(index) => work.attempts.get_mut(index),
                None => None,
            };
            if let Some(attempt) = attempt {
                if attempt.phases.len() < MAX_READ_CONNECTION_PHASES {
                    let index = attempt.phases.len();
                    attempt.phases.push(ReadConnectionPhase {
                        stage,
                        template_id,
                        outcome: ReadConnectionOutcome::Pending,
                        client_started_at: now(),
                        client_finished_at: None,
                        client_elapsed_ms: None,
                    });
                    return Some(index);
                }
                attempt.omitted_phases = attempt.omitted_phases.saturating_add(1);
            }
            work.omitted_phases = work.omitted_phases.saturating_add(1);
            None
        });
        ReadConnectionPhaseGuard { attempt: self.clone(), index, started: Instant::now(), finished: false }
    }
}
impl ReadConnectionPhaseGuard {
    pub fn finish(mut self, outcome: ReadConnectionOutcome) {
        self.settle(outcome);
    }
    fn settle(&mut self, outcome: ReadConnectionOutcome) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Ok(mut state) = self.attempt.recorder.state.lock() {
            if let Some(work) = &mut state.work {
                work.active_phases = work.active_phases.saturating_sub(1);
                work.unconfirmed_phases =
                    work.unconfirmed_phases.saturating_add(u32::from(outcome == ReadConnectionOutcome::Unconfirmed));
                let attempt = match self.attempt.index {
                    Some(index) => work.attempts.get_mut(index),
                    None => None,
                };
                if let Some(phase) = attempt.and_then(|attempt| attempt.phases.get_mut(self.index?)) {
                    phase.outcome = outcome;
                    phase.client_finished_at = Some(now());
                    phase.client_elapsed_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
    }
}
impl Drop for ReadConnectionPhaseGuard {
    fn drop(&mut self) {
        self.settle(ReadConnectionOutcome::Unconfirmed);
    }
}
