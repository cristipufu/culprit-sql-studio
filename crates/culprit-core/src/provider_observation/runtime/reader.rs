//! One original reader token, shared absolute snapshots and explicit final disposition.
use super::*;
#[derive(Clone)]
pub struct ProviderReaderObserver {
    pub(super) context: ProviderReadContext,
    pub(super) id: Option<u32>,
}
pub struct ProviderReaderObservation {
    pub(super) context: ProviderReadContext,
    pub(super) id: Option<u32>,
    pub(super) finished: bool,
}
impl ProviderReaderObserver {
    pub fn checkpoint(&self, queries: u32, metrics: Option<ReadWorkMetrics>) {
        record_reader(&self.context, self.id, queries, metrics, false);
    }
    pub fn setup_included(&self) {
        record_setup(&self.context, self.id, ProviderSetupAccounting::IncludedInReaderWork);
    }
    pub fn setup_not_applicable(&self) {
        record_setup(&self.context, self.id, ProviderSetupAccounting::NotApplicable);
    }
    pub fn setup_checkpoint(&self, queries: u32, metrics: Option<ReadWorkMetrics>) {
        record_setup(&self.context, self.id, ProviderSetupAccounting::Separate);
        record_reader(&self.context, self.id, queries, metrics, true);
    }
}

impl ProviderReaderObservation {
    pub fn observer(&self) -> ProviderReaderObserver {
        ProviderReaderObserver { context: self.context.clone(), id: self.id }
    }
    /// Publish the original absolute owner count, never add cumulative snapshots.
    pub fn checkpoint(&self, queries: u32, metrics: Option<ReadWorkMetrics>) {
        self.observer().checkpoint(queries, metrics);
    }
    pub fn setup_included(&self) {
        self.observer().setup_included();
    }
    pub fn setup_not_applicable(&self) {
        self.observer().setup_not_applicable();
    }
    pub fn setup_checkpoint(&self, queries: u32, metrics: Option<ReadWorkMetrics>) {
        self.observer().setup_checkpoint(queries, metrics);
    }
    pub fn finish(mut self, shutdown: SourceShutdown) {
        self.context.update(|state| {
            if let Some(reader) = self.id.and_then(|id| state.readers.get_mut(&id)) {
                if reader.disposition == ProviderReaderDisposition::Unconfirmed {
                    return;
                }
                reader.disposition = match shutdown {
                    SourceShutdown::Quiescent => ProviderReaderDisposition::Quiescent,
                    SourceShutdown::Unconfirmed => ProviderReaderDisposition::Unconfirmed,
                };
            }
        });
        self.finished = true;
    }
}
impl Drop for ProviderReaderObservation {
    fn drop(&mut self) {
        if !self.finished {
            self.context.update(|state| {
                if let Some(reader) = self.id.and_then(|id| state.readers.get_mut(&id)) {
                    reader.disposition = ProviderReaderDisposition::Abandoned;
                }
            });
        }
    }
}
fn record_reader(
    context: &ProviderReadContext,
    id: Option<u32>,
    queries: u32,
    metrics: Option<ReadWorkMetrics>,
    setup: bool,
) {
    let summary = metrics.and_then(|metrics| metrics.summary(None).ok()).map(Box::new);
    context.update(|state| {
        let Some(reader) = id.and_then(|id| state.readers.get_mut(&id)) else {
            return;
        };
        if matches!(
            reader.disposition,
            ProviderReaderDisposition::Unconfirmed
                | ProviderReaderDisposition::Abandoned
                | ProviderReaderDisposition::Quiescent
        ) {
            return;
        }
        let (before_queries, before_work) = if setup {
            (&mut reader.setup_queries, &mut reader.setup_work)
        } else {
            (&mut reader.queries, &mut reader.work)
        };
        let invalid = before_queries.is_some_and(|before| queries < before)
            || match (before_work.as_deref(), summary.as_deref()) {
                (Some(before), Some(after)) => !progresses(before, after),
                (Some(_), None) => true,
                _ => false,
            };
        if invalid {
            reader.disposition = ProviderReaderDisposition::Unconfirmed;
            reader.queries = None;
            reader.setup_queries = None;
            reader.work = None;
            reader.setup_work = None;
            state.gap(ProviderReadGap::InvalidObservation);
            return;
        }
        *before_queries = Some(queries);
        *before_work = summary;
    });
}
fn progresses(before: &ReadWorkSummary, after: &ReadWorkSummary) -> bool {
    connection_progresses(before.connection_work.as_deref(), after.connection_work.as_deref())
        && after.untracked_tasks >= before.untracked_tasks
        && before.templates.iter().all(|before| {
            after.templates.iter().find(|after| after.template_id == before.template_id).is_some_and(|after| {
                after.completed_tasks >= before.completed_tasks
                    && after.failures >= before.failures
                    && after.timeouts >= before.timeouts
                    && after.cancellations >= before.cancellations
                    && after.unconfirmed_cancellations >= before.unconfirmed_cancellations
                    && before.stop_causes.iter().all(|cause| {
                        after.stop_causes.iter().any(|next| next.reason == cause.reason && next.tasks >= cause.tasks)
                    })
                    && after.client_elapsed_ms >= before.client_elapsed_ms
                    && before
                        .rows_decoded
                        .parse::<u64>()
                        .ok()
                        .zip(after.rows_decoded.parse::<u64>().ok())
                        .is_some_and(|(before, after)| after >= before)
                    && before
                        .charged_value_bytes
                        .parse::<u64>()
                        .ok()
                        .zip(after.charged_value_bytes.parse::<u64>().ok())
                        .is_some_and(|(before, after)| after >= before)
            })
        })
}

fn connection_progresses(before: Option<&ReadConnectionWork>, after: Option<&ReadConnectionWork>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => after.follows(before),
        (Some(_), None) => false,
        _ => true,
    }
}

fn record_setup(context: &ProviderReadContext, id: Option<u32>, setup: ProviderSetupAccounting) {
    context.update(|state| {
        let Some(reader) = id.and_then(|id| state.readers.get_mut(&id)) else {
            return;
        };
        if reader.disposition != ProviderReaderDisposition::Pending {
            return;
        }
        if reader.setup_accounting != ProviderSetupAccounting::Unknown && reader.setup_accounting != setup {
            reader.disposition = ProviderReaderDisposition::Unconfirmed;
            reader.queries = None;
            reader.setup_queries = None;
            reader.work = None;
            reader.setup_work = None;
            reader.setup_accounting = ProviderSetupAccounting::Unknown;
            state.gap(ProviderReadGap::InvalidObservation);
        } else {
            reader.setup_accounting = setup;
        }
    });
}
