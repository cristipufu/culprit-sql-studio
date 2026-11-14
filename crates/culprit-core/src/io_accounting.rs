//! Native I/O counters can count storage operations, cache pages or bookkeeping. Their meaning is part of the
//! observation; a consumer cannot recover it from an engine name, file label or a nonzero timing counter.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum IoAccounting {
    /// Operations at the native storage interface. A measured delay does not establish a hardware fault.
    StorageOperation,
    /// Cache pages serviced; one storage request can account for multiple pages.
    CachePage,
    /// A remote page response, which can include page replay/catch-up and transfer time.
    PageServerResponse,
    /// Durable log-service writes on a native writer, rather than writes to a local log device.
    LogServiceWrite,
    /// Native bookkeeping does not describe an actual storage operation.
    Simulated,
    /// Replica accounting does not measure transaction-log hardening on that replica.
    ReplicaLogAccounting,
    /// Temporary log work that does not provide transaction commit durability.
    NonDurableLog,
    /// Counters exist, but their operation meaning has not been established.
    Unknown,
}

impl IoAccounting {
    pub const fn key(self) -> &'static str {
        match self {
            Self::StorageOperation => "storage-operation",
            Self::CachePage => "cache-page",
            Self::PageServerResponse => "page-server-response",
            Self::LogServiceWrite => "log-service-write",
            Self::Simulated => "simulated",
            Self::ReplicaLogAccounting => "replica-log-accounting",
            Self::NonDurableLog => "non-durable-log",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct FileIoContext {
    pub read: IoAccounting,
    pub write: IoAccounting,
    /// A native replica identifier local to this source; neither a globally unique ID nor a counter reset epoch.
    pub replica_id: Option<String>,
}

impl FileIoContext {
    pub const STORAGE: Self =
        Self { read: IoAccounting::StorageOperation, write: IoAccounting::StorageOperation, replica_id: None };
    pub const UNKNOWN: Self = Self { read: IoAccounting::Unknown, write: IoAccounting::Unknown, replica_id: None };

    pub fn is_valid(&self) -> bool {
        matches!(
            self.read,
            IoAccounting::StorageOperation
                | IoAccounting::CachePage
                | IoAccounting::PageServerResponse
                | IoAccounting::ReplicaLogAccounting
                | IoAccounting::Unknown
        ) && matches!(
            self.write,
            IoAccounting::StorageOperation
                | IoAccounting::CachePage
                | IoAccounting::LogServiceWrite
                | IoAccounting::Simulated
                | IoAccounting::ReplicaLogAccounting
                | IoAccounting::NonDurableLog
                | IoAccounting::Unknown
        ) && self
            .replica_id
            .as_ref()
            .is_none_or(|id| !id.is_empty() && id.len() <= MAX_REPLICA_ID_BYTES && !id.chars().any(char::is_control))
    }
}

const MAX_REPLICA_ID_BYTES: usize = 256;

#[cfg(test)]
mod tests;
