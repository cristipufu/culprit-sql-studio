//! Paid qualification phases. Ordinary source evidence carries no such auxiliary work.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ReadPhaseAccounting {
    pub queries: u32,
    /// Original template hashes and counters; preview availability grants no native authority.
    pub metrics: Box<ReadWorkMetrics>,
    /// Actual connection SET batches, distinct from reader queries and unavailable native setup CPU/I/O.
    pub session_setup: Box<ReadWorkMetrics>,
}
impl ReadPhaseAccounting {
    pub fn is_valid(&self) -> bool {
        self.queries <= CollectionBudget::upper_limits().max_queries
            && unnested(&self.metrics)
            && self.session_setup.native_session_work.is_none()
            && self.metrics.is_valid()
            && self.session_setup.is_valid()
            && tasks(&self.metrics) <= u64::from(self.queries)
    }
    fn complete(&self) -> bool {
        self.is_valid()
            && settled(&self.metrics)
            && settled(&self.session_setup)
            && tasks(&self.metrics) == u64::from(self.queries)
    }
    fn dispatches(&self) -> u64 {
        u64::from(self.queries) + tasks(&self.session_setup)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlQualificationReadWork {
    /// One parent allowance and absolute lifetime covered preparation, both connections and source work.
    pub aggregate_budget: CollectionBudget,
    pub preparation: ReadPhaseAccounting,
    pub observer: ReadPhaseAccounting,
}
impl MssqlQualificationReadWork {
    pub fn is_valid(&self) -> bool {
        self.aggregate_budget.validate().is_ok() && self.preparation.is_valid() && self.observer.is_valid()
    }
    /// Numerical-window admission only. This supplies neither grant nor active-cancellation certification.
    pub fn covers_source(&self, source: &ReadWorkMetrics, queries: u32) -> bool {
        let Some(interval) = native(source).and_then(|work| work.interval.as_deref()) else {
            return false;
        };
        let Some(prepared) = native(&self.preparation.metrics).and_then(|work| work.interval.as_deref()) else {
            return false;
        };
        let Some(observer) = native(&self.observer.metrics).and_then(|work| work.interval.as_deref()) else {
            return false;
        };
        source.is_valid()
            && self.is_valid()
            && self.preparation.queries > 0
            && self.observer.queries > 0
            && self.preparation.complete()
            && self.observer.complete()
            && settled(source)
            && native(source).is_some_and(|work| work.settlement == NativeReadSettlement::Quiescent)
            && native(&self.observer.metrics).is_some_and(|work| work.settlement == NativeReadSettlement::Quiescent)
            && tasks(source) == u64::from(queries)
            && u64::from(queries) + self.preparation.dispatches() + self.observer.dispatches()
                <= u64::from(self.aggregate_budget.max_queries)
            && [
                source,
                self.preparation.metrics.as_ref(),
                self.preparation.session_setup.as_ref(),
                self.observer.metrics.as_ref(),
                self.observer.session_setup.as_ref(),
            ]
            .into_iter()
            .try_fold(0u64, |sum, metrics| sum.checked_add(bytes(metrics)?))
            .is_some_and(|bytes| bytes <= u64::from(self.aggregate_budget.total_bytes))
            && interval.initial.query_ordinal == 0
            && interval.latest.as_ref().is_some_and(|point| point.query_ordinal == queries)
            && prepared.session == interval.session
            && prepared.reader_generation == interval.reader_generation
            && interval.context.matches_earlier_scope(&prepared.context)
            && observer.session != interval.session
            && observer.context.matches_earlier_scope(&prepared.context)
    }
}
fn native(metrics: &ReadWorkMetrics) -> Option<&MssqlSessionReadWork> {
    let NativeSessionReadWork::MssqlSession(work) = metrics.native_session_work.as_deref()?;
    Some(work)
}
fn unnested(metrics: &ReadWorkMetrics) -> bool {
    native(metrics).is_none_or(|work| work.qualification_work.is_none())
}
fn tasks(metrics: &ReadWorkMetrics) -> u64 {
    u64::from(metrics.active_tasks)
        + u64::from(metrics.untracked_tasks)
        + metrics.templates.iter().map(|template| u64::from(template.completed_tasks)).sum::<u64>()
}
fn bytes(metrics: &ReadWorkMetrics) -> Option<u64> {
    metrics.templates.iter().try_fold(0u64, |sum, template| sum.checked_add(template.charged_value_bytes.parse().ok()?))
}
fn settled(metrics: &ReadWorkMetrics) -> bool {
    metrics.active_tasks == 0
        && metrics.untracked_tasks == 0
        && !metrics.has_unconfirmed_work()
        && metrics.templates.iter().all(|template| template.failures == 0)
        && native(metrics).is_none_or(|work| work.issue.is_none())
}

#[cfg(test)]
mod tests;
