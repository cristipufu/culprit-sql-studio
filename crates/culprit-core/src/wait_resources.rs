//! Native resources a request was observed waiting on. Ownership is separate evidence.
use crate::telemetry::NativeIdentity;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MAX_IDENTIFIED_WAIT_RESOURCES: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitResourceKind {
    MemoryPage,
    PageIo,
    DataLock,
}

/// Native identities are local to their source and observation, not global warehouse resource IDs.
/// This identifies what the request waited on; it does not identify a resource owner or prove a root cause.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export)]
pub struct ObservedWaitResource {
    pub kind: WaitResourceKind,
    pub native: NativeIdentity,
}
impl ObservedWaitResource {
    pub fn is_valid(&self) -> bool {
        self.native.is_valid()
    }
}
