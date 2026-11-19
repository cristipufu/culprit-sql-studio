//! One qualified native file witness. A primary tempdb map can inherit its verified process/database lifetime;
//! this does not manufacture a file GUID, a page generation, an allocating statement or a latch owner.
use super::{
    context::LookupContext,
    rows::{Catalog, HeaderRecord, PageAddress},
};
use crate::{
    read::{optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ObservedFile {
    guid: Option<String>,
    created_lsn: Option<String>,
    kind: Option<i64>,
    state: Option<i64>,
}
impl ObservedFile {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            guid: optional_text(row, "file_guid")?,
            created_lsn: optional_text(row, "file_create_lsn")?,
            kind: optional_integer(row, "file_type")?,
            state: optional_integer(row, "file_state")?,
        })
    }
    fn from_catalog(catalog: &Catalog) -> Self {
        Self {
            guid: catalog.file_guid.clone(),
            created_lsn: catalog.file_create_lsn.clone(),
            kind: catalog.file_type.map(i64::from),
            state: catalog.file_state.map(i64::from),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileEpoch {
    address: PageAddress,
    observed: ObservedFile,
    basis: PageTargetIdentityBasis,
    identity: NativeIdentity,
    lifetime: Option<(NativeIdentity, u64)>,
}
impl FileEpoch {
    pub(super) fn select(context: &LookupContext, record: &HeaderRecord) -> Option<Self> {
        if !context.accepts(record.address)
            || context.unavailable_for(record.address).is_some()
            || !record.address.matches(&record.header).ok()?
        {
            return None;
        }
        let observed = ObservedFile::from_catalog(&record.catalog);
        if observed.kind != Some(0) || observed.state != Some(0) {
            return None;
        }
        let (basis, identity, lifetime) = if let Some(guid) = &observed.guid {
            let (basis, _) = context.database_identity(record.address).ok()?;
            (basis, NativeIdentity::new(guid).qualified("kind", "sql-server-database-file"), None)
        } else {
            let shared = context.shared_for(record.address)?;
            if !context.supports_primary_tempdb_file()
                || observed.created_lsn.is_some()
                || record.address.database != 2
                || record.address.file != 1
                || !matches!(
                    classify_sql_server_page_header(&super::header_facts(record).ok()?),
                    PageWaitTargetKind::Pfs | PageWaitTargetKind::Gam | PageWaitTargetKind::Sgam
                )
                || record.catalog.object_id.is_some()
                || record.header.object_id.is_some()
                || !shared.read_context().ok()?.has_qualified_local_owner()
            {
                return None;
            }
            (
                PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch,
                NativeIdentity::new("1")
                    .qualified("kind", MSSQL_TEMPDB_PRIMARY_FILE_KIND)
                    .qualified("file_type", "0")
                    .qualified("file_state", "0"),
                Some((shared.database_identity().ok()?, context.authority.connection_generation()?)),
            )
        };
        let identity = match &observed.created_lsn {
            Some(lsn) => identity.qualified("create_lsn", lsn),
            None => identity,
        };
        identity.is_valid().then_some(Self { address: record.address, observed, basis, identity, lifetime })
    }
    pub(super) fn basis(&self) -> PageTargetIdentityBasis {
        self.basis
    }
    pub(super) fn identity(&self) -> NativeIdentity {
        self.identity.clone()
    }
    pub(super) fn file_id(&self) -> u32 {
        self.address.file
    }
    pub(super) fn observed_file(&self) -> ObservedFile {
        self.observed.clone()
    }
}

#[cfg(test)]
mod tests;
