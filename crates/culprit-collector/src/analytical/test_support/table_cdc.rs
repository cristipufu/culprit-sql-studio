//! Current table enrollment remains separate from processing progress and retained changes.
use super::*;

#[derive(Clone, Copy, Debug)]
pub enum TableCdcCase {
    Tracked,
    Untracked,
    NativeNull,
    Partial,
    SourceDenied,
}
impl TableCdcCase {
    pub const ALL: [Self; 5] = [Self::Tracked, Self::Untracked, Self::NativeNull, Self::Partial, Self::SourceDenied];
}

pub fn table_cdc_evidence(case: TableCdcCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source_case = match case {
        TableCdcCase::Partial => TableFullTextCase::Partial,
        TableCdcCase::SourceDenied => TableFullTextCase::SourceDenied,
        _ => TableFullTextCase::Absent,
    };
    let (source, mut records) = table_full_text_evidence(source_case);
    for record in &mut records {
        let TelemetryRecord::Operational(row) = record else { unreachable!() };
        let OperationalFact::MssqlTable(table) = &mut row.fact else { unreachable!() };
        table.tracked_by_cdc = match case {
            TableCdcCase::Tracked | TableCdcCase::Partial => Some(true),
            TableCdcCase::Untracked => Some(false),
            TableCdcCase::NativeNull | TableCdcCase::SourceDenied => None,
        };
        assert!(table.is_valid());
    }
    (source, records)
}
