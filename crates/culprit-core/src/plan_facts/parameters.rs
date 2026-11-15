//! Parameter context belongs to one selected native plan. Literal values and value hashes are never retained.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

pub const MAX_PLAN_PARAMETERS: usize = 128;
pub const MAX_PARAMETER_NAME_UTF16: usize = 128;
pub const MAX_PARAMETER_TYPE_BYTES: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanParameterValueState {
    /// The native attribute was observed, not that its literal is retained or describes every execution.
    Recorded,
    NotRecorded,
    Redacted,
    /// An attribute could not be qualified, including runtime attributes on compilation-only evidence.
    Unqualified,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanParameterComparison {
    /// Exact equality of the original, decoded native attribute text, not SQL value equivalence.
    SameRepresentation,
    DifferentRepresentation,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanParameterIssue {
    DuplicateName,
    InvalidMetadata,
    ParameterLimit,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanParameter {
    pub name: Option<String>,
    pub data_type: Option<String>,
    pub compiled: PlanParameterValueState,
    pub runtime: PlanParameterValueState,
    pub comparison: PlanParameterComparison,
}

impl PlanParameter {
    pub fn is_valid(&self) -> bool {
        self.name.as_deref().is_none_or(parameter_name_is_valid)
            && self.data_type.as_deref().is_none_or(parameter_type_is_valid)
            && (self.comparison == PlanParameterComparison::Unavailable
                || self.name.is_some()
                    && self.compiled == PlanParameterValueState::Recorded
                    && self.runtime == PlanParameterValueState::Recorded)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanParameterContext {
    pub parameters: Vec<PlanParameter>,
    pub issues: Vec<PlanParameterIssue>,
}

impl PlanParameterContext {
    pub fn is_valid(&self) -> bool {
        let issues: BTreeSet<_> = self.issues.iter().collect();
        if self.parameters.len() > MAX_PLAN_PARAMETERS
            || issues.len() != self.issues.len()
            || !self.parameters.iter().all(PlanParameter::is_valid)
            || self.issues.contains(&PlanParameterIssue::ParameterLimit)
                && (self.parameters.len() != MAX_PLAN_PARAMETERS
                    || self
                        .parameters
                        .iter()
                        .any(|parameter| parameter.comparison != PlanParameterComparison::Unavailable))
        {
            return false;
        }
        self.parameters.iter().all(|parameter| {
            let duplicate = parameter.name.as_ref().is_some_and(|name| {
                self.parameters.iter().filter(|other| other.name.as_ref() == Some(name)).count() > 1
            });
            !duplicate
                || self.issues.contains(&PlanParameterIssue::DuplicateName)
                    && parameter.comparison == PlanParameterComparison::Unavailable
        })
    }
}

pub fn parameter_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.encode_utf16().count() <= MAX_PARAMETER_NAME_UTF16
        && name.chars().all(|character| character.is_alphanumeric() || matches!(character, '_' | '#' | '$' | '@'))
}

pub fn parameter_type_is_valid(data_type: &str) -> bool {
    !data_type.trim().is_empty()
        && data_type.len() <= MAX_PARAMETER_TYPE_BYTES
        && !data_type.chars().any(char::is_control)
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PlanParameterContext>, D::Error> {
    Ok(Option::<PlanParameterContext>::deserialize(deserializer)?.filter(PlanParameterContext::is_valid))
}

#[cfg(test)]
mod tests;
