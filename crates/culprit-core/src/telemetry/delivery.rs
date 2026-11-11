//! Delivery failures are typed; callers never branch on translated messages.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DeliveryProblem {
    InvalidContract,
    DestinationChanged,
    Fenced,
    Conflict,
    Integrity,
    Capacity,
    Storage,
    Transport,
    NotReady,
    UnsupportedSchema,
}
impl DeliveryProblem {
    pub fn error(self) -> crate::Error {
        let message = match self {
            Self::InvalidContract => t!("delivery.invalid_contract"),
            Self::DestinationChanged => t!("delivery.destination_changed"),
            Self::Fenced => t!("delivery.fenced"),
            Self::Conflict => t!("delivery.conflict"),
            Self::Integrity => t!("delivery.integrity"),
            Self::Capacity => t!("delivery.capacity"),
            Self::Storage => t!("delivery.storage"),
            Self::Transport => t!("delivery.transport"),
            Self::NotReady => t!("delivery.not_ready"),
            Self::UnsupportedSchema => t!("delivery.unsupported_schema"),
        };
        crate::Error::new(crate::ErrorCode::QueryFailed, message).with_detail(crate::ErrorDetail::Delivery(self))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DeliveryPhase {
    Captured,
    Committed,
    DispatchPending,
    AwaitingData,
    PublicationPending,
    AwaitingPublication,
    Published,
    Retired,
    Quarantined,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct DeliveryEntry {
    pub range_id: String,
    pub phase: DeliveryPhase,
    pub attempt: u32,
    pub retry_at_ms: String,
    pub problem: Option<DeliveryProblem>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct DeliveryBacklog {
    pub pending: u32,
    pub quarantined: u32,
    /// Collector time, not native measurement time or destination arrival time.
    #[ts(type = "number | null")]
    pub oldest_pending_collection_ms: Option<i64>,
}
