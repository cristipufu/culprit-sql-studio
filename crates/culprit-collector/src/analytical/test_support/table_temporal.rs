//! One original table-catalog corpus reused by SDK, local analysis and both offline stores.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableTemporalCase {
    Ordinary,
    History,
    SystemVersioned,
    Infinite,
    NativeNulls,
    MixedAvailability,
    Unsupported,
    Partial,
    SourceDenied,
}
impl TableTemporalCase {
    pub const ALL: [Self; 9] = [
        Self::Ordinary,
        Self::History,
        Self::SystemVersioned,
        Self::Infinite,
        Self::NativeNulls,
        Self::MixedAvailability,
        Self::Unsupported,
        Self::Partial,
        Self::SourceDenied,
    ];
}
pub fn table_temporal_evidence(case: TableTemporalCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, mut records) = table_full_text_evidence(match case {
        TableTemporalCase::Partial => TableFullTextCase::Partial,
        TableTemporalCase::SourceDenied => TableFullTextCase::SourceDenied,
        _ => TableFullTextCase::Absent,
    });
    for record in &mut records {
        let TelemetryRecord::Operational(row) = record else { unreachable!() };
        let OperationalFact::MssqlTable(table) = &mut row.fact else { unreachable!() };
        table.temporal_configuration = configuration(case);
        assert!(table.is_valid());
    }
    (source, records)
}
fn configuration(case: TableTemporalCase) -> MssqlTableTemporalConfiguration {
    let (kind, description) = match case {
        TableTemporalCase::History => (Some(1), Some("HISTORY_TABLE")),
        TableTemporalCase::SystemVersioned
        | TableTemporalCase::Infinite
        | TableTemporalCase::Partial
        | TableTemporalCase::MixedAvailability => (Some(2), Some("SYSTEM_VERSIONED_TEMPORAL_TABLE")),
        TableTemporalCase::NativeNulls | TableTemporalCase::Unsupported | TableTemporalCase::SourceDenied => {
            (None, None)
        }
        _ => (Some(0), Some("NON_TEMPORAL_TABLE")),
    };
    let available = if case == TableTemporalCase::Unsupported {
        MssqlCatalogAvailability::Unsupported
    } else {
        MssqlCatalogAvailability::Available
    };
    let history =
        matches!(case, TableTemporalCase::SystemVersioned | TableTemporalCase::Infinite | TableTemporalCase::Partial)
            .then_some(102);
    let (period, unit, unit_desc) = if history.is_some() {
        if case == TableTemporalCase::Infinite {
            (Some(-1), Some(-1), Some("INFINITE"))
        } else {
            (Some(6), Some(5), Some("MONTH"))
        }
    } else {
        (None, None, None)
    };
    let mut value = MssqlTableTemporalConfiguration {
        temporal_type_availability: available,
        temporal_type: kind,
        temporal_type_desc_availability: available,
        temporal_type_desc: description.map(str::to_owned),
        history_table_id_availability: available,
        history_table_id: history,
        history_retention_period_availability: available,
        history_retention_period: period,
        history_retention_period_unit_availability: available,
        history_retention_period_unit: unit,
        history_retention_period_unit_desc_availability: available,
        history_retention_period_unit_desc: unit_desc.map(str::to_owned),
    };
    if case == TableTemporalCase::MixedAvailability {
        value.temporal_type_desc_availability = MssqlCatalogAvailability::PermissionDenied;
        value.temporal_type_desc = None;
        value.history_table_id_availability = MssqlCatalogAvailability::Unsupported;
        value.history_retention_period = Some(6);
        value.history_retention_period_unit_availability = MssqlCatalogAvailability::Unknown;
    }
    value
}
