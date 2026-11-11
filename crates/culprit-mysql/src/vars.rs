//! Portable, allowlisted native variable/status reads. SHOW does not depend on performance_schema being enabled.

use std::collections::HashMap;

use crate::{read::QueryRead, rows::Rec};
use culprit_core::Result;

#[derive(Clone, Copy)]
pub(crate) enum VariableSet {
    Configuration,
    Status,
}
impl VariableSet {
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Configuration => "global-variables",
            Self::Status => "global-status",
        }
    }
    pub(crate) fn query(self, names: &[&str]) -> String {
        let kind = match self {
            Self::Configuration => "VARIABLES",
            Self::Status => "STATUS",
        };
        format!("SHOW GLOBAL {kind} WHERE Variable_name IN ({})", names_list(names))
    }
}

fn names_list(names: &[&str]) -> String {
    names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ")
}

/// `(name, value)` with lower-case names; names the server doesn't know are absent.
pub(crate) async fn records<R: QueryRead>(reader: &mut R, set: VariableSet, names: &[&str]) -> Result<Vec<Rec>> {
    reader.read(&set.query(names), vec![], names.len() + 1).await
}

pub(crate) async fn variables<R: QueryRead>(reader: &mut R, names: &[&str]) -> Result<Vec<(String, String)>> {
    let mut values = Vec::new();
    for row in records(reader, VariableSet::Configuration, names).await? {
        if let (Some(name), Some(value)) = (row.native_text("variable_name")?, row.native_text("value")?) {
            values.push((name.to_ascii_lowercase(), value));
        }
    }
    Ok(values)
}

/// Numeric status counters by their documented (capitalized) names.
pub(crate) async fn status<R: QueryRead>(reader: &mut R, names: &[&str]) -> Result<HashMap<String, f64>> {
    let rows = records(reader, VariableSet::Status, names).await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row.text("variable_name")?;
            let value = row.text("value")?;
            let canonical = names.iter().find(|n| n.eq_ignore_ascii_case(&name))?;
            Some((canonical.to_string(), value.trim().parse::<f64>().ok()?))
        })
        .collect())
}
