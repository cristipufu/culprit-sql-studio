//! Inspectable local collection ownership, independent of the selected source and destination.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CollectionAdmissionStatus {
    pub observation_id: Option<String>,
    pub connection: Option<String>,
    pub source_quiescence_required: bool,
}
