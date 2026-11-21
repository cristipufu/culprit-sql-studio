//! One bounded native column roster, separate from native NULL values and source read permissions.
use super::*;
use crate::read::{integer, optional_boolean, optional_integer, optional_text};
use culprit_core::telemetry::MssqlCatalogAvailability as Availability;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Start,
    End,
    Cpu,
    Cores,
    Reserved,
    Used,
    Requests,
    Read,
    Written,
    ResourceType,
    ResourceName,
    Sku,
    Hardware,
}
impl Field {
    pub(super) const ALL: [Self; 13] = [
        Self::Start,
        Self::End,
        Self::Cpu,
        Self::Cores,
        Self::Reserved,
        Self::Used,
        Self::Requests,
        Self::Read,
        Self::Written,
        Self::ResourceType,
        Self::ResourceName,
        Self::Sku,
        Self::Hardware,
    ];
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Start => "start_time",
            Self::End => "end_time",
            Self::Cpu => "avg_cpu_percent",
            Self::Cores => "virtual_core_count",
            Self::Reserved => "reserved_storage_mb",
            Self::Used => "storage_space_used_mb",
            Self::Requests => "io_request",
            Self::Read => "io_bytes_read",
            Self::Written => "io_bytes_written",
            Self::ResourceType => "resource_type",
            Self::ResourceName => "resource_name",
            Self::Sku => "sku",
            Self::Hardware => "hardware_generation",
        }
    }
    pub(super) fn supported(self, row: &Rec) -> Result<bool> {
        let native = integer(row, "native_type")?;
        Ok(match self {
            Self::Start => native == 42 && integer(row, "native_scale")? <= 7,
            Self::End => native == 61,
            Self::Cpu => {
                matches!(native, 106 | 108)
                    && integer(row, "native_precision")? == 5
                    && integer(row, "native_scale")? == 2
            }
            Self::Used => {
                matches!(native, 106 | 108)
                    && integer(row, "native_precision")? == 18
                    && integer(row, "native_scale")? == 2
            }
            Self::Cores => native == 56,
            Self::Reserved | Self::Requests | Self::Read | Self::Written => native == 127,
            Self::ResourceType | Self::ResourceName | Self::Sku | Self::Hardware => {
                native == 231 && integer(row, "native_length")? == 256
            }
        })
    }
}

pub(crate) struct Layout {
    pub(crate) native: MssqlManagedInstanceResourceLayout,
    pub source_permission: Option<bool>,
    pub observer_permission: Option<bool>,
}
impl Layout {
    pub(crate) fn unsupported() -> Self {
        let status = Availability::Unsupported;
        Self {
            source_permission: None,
            observer_permission: None,
            native: MssqlManagedInstanceResourceLayout {
                interval: status,
                cpu: status,
                virtual_core_count: status,
                reserved_storage_mb: status,
                storage_space_used_mb: status,
                io_request: status,
                io_bytes_read: status,
                io_bytes_written: status,
                resource_type: status,
                resource_name: status,
                sku: status,
                hardware_generation: status,
            },
        }
    }
    pub(super) fn decode(rows: &[Rec], major: u32) -> Result<Self> {
        if rows.is_empty() || rows.len() > Field::ALL.len() {
            return Err(invalid_data());
        }
        let first = &rows[0];
        let source_permission = optional_boolean(first, "server_state")?;
        let observer_permission =
            if major >= 16 { optional_boolean(first, "server_performance_state")? } else { source_permission };
        let exposed = optional_integer(first, "native_object_id")?;
        let missing = if source_permission == Some(false) {
            Availability::PermissionDenied
        } else if source_permission.is_none() || exposed.is_none() {
            Availability::Unknown
        } else {
            Availability::Unsupported
        };
        let mut columns = BTreeMap::new();
        for row in rows {
            if optional_boolean(row, "server_state")? != source_permission
                || optional_integer(row, "native_object_id")? != exposed
                || (major >= 16 && optional_boolean(row, "server_performance_state")? != observer_permission)
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            let Some(name) = optional_text(row, "native_column")? else {
                if rows.len() != 1 {
                    return Err(invalid_data());
                };
                continue;
            };
            let field = Field::ALL.into_iter().find(|field| field.name() == name).ok_or_else(invalid_data)?;
            if columns.insert(name, field.supported(row)?).is_some() {
                return Err(invalid_data());
            }
        }
        let status = |field: Field| {
            if source_permission != Some(true) {
                missing
            } else {
                match columns.get(field.name()) {
                    Some(true) => Availability::Available,
                    Some(false) => Availability::Unsupported,
                    None => missing,
                }
            }
        };
        let interval = [Field::Start, Field::End]
            .into_iter()
            .map(status)
            .find(|status| *status != Availability::Available)
            .unwrap_or(Availability::Available);
        Ok(Self {
            source_permission,
            observer_permission,
            native: MssqlManagedInstanceResourceLayout {
                interval,
                cpu: status(Field::Cpu),
                virtual_core_count: status(Field::Cores),
                reserved_storage_mb: status(Field::Reserved),
                storage_space_used_mb: status(Field::Used),
                io_request: status(Field::Requests),
                io_bytes_read: status(Field::Read),
                io_bytes_written: status(Field::Written),
                resource_type: status(Field::ResourceType),
                resource_name: status(Field::ResourceName),
                sku: status(Field::Sku),
                hardware_generation: status(Field::Hardware),
            },
        })
    }
    pub(crate) fn available(&self) -> bool {
        self.native.interval == Availability::Available
            && self.source_permission == Some(true)
            && self.observer_permission == Some(true)
    }
    pub(crate) fn unavailable_code(&self) -> ErrorCode {
        if self.source_permission == Some(false) || self.observer_permission == Some(false) {
            ErrorCode::PermissionDenied
        } else if self.native.interval == Availability::Unsupported {
            ErrorCode::Unsupported
        } else {
            ErrorCode::QueryFailed
        }
    }
    pub(super) fn status(&self, field: Field) -> Availability {
        match field {
            Field::Start | Field::End => self.native.interval,
            Field::Cpu => self.native.cpu,
            Field::Cores => self.native.virtual_core_count,
            Field::Reserved => self.native.reserved_storage_mb,
            Field::Used => self.native.storage_space_used_mb,
            Field::Requests => self.native.io_request,
            Field::Read => self.native.io_bytes_read,
            Field::Written => self.native.io_bytes_written,
            Field::ResourceType => self.native.resource_type,
            Field::ResourceName => self.native.resource_name,
            Field::Sku => self.native.sku,
            Field::Hardware => self.native.hardware_generation,
        }
    }
}
