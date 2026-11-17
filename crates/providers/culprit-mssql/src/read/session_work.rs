//! Two bounded native points on the held source socket. Missing observations never trigger replacement SQL.
use crate::{
    local_source::Context,
    read::{NativeRead, invalid_data, optional_counter, optional_text},
    value::{Rec, Val},
};
use culprit_core::{Result, telemetry::*};

pub(crate) enum Observation {
    Started,
    Point(Box<ObservedPoint>),
    Unavailable(NativeReadWorkIssue),
}
pub(crate) struct ObservedPoint {
    authority: Context,
    session: NativeActivityIdentity,
    point: MssqlSessionReadPoint,
    generation: u64,
}

/// Continuity-only watermarks. These merged values are never persisted or presented as an observed point.
#[derive(Default)]
struct CounterWatermark([Option<u64>; 4]);
impl CounterWatermark {
    fn accepts(&self, point: &MssqlSessionReadPoint) -> bool {
        self.0
            .into_iter()
            .zip(point.cumulative_counts())
            .all(|(before, after)| before.zip(after).is_none_or(|(before, after)| after >= before))
    }
    fn observe(&mut self, point: &MssqlSessionReadPoint) {
        for (before, after) in self.0.iter_mut().zip(point.cumulative_counts()) {
            if let Some(after) = after {
                *before = Some(before.unwrap_or(0).max(after));
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct SessionWork {
    work: Option<MssqlSessionReadWork>,
    authority: Option<Context>,
    counters: CounterWatermark,
    awaiting_context: bool,
}
impl SessionWork {
    pub(crate) fn observe(&mut self, observation: Observation) {
        if self.work.as_ref().is_some_and(|work| work.settlement != NativeReadSettlement::Pending) {
            return;
        }
        match observation {
            Observation::Started => self.awaiting_context = true,
            Observation::Unavailable(issue) => {
                self.awaiting_context = false;
                self.poison(issue);
            }
            Observation::Point(point) => {
                self.awaiting_context = false;
                self.point(point.authority, point.session, point.point, point.generation);
            }
        }
    }
    fn point(
        &mut self,
        authority: Context,
        session: NativeActivityIdentity,
        point: MssqlSessionReadPoint,
        generation: u64,
    ) {
        if self.work.as_ref().is_some_and(|work| work.issue.is_some()) {
            return;
        }
        if self.work.is_none() {
            let context = match authority.completed(&authority) {
                Ok(context) => context,
                Err(_) => {
                    self.poison(NativeReadWorkIssue::InvalidObservation);
                    return;
                }
            };
            let issue = if !authority.is_qualified()
                || !context.has_qualified_local_owner()
                || context.database_writable.is_none()
            {
                Some(NativeReadWorkIssue::NativeScopeUnqualified)
            } else if !point.has_counters() {
                Some(NativeReadWorkIssue::MissingCounters)
            } else {
                None
            };
            self.counters.observe(&point);
            self.work = Some(MssqlSessionReadWork {
                settlement: NativeReadSettlement::Pending,
                issue,
                interval: Some(Box::new(MssqlSessionReadInterval {
                    context,
                    session,
                    reader_generation: generation.to_string(),
                    initial: point,
                    latest: None,
                })),
            });
            self.authority = Some(authority);
            return;
        }
        let issue = self.changed(&authority, &session, &point, generation);
        if let Some(issue) = issue {
            if matches!(issue, NativeReadWorkIssue::CounterDecreased | NativeReadWorkIssue::MissingCounters) {
                self.retain(&authority, point);
            }
            self.poison(issue);
            return;
        }
        self.counters.observe(&point);
        self.retain(&authority, point);
    }
    fn retain(&mut self, authority: &Context, point: MssqlSessionReadPoint) {
        let Some(work) = &mut self.work else {
            return;
        };
        let Some(interval) = &mut work.interval else {
            return;
        };
        let before = self.authority.as_ref();
        match before.and_then(|before| before.completed(authority).ok()) {
            Some(context) => {
                interval.context = context;
                interval.latest = Some(point);
            }
            None => self.poison(NativeReadWorkIssue::ClockChanged),
        }
    }
    fn changed(
        &self,
        authority: &Context,
        session: &NativeActivityIdentity,
        point: &MssqlSessionReadPoint,
        generation: u64,
    ) -> Option<NativeReadWorkIssue> {
        let work = self.work.as_ref()?;
        let interval = work.interval.as_ref()?;
        if interval.reader_generation != generation.to_string() {
            return Some(NativeReadWorkIssue::ConnectionChanged);
        }
        if &interval.session != session {
            return Some(NativeReadWorkIssue::SessionChanged);
        }
        let before = self.authority.as_ref()?;
        if !authority.is_qualified() {
            return Some(NativeReadWorkIssue::NativeScopeUnqualified);
        }
        if !before.same_source(authority) {
            return Some(NativeReadWorkIssue::SourceChanged);
        }
        let previous = interval.latest.as_ref().unwrap_or(&interval.initial);
        if point.query_ordinal <= previous.query_ordinal {
            return Some(NativeReadWorkIssue::InvalidObservation);
        }
        if ObservationBounds::new(&interval.context.observed_until, &authority.activity.at).is_none() {
            return Some(NativeReadWorkIssue::ClockChanged);
        }
        if !self.counters.accepts(point) {
            return Some(NativeReadWorkIssue::CounterDecreased);
        }
        if !point.has_counters() {
            return Some(NativeReadWorkIssue::MissingCounters);
        }
        None
    }
    pub(crate) fn poison(&mut self, issue: NativeReadWorkIssue) {
        let work = self.work.get_or_insert(MssqlSessionReadWork {
            settlement: NativeReadSettlement::Pending,
            issue: None,
            interval: None,
        });
        work.issue.get_or_insert(issue);
    }
    pub(crate) fn generation(&mut self, generation: u64) {
        if self
            .work
            .as_ref()
            .and_then(|work| work.interval.as_ref())
            .is_some_and(|interval| interval.reader_generation != generation.to_string())
        {
            self.poison(NativeReadWorkIssue::ConnectionChanged);
        }
    }
    pub(crate) fn close(&mut self, shutdown: SourceShutdown, generation: u64) {
        if self.awaiting_context {
            self.poison(NativeReadWorkIssue::MissingFinalObservation);
        }
        let Some(work) = &mut self.work else {
            return;
        };
        if work.interval.as_ref().is_some_and(|interval| interval.reader_generation != generation.to_string()) {
            work.issue.get_or_insert(NativeReadWorkIssue::ConnectionChanged);
        }
        if work.settlement != NativeReadSettlement::Unconfirmed {
            work.settlement = match shutdown {
                SourceShutdown::Quiescent => NativeReadSettlement::Quiescent,
                SourceShutdown::Unconfirmed => NativeReadSettlement::Unconfirmed,
            };
        }
        if shutdown == SourceShutdown::Quiescent {
            if work.interval.as_ref().is_some_and(|interval| interval.latest.is_none()) {
                work.issue.get_or_insert(NativeReadWorkIssue::MissingFinalObservation);
            } else if work.interval.as_ref().is_some_and(|interval| !interval.has_comparable_counters()) {
                work.issue.get_or_insert(NativeReadWorkIssue::MissingCounters);
            }
        }
    }
    pub(crate) fn snapshot(&self) -> Option<Box<NativeSessionReadWork>> {
        self.work.as_ref().map(|work| Box::new(NativeSessionReadWork::MssqlSession(Box::new(work.clone()))))
    }
}

pub(crate) fn observe<R: NativeRead>(reader: &mut R, authority: &Context, row: &Rec) {
    let observation = observation(authority, row, reader.queries(), reader.connection_generation());
    reader.observe_session_work(observation);
}
fn observation(authority: &Context, row: &Rec, query_ordinal: u32, generation: Option<u64>) -> Observation {
    if generation.is_none_or(|generation| generation == 0) {
        return Observation::Unavailable(NativeReadWorkIssue::NativeScopeUnqualified);
    }
    match point(authority, row, query_ordinal, generation) {
        Ok(Some(point)) => Observation::Point(Box::new(point)),
        Ok(None) => Observation::Unavailable(NativeReadWorkIssue::MissingSession),
        Err(_) => Observation::Unavailable(NativeReadWorkIssue::InvalidObservation),
    }
}
fn point(authority: &Context, row: &Rec, query_ordinal: u32, generation: Option<u64>) -> Result<Option<ObservedPoint>> {
    let Some(generation) = generation.filter(|generation| *generation > 0) else {
        return Ok(None);
    };
    let id = optional(row, "collector_session_id")?;
    let login = if field(row, "collector_login_without_offset")?.is_none() {
        None
    } else {
        optional_text(row, "collector_login_without_offset")?
    };
    let (Some(id), Some(login)) = (id, login) else {
        return Ok(None);
    };
    let native_id = id.parse::<u16>().map_err(|_| invalid_data())?;
    if !(1..=i16::MAX as u16).contains(&native_id) {
        return Err(invalid_data());
    }
    let session = NativeActivityIdentity { id, generation: crate::sql_time::native_time(&login)? };
    let point = MssqlSessionReadPoint {
        query_ordinal,
        cpu_ms: optional(row, "collector_cpu_ms")?,
        reads: optional(row, "collector_reads")?,
        logical_reads: optional(row, "collector_logical_reads")?,
        dirty_marks: optional(row, "collector_dirty_marks")?,
        memory_pages: optional(row, "collector_memory_pages")?,
    };
    if !point.is_valid() || session.generation.starts_with("1900-01-01") {
        return Err(invalid_data());
    }
    Ok(Some(ObservedPoint { authority: authority.clone(), session, point, generation }))
}
fn optional(row: &Rec, name: &str) -> Result<Option<String>> {
    if field(row, name)?.is_none() {
        return Ok(None);
    }
    optional_counter(row, name)
}
fn field<'a>(row: &'a Rec, name: &str) -> Result<Option<&'a Val>> {
    let mut fields = row.columns().filter(|(key, _)| *key == name);
    let first = fields.next().map(|(_, value)| value);
    if fields.next().is_some() {
        return Err(invalid_data());
    }
    Ok(first)
}

#[cfg(test)]
mod tests;
