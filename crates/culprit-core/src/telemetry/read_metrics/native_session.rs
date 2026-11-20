//! Private source-session proof and a public numerical projection. Neither is a native-cost approval.
use super::*;

mod qualification;
pub use qualification::*;

const MAX_NATIVE_SESSION_WORK_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeReadSettlement {
    Pending,
    Quiescent,
    Unconfirmed,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeReadWorkIssue {
    MissingSession,
    MissingCounters,
    NativeScopeUnqualified,
    InvalidObservation,
    SourceChanged,
    SessionChanged,
    ConnectionChanged,
    CounterDecreased,
    ClockChanged,
    MissingFinalObservation,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeReadWorkState {
    Observing,
    Settled,
    Unavailable,
}

/// Dataset ownership and the independently observed source-session owner are separate facts.
#[derive(Serialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeReadOwnerBinding {
    Matched,
    Unverified,
    Contradictory,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlSessionReadPoint {
    /// Reader-local original query count at this boundary, independent of native completion.
    /// Only an interval's initial point may be zero, before its first capture query.
    pub query_ordinal: u32,
    pub cpu_ms: Option<String>,
    pub reads: Option<String>,
    pub logical_reads: Option<String>,
    /// Native sys.dm_exec_sessions.writes: buffer-pool dirty marks, not storage writes.
    pub dirty_marks: Option<String>,
    /// Native memory_usage gauge, in 8 KiB pages. It is neither a grant nor a peak.
    pub memory_pages: Option<String>,
}
impl MssqlSessionReadPoint {
    pub fn is_valid(&self) -> bool {
        bounded_count(&self.cpu_ms, i32::MAX as u64)
            && bounded_count(&self.memory_pages, i32::MAX as u64)
            && [&self.reads, &self.logical_reads, &self.dirty_marks]
                .into_iter()
                .all(|value| bounded_count(value, i64::MAX as u64))
    }
    pub fn has_counters(&self) -> bool {
        [&self.cpu_ms, &self.reads, &self.logical_reads, &self.dirty_marks, &self.memory_pages]
            .into_iter()
            .any(|value| value.is_some())
    }
    /// Fixed source-schema order: CPU, native reads, logical reads, dirty marks. The memory gauge is excluded.
    pub fn cumulative_counts(&self) -> [Option<u64>; 4] {
        [
            self.cpu_ms.as_deref().and_then(count),
            self.reads.as_deref().and_then(count),
            self.logical_reads.as_deref().and_then(count),
            self.dirty_marks.as_deref().and_then(count),
        ]
    }
    pub fn is_monotonic_after(&self, before: &Self) -> bool {
        before
            .cumulative_counts()
            .into_iter()
            .zip(self.cumulative_counts())
            .all(|(before, after)| before.zip(after).is_none_or(|(before, after)| after >= before))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlSessionReadInterval {
    pub context: NativeReadContext,
    pub session: NativeActivityIdentity,
    /// Reader-local socket acquisition generation, not a native process epoch.
    pub reader_generation: String,
    pub initial: MssqlSessionReadPoint,
    pub latest: Option<MssqlSessionReadPoint>,
}
impl MssqlSessionReadInterval {
    fn is_valid(&self) -> bool {
        self.context.is_valid()
            && self.context.target.database().is_some()
            && self.context.target.engine() == crate::Engine::Mssql
            && self.session.is_valid()
            && self
                .session
                .id
                .parse::<u16>()
                .is_ok_and(|id| (1..=i16::MAX as u16).contains(&id) && id.to_string() == self.session.id)
            && super::super::catalog::local_time(&self.session.generation)
            && !self.session.generation.starts_with("1900-01-01")
            && positive_generation(&self.reader_generation)
            && self.initial.is_valid()
            && self
                .latest
                .as_ref()
                .is_none_or(|latest| latest.is_valid() && latest.query_ordinal > self.initial.query_ordinal)
            && (self.latest.is_some() || self.context.observed_from == self.context.observed_until)
    }
    pub fn has_comparable_counters(&self) -> bool {
        self.latest.as_ref().is_some_and(|latest| {
            self.initial
                .cumulative_counts()
                .into_iter()
                .zip(latest.cumulative_counts())
                .any(|(before, after)| before.is_some() && after.is_some())
                || self.initial.memory_pages.is_some() && latest.memory_pages.is_some()
        })
    }
    fn binding(&self, source: Option<&CollectionDescription>) -> NativeReadOwnerBinding {
        let Some(source) = source else {
            return NativeReadOwnerBinding::Unverified;
        };
        if source.target.engine() != crate::Engine::Mssql {
            return NativeReadOwnerBinding::Contradictory;
        }
        let owner = member(&self.context.partition);
        let original = member(&source.partition);
        if owner.zip(original).is_some_and(|(owner, original)| owner != original) {
            return NativeReadOwnerBinding::Contradictory;
        }
        if self.context.target == source.target
            && member(&source.partition).is_some()
            && source.partition.native_epoch().is_some()
        {
            if self.context.partition != source.partition {
                return NativeReadOwnerBinding::Contradictory;
            }
            let original =
                NativeReadContext::from_activity(source).or_else(|_| NativeReadContext::from_snapshot(source));
            if original
                .ok()
                .and_then(|context| context.database_writable)
                .is_some_and(|role| Some(role) != self.context.database_writable)
            {
                return NativeReadOwnerBinding::Contradictory;
            }
            return NativeReadOwnerBinding::Matched;
        }
        // A shared resource may be read from another database. A matching member alone does not equate the
        // resource/counter epoch, and labels or endpoint aliases cannot fill that missing relation.
        NativeReadOwnerBinding::Unverified
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlSessionReadWork {
    /// Paid auxiliary work for this explicit qualification run; never a cost approval.
    pub qualification_work: Option<Box<MssqlQualificationReadWork>>,
    pub settlement: NativeReadSettlement,
    /// First failure retained independently from later shutdown or a return to the original owner/counters.
    pub issue: Option<NativeReadWorkIssue>,
    pub interval: Option<Box<MssqlSessionReadInterval>>,
}
impl MssqlSessionReadWork {
    fn is_valid(&self) -> bool {
        self.qualification_work.as_ref().is_none_or(|work| work.is_valid())
            && self.interval.as_ref().is_none_or(|interval| interval.is_valid())
            && match (&self.interval, self.issue, self.settlement) {
                (None, None, _) => false,
                (Some(interval), None, _)
                    if !interval.context.has_qualified_local_owner()
                        || interval.context.database_writable.is_none() =>
                {
                    false
                }
                (Some(interval), None, NativeReadSettlement::Quiescent) => {
                    interval.latest.as_ref().is_some_and(|latest| {
                        latest.is_monotonic_after(&interval.initial) && interval.has_comparable_counters()
                    })
                }
                _ => true,
            }
    }
    fn state(&self, binding: NativeReadOwnerBinding) -> NativeReadWorkState {
        if self.issue.is_some()
            || self.settlement == NativeReadSettlement::Unconfirmed
            || binding == NativeReadOwnerBinding::Contradictory
        {
            NativeReadWorkState::Unavailable
        } else if self.settlement == NativeReadSettlement::Quiescent {
            NativeReadWorkState::Settled
        } else {
            NativeReadWorkState::Observing
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "profile", content = "observation", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeSessionReadWork {
    MssqlSession(Box<MssqlSessionReadWork>),
}
impl NativeSessionReadWork {
    pub fn is_valid(&self) -> bool {
        (match self {
            Self::MssqlSession(work) => work.is_valid(),
        }) && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_NATIVE_SESSION_WORK_BYTES)
    }
    /// Native ownership settlement is independent of interval availability and cost qualification.
    pub fn settlement(&self) -> NativeReadSettlement {
        match self {
            Self::MssqlSession(work) => work.settlement,
        }
    }
    pub fn has_unconfirmed_work(&self) -> bool {
        self.settlement() == NativeReadSettlement::Unconfirmed
    }
    pub fn summary(&self, source: Option<&CollectionDescription>) -> Result<NativeSessionReadSummary> {
        if !self.is_valid() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let reference = digest(&serde_json::to_vec(self).map_err(|_| DeliveryProblem::Integrity.error())?);
        match self {
            Self::MssqlSession(work) => {
                let binding = work
                    .interval
                    .as_ref()
                    .map_or(NativeReadOwnerBinding::Unverified, |interval| interval.binding(source));
                let state = work.state(binding);
                let interval = work.interval.as_ref().map(|interval| {
                    let measured = state == NativeReadWorkState::Settled;
                    let latest = interval.latest.as_ref().filter(|_| measured);
                    MssqlSessionIntervalSummary {
                        observed_from: interval.context.observed_from.clone(),
                        observed_until: interval.latest.as_ref().map(|_| interval.context.observed_until.clone()),
                        first_query_ordinal: interval.initial.query_ordinal,
                        last_query_ordinal: interval.latest.as_ref().map(|point| point.query_ordinal),
                        cpu_ms: difference(&interval.initial.cpu_ms, latest.and_then(|point| point.cpu_ms.as_ref())),
                        reads: difference(&interval.initial.reads, latest.and_then(|point| point.reads.as_ref())),
                        logical_reads: difference(
                            &interval.initial.logical_reads,
                            latest.and_then(|point| point.logical_reads.as_ref()),
                        ),
                        dirty_marks: difference(
                            &interval.initial.dirty_marks,
                            latest.and_then(|point| point.dirty_marks.as_ref()),
                        ),
                        memory_pages_initial: measured.then(|| interval.initial.memory_pages.clone()).flatten(),
                        memory_pages_latest: latest.and_then(|point| point.memory_pages.clone()),
                    }
                });
                Ok(NativeSessionReadSummary::MssqlSession {
                    reference,
                    settlement: work.settlement,
                    issue: work.issue,
                    binding,
                    state,
                    interval: interval.map(Box::new),
                })
            }
        }
    }
}

#[derive(Serialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "profile", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeSessionReadSummary {
    MssqlSession {
        reference: String,
        settlement: NativeReadSettlement,
        issue: Option<NativeReadWorkIssue>,
        binding: NativeReadOwnerBinding,
        state: NativeReadWorkState,
        interval: Option<Box<MssqlSessionIntervalSummary>>,
    },
}

#[derive(Serialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlSessionIntervalSummary {
    pub observed_from: String,
    pub observed_until: Option<String>,
    pub first_query_ordinal: u32,
    pub last_query_ordinal: Option<u32>,
    pub cpu_ms: Option<String>,
    pub reads: Option<String>,
    pub logical_reads: Option<String>,
    pub dirty_marks: Option<String>,
    pub memory_pages_initial: Option<String>,
    pub memory_pages_latest: Option<String>,
}

fn member(partition: &SourcePartition) -> Option<&SharedResourceIdentity> {
    match partition {
        SourcePartition::InstanceLocal { member, .. } => member.as_ref(),
        SourcePartition::Target => None,
    }
}
fn count(value: &str) -> Option<u64> {
    value.parse().ok()
}
fn bounded_count(value: &Option<String>, maximum: u64) -> bool {
    value
        .as_ref()
        .is_none_or(|value| count(value).is_some_and(|number| number <= maximum && number.to_string() == *value))
}
fn positive_generation(value: &str) -> bool {
    count(value).is_some_and(|number| number > 0 && number.to_string() == value)
}
fn difference(before: &Option<String>, after: Option<&String>) -> Option<String> {
    let (before, after) = before.as_ref().zip(after)?;
    count(after)?.checked_sub(count(before)?).map(|value| value.to_string())
}

#[cfg(test)]
mod tests;
