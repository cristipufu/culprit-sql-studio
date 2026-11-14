//! Resolve native accounting before counters cross the provider boundary. An Azure edition not recognized here
//! stays unknown; neither a file name nor a missing Hyperscale property establishes conventional storage.
use super::*;
use crate::read::{boolean, optional_integer};
use culprit_core::{FileIoContext, IoAccounting};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeContext {
    engine_edition: Option<i64>,
    database_edition: Option<String>,
    replica_id: Option<String>,
    updateability: Option<String>,
}

impl NativeContext {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        let context = Self {
            engine_edition: optional_integer(row, "engine_edition")?,
            database_edition: optional_text(row, "database_edition")?,
            replica_id: optional_text(row, "replica_id")?,
            updateability: optional_text(row, "updateability")?,
        };
        // The query converts ReplicaID to uniqueidentifier before serialization. Refuse malformed fixture/driver
        // values as well, so a nonempty arbitrary string cannot authorize the special database_id=0 source.
        if context.replica_id.as_deref().is_some_and(|value| !native_guid(value)) {
            return Err(invalid_data());
        }
        Ok(context)
    }

    pub(super) fn hyperscale(&self) -> bool {
        self.engine_edition == Some(crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE)
            && (self.database_edition.as_deref() == Some("Hyperscale") || self.replica_id.is_some())
    }

    fn conventional(&self) -> bool {
        match self.engine_edition {
            Some(2 | 3 | 4 | crate::probe::ENGINE_EDITION_MANAGED_INSTANCE) => true,
            Some(crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE) if !self.hyperscale() => matches!(
                self.database_edition.as_deref(),
                Some("Basic" | "Standard" | "Premium" | "General Purpose" | "Business Critical")
            ),
            _ => false,
        }
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
            return Ok(FileIoContext { read, write, replica_id: self.replica_id.clone() });
        }
        let mut context = if self.conventional() { FileIoContext::STORAGE } else { FileIoContext::UNKNOWN };
        if temporary && kind == FileKind::Log && self.conventional() {
            context.write = IoAccounting::NonDurableLog;
        }
        Ok(context)
    }
}

fn native_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}
