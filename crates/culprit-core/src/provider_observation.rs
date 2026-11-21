//! One normal probe/scan's client accounting. These values establish no native identity, cost or admission.
use crate::{Engine, Error, ErrorCode, Result, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
mod runtime;
pub use runtime::{
    ProviderFactoryObservation, ProviderOperationObservation, ProviderReadContext, ProviderReadObservation,
    ProviderReaderObservation, ProviderReaderObserver,
};

pub const MAX_PROVIDER_READERS: usize = 32;
pub const MAX_PROVIDER_ACQUISITION_BYTES: usize = 128 * 1024;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderReadKind {
    Probe,
    Scan,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderReadDisposition {
    NotStarted,
    Quiescent,
    Unconfirmed,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderReadGap {
    NotInstrumented,
    PartialReaderCoverage,
    UnhookedEnrichment,
    UnhookedCancellation,
    OmittedReaders,
    ReaderAbandoned,
    MissingReaderSnapshot,
    MissingSetupSnapshot,
    UntrackedTasks,
    InvalidObservation,
    Oversized,
    ClientClockChanged,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ProviderReadCoverage {
    Complete,
    Partial { gaps: Vec<ProviderReadGap> },
    Unavailable { reason: ProviderReadGap },
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderReaderDisposition {
    Pending,
    Quiescent,
    Unconfirmed,
    Abandoned,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderSetupAccounting {
    Unknown,
    IncludedInReaderWork,
    Separate,
    NotApplicable,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ProviderReaderSummary {
    pub reader_id: u32,
    pub engine: Engine,
    pub setup_accounting: ProviderSetupAccounting,
    #[serde(deserialize_with = "required_nullable")]
    pub queries: Option<u32>,
    #[serde(deserialize_with = "required_nullable")]
    pub setup_queries: Option<u32>,
    #[serde(deserialize_with = "required_nullable")]
    pub work: Option<Box<ReadWorkSummary>>,
    #[serde(deserialize_with = "required_nullable")]
    pub setup_work: Option<Box<ReadWorkSummary>>,
    pub disposition: ProviderReaderDisposition,
}
impl ProviderReaderSummary {
    fn valid_setup(&self) -> bool {
        match self.setup_accounting {
            ProviderSetupAccounting::Unknown
            | ProviderSetupAccounting::IncludedInReaderWork
            | ProviderSetupAccounting::NotApplicable => self.setup_queries.is_none() && self.setup_work.is_none(),
            ProviderSetupAccounting::Separate => true,
        }
    }
    fn complete_setup(&self) -> bool {
        match self.setup_accounting {
            ProviderSetupAccounting::Unknown => false,
            ProviderSetupAccounting::IncludedInReaderWork | ProviderSetupAccounting::NotApplicable => {
                self.valid_setup()
            }
            ProviderSetupAccounting::Separate => {
                self.setup_queries.is_some() && self.setup_work.as_deref().is_some_and(settled_work)
            }
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ProviderReadFailure {
    pub code: ErrorCode,
    #[serde(deserialize_with = "required_nullable")]
    pub stop: Option<CollectionStop>,
}
impl ProviderReadFailure {
    pub fn from_error(error: &Error) -> Self {
        let stop = match error.detail {
            Some(crate::ErrorDetail::CollectionStopped(reason)) => Some(reason),
            _ => None,
        };
        Self { code: error.code, stop }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ProviderFactoryOutcome {
    Pending,
    Constructed,
    Rejected { code: ErrorCode },
    Abandoned,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ProviderFactorySpan {
    pub client_started_at: String,
    #[serde(deserialize_with = "required_nullable")]
    pub client_finished_at: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub client_elapsed_ms: Option<f64>,
    pub outcome: ProviderFactoryOutcome,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ProviderAcquisitionSummary {
    pub observation_id: String,
    pub kind: ProviderReadKind,
    #[serde(deserialize_with = "required_nullable")]
    pub route: Option<SourceAdmissionRoute>,
    #[serde(deserialize_with = "required_nullable")]
    pub engine: Option<Engine>,
    pub client_started_at: String,
    pub client_finished_at: String,
    pub client_elapsed_ms: f64,
    pub native_disposition: ProviderReadDisposition,
    pub coverage: ProviderReadCoverage,
    #[serde(deserialize_with = "required_nullable")]
    pub factory: Option<Box<ProviderFactorySpan>>,
    #[serde(deserialize_with = "required_nullable")]
    pub original_failure: Option<ProviderReadFailure>,
    #[serde(deserialize_with = "required_nullable")]
    pub finalization_failure: Option<ProviderReadFailure>,
    pub readers: Vec<ProviderReaderSummary>,
    pub omitted_readers: u32,
}
impl ProviderAcquisitionSummary {
    pub fn is_valid(&self) -> bool {
        valid_id(&self.observation_id)
            && self.route.as_ref().is_none_or(|route| route.matches_observation(&self.observation_id))
            && valid_client_time(&self.client_started_at)
            && valid_client_time(&self.client_finished_at)
            && valid_elapsed(self.client_elapsed_ms)
            && self.readers.len() <= MAX_PROVIDER_READERS
            && self.readers.iter().enumerate().all(|(index, reader)| {
                reader.reader_id > 0
                    && !self.readers[..index].iter().any(|before| before.reader_id == reader.reader_id)
                    && reader.valid_setup()
                    && reader.work.as_ref().is_none_or(|work| work.is_valid())
                    && reader.setup_work.as_ref().is_none_or(|work| work.is_valid())
            })
            && match &self.coverage {
                ProviderReadCoverage::Complete => {
                    self.has_ordered_client_clocks()
                        && self.omitted_readers == 0
                        && self.readers.iter().all(|reader| {
                            reader.disposition == ProviderReaderDisposition::Quiescent
                                && reader.queries.is_some()
                                && reader.work.as_deref().is_some_and(settled_work)
                                && reader.complete_setup()
                        })
                }
                ProviderReadCoverage::Partial { gaps } => {
                    !gaps.is_empty()
                        && (self.has_ordered_client_clocks() || gaps.contains(&ProviderReadGap::ClientClockChanged))
                        && gaps.iter().enumerate().all(|(index, gap)| !gaps[..index].contains(gap))
                }
                ProviderReadCoverage::Unavailable { .. } => true,
            }
            && self.factory.as_ref().is_none_or(|factory| {
                valid_client_time(&factory.client_started_at)
                    && match factory.outcome {
                        ProviderFactoryOutcome::Pending => {
                            factory.client_finished_at.is_none() && factory.client_elapsed_ms.is_none()
                        }
                        _ => {
                            factory.client_finished_at.as_deref().is_some_and(valid_client_time)
                                && factory.client_elapsed_ms.is_some_and(valid_elapsed)
                        }
                    }
            })
            && crate::serialization::json_size(self, MAX_PROVIDER_ACQUISITION_BYTES).is_ok()
    }
    pub fn has_ordered_client_clocks(&self) -> bool {
        ObservationBounds::new(&self.client_started_at, &self.client_finished_at).is_some()
    }
}
/// A completed existing SourceOperation, carried through the caller's ordinary cancellable Result envelope.
/// Its fields stay opaque so a successful provider value cannot bypass a failed native finalization.
pub struct ProviderCall<T> {
    original: Option<Result<T>>,
    finalization: Result<()>,
    acquisition: Option<Box<ProviderAcquisitionSummary>>,
}
impl<T> ProviderCall<T> {
    pub fn completed(
        original: Result<T>,
        finalization: Result<()>,
        acquisition: Option<ProviderAcquisitionSummary>,
    ) -> Self {
        Self { original: Some(original), finalization, acquisition: acquisition.map(Box::new) }
    }
    /// Native acquisition succeeded, but failed finalization forbids creating or publishing its derived output.
    /// The failure remains finalization-only; no synthetic original error is created.
    pub fn unmaterialized_after_finalization(error: Error, acquisition: Option<ProviderAcquisitionSummary>) -> Self {
        Self { original: None, finalization: Err(error), acquisition: acquisition.map(Box::new) }
    }
    /// Transform only a materialized successful value, retaining an absent output and every failure unchanged.
    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> ProviderCall<U> {
        ProviderCall {
            original: self.original.map(|original| original.map(transform)),
            finalization: self.finalization,
            acquisition: self.acquisition,
        }
    }
    pub fn acquisition(&self) -> Option<&ProviderAcquisitionSummary> {
        self.acquisition.as_deref()
    }
    pub fn into_parts(self) -> (Option<Result<T>>, Result<()>, Option<ProviderAcquisitionSummary>) {
        (self.original, self.finalization, self.acquisition.map(|value| *value))
    }
    pub fn into_result(self) -> Result<T> {
        self.finalization?;
        if self
            .acquisition
            .as_ref()
            .is_some_and(|summary| summary.native_disposition == ProviderReadDisposition::Unconfirmed)
        {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        self.original.ok_or_else(invalid)?
    }
}
/// The original call error and actual gate/finalization error stay independent; neither gains metadata in detail.
pub struct ProviderAttempt<T> {
    original: Option<Result<T>>,
    finalization: Result<()>,
    acquisition: ProviderAcquisitionSummary,
}
impl<T> ProviderAttempt<T> {
    pub fn original(&self) -> Option<&Result<T>> {
        self.original.as_ref()
    }
    pub fn original_failure(&self) -> Option<&Error> {
        self.original.as_ref().and_then(|original| original.as_ref().err())
    }
    pub fn finalization(&self) -> &Result<()> {
        &self.finalization
    }
    pub fn acquisition(&self) -> &ProviderAcquisitionSummary {
        &self.acquisition
    }
    pub fn into_parts(self) -> (Option<Result<T>>, Result<()>, ProviderAcquisitionSummary) {
        (self.original, self.finalization, self.acquisition)
    }
    pub fn into_result(self) -> Result<T> {
        self.finalization?;
        if self.acquisition.native_disposition == ProviderReadDisposition::Unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        self.original.ok_or_else(invalid)?
    }
}
fn settled_work(work: &ReadWorkSummary) -> bool {
    work.active_tasks == 0
        && work.untracked_tasks == 0
        && !work.templates.iter().any(|metric| metric.unconfirmed_cancellations > 0)
        && work
            .connection_work
            .as_ref()
            .is_none_or(|work| work.has_complete_observation_roster() && !work.has_unconfirmed_work())
}
fn valid_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_client_time(value: &str) -> bool {
    ObservationBounds::new(value, value).is_some()
}
fn valid_elapsed(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}
fn invalid() -> Error {
    DeliveryProblem::Integrity.error()
}
#[cfg(test)]
mod tests;
