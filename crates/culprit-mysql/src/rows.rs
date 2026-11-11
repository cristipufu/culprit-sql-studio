use culprit_core::Result;
use mysql_async::{Row, Value};

use crate::connect::TAG;

/// One result row with case-insensitive access by column name, tolerant of whatever protocol and type the server used.
pub struct Rec {
    columns: Vec<String>,
    values: Vec<Value>,
}

impl Rec {
    pub(crate) fn from_row(row: Row) -> Rec {
        let columns = row.columns_ref().iter().map(|c| c.name_str().to_ascii_lowercase()).collect();
        Rec { columns, values: row.unwrap() }
    }

    pub(crate) fn from_native(row: Row) -> Result<Rec> {
        let row = Self::from_row(row);
        let names: std::collections::BTreeSet<_> = row.columns.iter().collect();
        if names.len() != row.columns.len() || row.columns.len() != row.values.len() {
            return Err(invalid_native_row());
        }
        Ok(row)
    }

    fn native_value(&self, name: &str) -> Result<Option<&Value>> {
        let name = name.to_ascii_lowercase();
        let index = self.columns.iter().position(|column| *column == name).ok_or_else(invalid_native_row)?;
        Ok(self.values.get(index).filter(|value| !matches!(value, Value::NULL)))
    }
    pub(crate) fn has_column(&self, name: &str) -> bool {
        self.columns.iter().any(|column| column.eq_ignore_ascii_case(name))
    }

    /// Missing columns are malformed; native NULL is unavailable. Never pass exact counters through f64.
    pub(crate) fn unsigned(&self, name: &str) -> Result<Option<u64>> {
        match self.native_value(name)? {
            None => Ok(None),
            Some(Value::UInt(value)) => Ok(Some(*value)),
            Some(Value::Int(value)) => u64::try_from(*value).map(Some).map_err(|_| invalid_native_row()),
            Some(Value::Bytes(value)) if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .map(Some)
                    .ok_or_else(invalid_native_row)
            }
            _ => Err(invalid_native_row()),
        }
    }

    /// Native text must be valid UTF-8. Display helpers may be forgiving; exported identities and SQL cannot be.
    pub(crate) fn native_text(&self, name: &str) -> Result<Option<String>> {
        match self.native_value(name)? {
            None => Ok(None),
            Some(Value::Bytes(value)) => String::from_utf8(value.clone()).map(Some).map_err(|_| invalid_native_row()),
            _ => Err(invalid_native_row()),
        }
    }

    pub(crate) fn decoded_value_bytes(&self) -> usize {
        decoded_value_bytes(&self.values)
    }

    fn value(&self, name: &str) -> Option<&Value> {
        let name = name.to_ascii_lowercase();
        let index = self.columns.iter().position(|c| *c == name)?;
        self.values.get(index).filter(|v| !matches!(v, Value::NULL))
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.value(name).map(value_text)
    }

    /// The first column, whatever the server named it (`ANALYZE`, `EXPLAIN`).
    pub fn first_text(&self) -> Option<String> {
        self.values.first().filter(|v| !matches!(v, Value::NULL)).map(value_text)
    }

    pub fn string(&self, name: &str) -> String {
        self.text(name).unwrap_or_default()
    }

    pub fn number(&self, name: &str) -> Option<f64> {
        match self.value(name)? {
            Value::Int(i) => Some(*i as f64),
            Value::UInt(u) => Some(*u as f64),
            Value::Float(f) => Some(f64::from(*f)),
            Value::Double(d) => Some(*d),
            Value::Bytes(b) => String::from_utf8_lossy(b).trim().parse().ok(),
            _ => None,
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        self.number(name).is_some_and(|n| n != 0.0)
            || self.text(name).is_some_and(|t| matches!(t.to_ascii_uppercase().as_str(), "YES" | "ON" | "TRUE"))
    }
}

pub(crate) fn decoded_value_bytes(values: &[Value]) -> usize {
    values.iter().fold(0usize, |total, value| {
        total.saturating_add(match value {
            Value::NULL => 0,
            Value::Bytes(value) => value.len(),
            Value::Int(_) | Value::UInt(_) | Value::Double(_) => 8,
            Value::Float(_) => 4,
            Value::Date(..) => 11,
            Value::Time(..) => 12,
        })
    })
}

fn invalid_native_row() -> culprit_core::Error {
    culprit_core::telemetry::CollectionStop::InvalidPage.error()
}

pub fn value_text(value: &Value) -> String {
    match value {
        Value::NULL => String::new(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Date(y, mo, d, h, mi, s, us) => {
            if *us > 0 {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            }
        }
        Value::Time(negative, days, h, mi, s, us) => {
            let hours = u32::from(*h) + days * 24;
            let sign = if *negative { "-" } else { "" };
            if *us > 0 {
                format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}")
            } else {
                format!("{sign}{hours:02}:{mi:02}:{s:02}")
            }
        }
    }
}

pub fn tagged(sql: &str) -> String {
    format!("{TAG} {sql}")
}

const APPLICATION_ROWS: usize = 10_000;
pub async fn query(conn: &mut impl crate::read::QueryRead, sql: &str) -> Result<Vec<Rec>> {
    conn.read(sql, vec![], APPLICATION_ROWS).await
}
pub async fn query_one(conn: &mut impl crate::read::QueryRead, sql: &str) -> Result<Option<Rec>> {
    let mut rows = conn.read(sql, vec![], 1).await?;
    Ok(rows.pop())
}

/// EXPLAIN through the binary protocol: a prepared statement can only ever hold one statement, whatever the text says.
/// Servers that can't prepare EXPLAIN (ER_UNSUPPORTED_PS) get the text protocol; callers have already checked the text
/// is a single statement.
pub async fn explain_one(conn: &mut impl crate::read::QueryRead, sql: &str) -> Result<Option<Rec>> {
    conn.explain(sql).await
}

/// A query with bound parameters (the schema name, mostly), so names are never spliced into SQL.
pub async fn query_with(conn: &mut impl crate::read::QueryRead, sql: &str, params: Vec<Value>) -> Result<Vec<Rec>> {
    conn.read(sql, params, APPLICATION_ROWS).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_render_as_text() {
        assert_eq!(value_text(&Value::Bytes(b"abc".to_vec())), "abc");
        assert_eq!(value_text(&Value::Date(2026, 1, 2, 3, 4, 5, 0)), "2026-01-02 03:04:05");
        assert_eq!(value_text(&Value::Time(false, 1, 2, 3, 4, 0)), "26:03:04");
        assert_eq!(value_text(&Value::UInt(7)), "7");
    }

    fn native(values: Vec<(&str, Value)>) -> Rec {
        Rec {
            columns: values.iter().map(|(name, _)| name.to_string()).collect(),
            values: values.into_iter().map(|(_, value)| value).collect(),
        }
    }

    #[test]
    fn native_counters_preserve_unsigned_precision_in_both_protocols() {
        let row = native(vec![
            ("binary", Value::UInt(u64::MAX)),
            ("text", Value::Bytes(b"18446744073709551615".to_vec())),
            ("above_js", Value::Bytes(b"9007199254740993".to_vec())),
        ]);
        assert_eq!(row.unsigned("binary").unwrap(), Some(u64::MAX));
        assert_eq!(row.unsigned("text").unwrap(), Some(u64::MAX));
        assert_eq!(row.unsigned("above_js").unwrap(), Some(9_007_199_254_740_993));
    }

    #[test]
    fn native_null_is_distinct_from_missing_malformed_negative_or_rounded_counters() {
        let row = native(vec![("unknown", Value::NULL), ("zero", Value::UInt(0))]);
        assert_eq!(row.unsigned("unknown").unwrap(), None);
        assert_eq!(row.unsigned("zero").unwrap(), Some(0));
        assert!(row.unsigned("missing").is_err());
        for value in [
            Value::Int(-1),
            Value::Double(42.0),
            Value::Bytes(b" 42".to_vec()),
            Value::Bytes(b"42.0".to_vec()),
            Value::Bytes(b"18446744073709551616".to_vec()),
        ] {
            assert!(native(vec![("value", value)]).unsigned("value").is_err());
        }
    }

    #[test]
    fn native_text_rejects_corrupt_identities_and_accounts_for_decoded_values() {
        let row = native(vec![
            ("text", Value::Bytes(b"native".to_vec())),
            ("null", Value::NULL),
            ("bad", Value::Bytes(vec![0xff])),
            ("counter", Value::UInt(u64::MAX)),
        ]);
        assert_eq!(row.native_text("text").unwrap().as_deref(), Some("native"));
        assert_eq!(row.native_text("null").unwrap(), None);
        assert!(row.native_text("bad").is_err());
        assert!(row.native_text("missing").is_err());
        assert_eq!(row.decoded_value_bytes(), 15);
    }
}
