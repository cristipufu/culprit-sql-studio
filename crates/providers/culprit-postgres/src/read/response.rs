//! Closed response modes of the same native read owner. Values never become interpolated SQL.
use super::*;
use tokio_postgres::{
    SimpleQueryRow,
    types::{FromSql, Type},
};

pub(super) enum ReadRows {
    Text(Vec<Row>),
    Native(Vec<tokio_postgres::Row>),
}
pub(super) enum ReadRow {
    Text(SimpleQueryRow),
    Native(tokio_postgres::Row),
}
pub(super) enum ReadOutput {
    Text(RowOutput<Row>),
    Native(RowOutput<tokio_postgres::Row>),
}
impl ReadOutput {
    pub(super) fn retention_control(&self) -> culprit_read::RetentionControl {
        match self {
            Self::Text(output) => output.retention_control(),
            Self::Native(output) => output.retention_control(),
        }
    }
    pub(super) fn discard(&mut self) {
        match self {
            Self::Text(output) => output.discard(),
            Self::Native(output) => output.discard(),
        }
    }
    pub(super) fn discarding(&self) -> bool {
        match self {
            Self::Text(output) => output.discarding(),
            Self::Native(output) => output.discarding(),
        }
    }
    pub(super) async fn push(&mut self, row: ReadRow, bytes: usize) -> Result<()> {
        match (self, row) {
            (Self::Text(output), ReadRow::Text(row)) => output.push(Row::from_native(row), bytes).await,
            (Self::Native(output), ReadRow::Native(row)) => output.push(row, bytes).await,
            _ => Err(CollectionStop::InvalidPage.error()),
        }
    }
    pub(super) async fn finish(self) -> ReadRows {
        match self {
            Self::Text(output) => ReadRows::Text(output.finish().await),
            Self::Native(output) => ReadRows::Native(output.finish().await),
        }
    }
}
impl ReadRow {
    pub(super) fn value_bytes(&self) -> Result<usize> {
        match self {
            Self::Text(row) => (0..row.len()).try_fold(0usize, |sum, index| {
                sum.checked_add(row.get(index).map_or(0, str::len)).ok_or_else(|| CollectionStop::ByteLimit.error())
            }),
            Self::Native(row) => (0..row.len()).try_fold(0usize, |sum, index| {
                let bytes = row.try_get::<_, Option<NativeBytes<'_>>>(index).map_err(crate::connect::map_error)?;
                sum.checked_add(bytes.map_or(0, |value| value.0.len())).ok_or_else(|| CollectionStop::ByteLimit.error())
            }),
        }
    }
}
// Only payload length is inspected here. The original native Row and its existing typed decoder are unchanged.
struct NativeBytes<'a>(&'a [u8]);
impl<'a> FromSql<'a> for NativeBytes<'a> {
    fn from_sql(_kind: &Type, bytes: &'a [u8]) -> std::result::Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(bytes))
    }
    fn accepts(_kind: &Type) -> bool {
        true
    }
}
