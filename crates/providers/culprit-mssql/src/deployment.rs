//! One source-native deployment interpretation for resource percentages and virtual-file accounting.
//! Edition is a native DATABASEPROPERTYEX value, never a configured tier or localized server label.
use crate::{
    read::{optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::DataIoAccounting};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeDeployment {
    engine_edition: Option<i64>,
    database_edition: Option<String>,
    replica_id: Option<String>,
}
impl NativeDeployment {
    pub(crate) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            engine_edition: optional_integer(row, "engine_edition")?,
            database_edition: optional_text(row, "database_edition")?,
            replica_id: crate::replica::id(row, "replica_id")?,
        })
    }

    pub(crate) fn hyperscale(&self) -> bool {
        // ReplicaID is documented as Hyperscale-only; NULL can also mean unavailable/not started. Neither NULL
        // nor an unknown Edition proves conventional storage. https://learn.microsoft.com/sql/t-sql/functions/databasepropertyex-transact-sql
        self.engine_edition == Some(crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE)
            && (self.database_edition.as_deref() == Some("Hyperscale") || self.replica_id.is_some())
    }

    pub(crate) fn conventional(&self) -> bool {
        match self.engine_edition {
            Some(2 | 3 | 4 | crate::probe::ENGINE_EDITION_MANAGED_INSTANCE) => true,
            Some(crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE) if !self.hyperscale() => matches!(
                self.database_edition.as_deref(),
                Some("Basic" | "Standard" | "Premium" | "General Purpose" | "Business Critical")
            ),
            _ => false,
        }
    }

    pub(crate) fn data_io_accounting(&self) -> DataIoAccounting {
        if self.hyperscale() {
            DataIoAccounting::ComputeLocalSsd
        } else if self.conventional() {
            DataIoAccounting::DataFiles
        } else {
            DataIoAccounting::Unknown
        }
    }

    pub(crate) fn replica_id(&self) -> Option<&str> {
        self.replica_id.as_deref()
    }
}
