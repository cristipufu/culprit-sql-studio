//! Constraint evidence uses native flags and shares the foreign-key relation with catalog capture.
use crate::read::{NativeRead, boolean, integer, invalid_data, rows, text};
use culprit_core::{ConstraintIssue, ConstraintKind, Result};
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Vec<ConstraintIssue>> {
    let sql = format!(
        "{} UNION ALL {} OPTION(MAXDOP 1)",
        crate::foreign_keys::issues_sql(),
        crate::check_constraints::issues_sql()
    );
    rows(reader, &sql)
        .await?
        .iter()
        .map(|row| {
            Ok(ConstraintIssue {
                schema: text(row, "schema_name")?,
                table: text(row, "table_name")?,
                name: text(row, "name")?,
                kind: match integer(row, "kind")? {
                    0 => ConstraintKind::ForeignKey,
                    1 => ConstraintKind::Check,
                    _ => return Err(invalid_data()),
                },
                disabled: boolean(row, "disabled")?,
                inheritance_child: false,
            })
        })
        .collect()
}
