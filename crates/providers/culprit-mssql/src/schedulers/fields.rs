//! Scheduler descriptor delegates the same native layout/decoder used by other closed SQLOS populations.
use super::*;
pub(super) struct Columns(crate::native_counter_fields::Columns);
impl Columns {
    pub(super) async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        Ok(Self(crate::native_counter_fields::Columns::read(reader, NATIVE_OBJECT, MSSQL_SCHEDULER_FIELDS).await?))
    }
    pub(super) fn projection(&self) -> String {
        self.0.projection()
    }
    pub(super) fn decode(&self, row: &crate::value::Rec) -> Result<NativeCounterSet> {
        self.0.decode(row)
    }
}
#[cfg(test)]
pub(crate) fn all_columns() -> Vec<crate::value::Rec> {
    crate::native_counter_fields::all_columns(MSSQL_SCHEDULER_FIELDS)
}
