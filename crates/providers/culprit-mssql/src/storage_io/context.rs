//! Resolve native accounting before counters cross the provider boundary. An Azure edition not recognized here
//! stays unknown; neither a file name nor a missing Hyperscale property establishes conventional storage.
use super::*;
use crate::{deployment::NativeDeployment, read::boolean};
use culprit_core::{FileIoContext, IoAccounting};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeContext {
    deployment: NativeDeployment,
    updateability: Option<String>,
}

impl NativeContext {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        let context =
            Self { deployment: NativeDeployment::decode(row)?, updateability: optional_text(row, "updateability")? };
        Ok(context)
    }

    pub(super) fn hyperscale(&self) -> bool {
        self.deployment.hyperscale()
    }

    pub(super) fn accounting(&self, row: &Rec, kind: FileKind) -> Result<FileIoContext> {
        let cache = boolean(row, "cache")?;
        let temporary = boolean(row, "temporary")?;
        let database_id = integer(row, "database_id")?;
        if cache {
            if !self.hyperscale() || database_id != 0 || temporary || boolean(row, "shared")? {
                return Err(invalid_data());
            }
            // Elastic-pool RBPEX statistics need not belong solely to this database/replica. Keep server scope
            // and no selected-database replica identity; the page count is not a physical I/O call count.
            return Ok(FileIoContext { read: IoAccounting::CachePage, write: IoAccounting::Unknown, replica_id: None });
        }
        if database_id == 0 || integer(row, "file_id")? == 0 {
            return Err(invalid_data());
        }
        if self.hyperscale() && !temporary {
            let (read, write) = match kind {
                FileKind::Data => (IoAccounting::PageServerResponse, IoAccounting::Simulated),
                FileKind::Log => match self.updateability.as_deref() {
                    Some("READ_WRITE") => (IoAccounting::Unknown, IoAccounting::LogServiceWrite),
                    Some("READ_ONLY") => (IoAccounting::ReplicaLogAccounting, IoAccounting::ReplicaLogAccounting),
                    _ => (IoAccounting::Unknown, IoAccounting::Unknown),
                },
                _ => (IoAccounting::Unknown, IoAccounting::Unknown),
            };
            return Ok(FileIoContext { read, write, replica_id: self.deployment.replica_id().map(String::from) });
        }
        let mut context = if self.deployment.conventional() { FileIoContext::STORAGE } else { FileIoContext::UNKNOWN };
        if temporary && kind == FileKind::Log && self.deployment.conventional() {
            context.write = IoAccounting::NonDurableLog;
        }
        Ok(context)
    }
}
