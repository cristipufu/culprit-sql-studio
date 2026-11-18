//! An opt-in fixture inspection keeps only one original successful result, independent of source ownership.
use super::*;
const MAX_RESULT_ROWS: usize = 32;
const MAX_RESULT_VALUE_BYTES: usize = 64 * 1024;

pub(super) struct ResultTrace {
    template_id: String,
    result: Option<culprit_core::Result<Vec<Rec>>>,
    consumed: bool,
}
impl ResultTrace {
    pub(super) fn new(template_id: &str) -> Self {
        Self { template_id: template_id.into(), result: None, consumed: false }
    }
    pub(super) fn retain(&mut self, sql: &str, rows: &[Rec]) {
        if self.result.is_some() || self.consumed || digest(sql.as_bytes()) != self.template_id {
            return;
        }
        self.result = Some(if rows.len() > MAX_RESULT_ROWS {
            Err(CollectionStop::RowLimit.error())
        } else if rows
            .iter()
            .try_fold(0usize, |bytes, row| bytes.checked_add(value_bytes(row)))
            .is_none_or(|bytes| bytes > MAX_RESULT_VALUE_BYTES)
        {
            Err(CollectionStop::ByteLimit.error())
        } else {
            Ok(rows.to_vec())
        });
    }
    pub(super) fn take(&mut self) -> culprit_core::Result<Option<Vec<Rec>>> {
        let result = self.result.take().transpose();
        self.consumed = true;
        result
    }
}

#[test]
fn a_result_trace_cannot_substitute_a_later_or_different_template_body() {
    let sql = "SELECT /* culprit */ original_metadata";
    let mut trace = ResultTrace::new(&digest(sql.as_bytes()));
    trace.retain("SELECT /* culprit */ other_metadata", &[Rec::of(&[("value", Val::Int(3))])]);
    assert!(trace.result.is_none());
    trace.retain(sql, &[Rec::of(&[("value", Val::Int(1))])]);
    trace.retain(sql, &[Rec::of(&[("value", Val::Int(2))])]);
    let rows = trace.take().unwrap().unwrap();
    assert_eq!(integer(&rows[0], "value").unwrap(), 1);
    trace.retain(sql, &[Rec::of(&[("value", Val::Int(4))])]);
    assert!(trace.take().unwrap().is_none());
}

#[test]
fn inspection_limits_return_a_gap_without_a_truncated_successful_trace() {
    let sql = "SELECT /* culprit */ native_metadata";
    for rows in [
        vec![Rec::of(&[("value", Val::Int(1))]); MAX_RESULT_ROWS + 1],
        vec![Rec::of(&[("value", Val::Text("x".repeat(MAX_RESULT_VALUE_BYTES + 1)))])],
    ] {
        let mut trace = ResultTrace::new(&digest(sql.as_bytes()));
        trace.retain(sql, &rows);
        assert!(trace.take().is_err());
        assert!(trace.take().unwrap().is_none());
    }
}

#[test]
fn an_empty_original_result_is_distinct_from_no_completed_result() {
    let sql = "SELECT /* culprit */ native_metadata";
    let mut missing = ResultTrace::new(&digest(sql.as_bytes()));
    assert!(missing.take().unwrap().is_none());
    let mut empty = ResultTrace::new(&digest(sql.as_bytes()));
    empty.retain(sql, &[]);
    assert!(empty.take().unwrap().unwrap().is_empty());
}
