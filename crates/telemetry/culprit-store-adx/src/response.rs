//! Bounded, non-progressive Query V2 replies. HTTP 200 and primary rows alone never prove completion.
//! https://learn.microsoft.com/kusto/api/rest/response-v2?view=azure-data-explorer
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAMES: usize = 16;
const MAX_COLUMNS: usize = 128;
const MAX_AUXILIARY_ROWS: usize = 512;
const MAX_QUERY_ROWS: usize = 10_001;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Column {
    pub column_name: String,
    pub column_type: String,
}

pub struct QueryTable {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(tag = "FrameType")]
enum Frame {
    #[serde(rename = "DataSetHeader", rename_all = "PascalCase")]
    Header { version: String, is_progressive: bool },
    #[serde(rename = "DataTable", rename_all = "PascalCase")]
    Table { table_id: u64, table_kind: String, columns: Vec<Column>, rows: Vec<Vec<Value>> },
    #[serde(rename = "DataSetCompletion", rename_all = "PascalCase")]
    Completion { has_errors: bool, cancelled: bool, one_api_errors: Option<Vec<Value>> },
}

pub fn parse(bytes: &[u8], expected: &[Column], row_limit: usize) -> Result<QueryTable> {
    if bytes.len() > MAX_RESPONSE_BYTES
        || row_limit == 0
        || row_limit > MAX_QUERY_ROWS
        || expected.is_empty()
        || expected.len() > MAX_COLUMNS
    {
        return Err(DeliveryProblem::Capacity.error());
    }
    let frames: Vec<Frame> = serde_json::from_slice(bytes).map_err(|_| DeliveryProblem::Transport.error())?;
    if frames.len() < 3 || frames.len() > MAX_FRAMES {
        return Err(DeliveryProblem::Transport.error());
    }
    validate_boundaries(&frames)?;
    let mut ids = BTreeSet::new();
    let mut primary = None;
    let mut auxiliary_rows = 0;
    let frame_count = frames.len();
    for frame in frames.into_iter().skip(1).take(frame_count - 2) {
        let Frame::Table { table_id, table_kind, columns, rows } = frame else {
            return Err(DeliveryProblem::Integrity.error());
        };
        if !ids.insert(table_id) {
            return Err(DeliveryProblem::Integrity.error());
        }
        validate_table(&columns, &rows)?;
        if table_kind == "PrimaryResult" {
            if primary.is_some() {
                return Err(DeliveryProblem::Integrity.error());
            }
            if columns != expected {
                return Err(DeliveryProblem::UnsupportedSchema.error());
            }
            if rows.len() > row_limit {
                return Err(DeliveryProblem::Capacity.error());
            }
            primary = Some(QueryTable { columns, rows });
        } else {
            auxiliary_rows += rows.len();
            if auxiliary_rows > MAX_AUXILIARY_ROWS {
                return Err(DeliveryProblem::Capacity.error());
            }
        }
    }
    primary.ok_or_else(|| DeliveryProblem::Integrity.error())
}
fn validate_boundaries(frames: &[Frame]) -> Result<()> {
    match &frames[0] {
        Frame::Header { version, is_progressive: false } if version == "v2.0" => (),
        Frame::Header { .. } => return Err(DeliveryProblem::UnsupportedSchema.error()),
        _ => return Err(DeliveryProblem::Integrity.error()),
    }
    match frames.last() {
        Some(Frame::Completion { has_errors: false, cancelled: false, one_api_errors })
            if one_api_errors.as_ref().is_none_or(Vec::is_empty) =>
        {
            Ok(())
        }
        _ => Err(DeliveryProblem::Transport.error()),
    }
}
fn validate_table(columns: &[Column], rows: &[Vec<Value>]) -> Result<()> {
    if columns.is_empty() || columns.len() > MAX_COLUMNS {
        return Err(DeliveryProblem::Capacity.error());
    }
    let mut names = BTreeSet::new();
    if columns.iter().any(|column| {
        column.column_name.is_empty()
            || column.column_name.len() > 256
            || column.column_type.is_empty()
            || column.column_type.len() > 32
            || !names.insert(&column.column_name)
    }) || rows.iter().any(|row| row.len() != columns.len())
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
