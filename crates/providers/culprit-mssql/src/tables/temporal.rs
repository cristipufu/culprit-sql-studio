//! Optional same-row catalog fields. Layout/permissions are evaluated once per batch, not per application table.
use crate::{
    read::{invalid_data, optional_integer, optional_signed_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

pub(super) struct QueryParts {
    pub before_temporal: String,
    pub after_temporal: String,
    pub companion_expression: String,
}
#[derive(Clone, Copy)]
struct Column {
    name: &'static str,
    kind: &'static str,
    native_bytes: i32,
    minimum_major: i32,
}
const COLUMNS: [Column; 6] = [
    Column { name: "temporal_type", kind: "tinyint", native_bytes: 1, minimum_major: 13 },
    Column { name: "temporal_type_desc", kind: "nvarchar", native_bytes: 120, minimum_major: 13 },
    Column { name: "history_table_id", kind: "int", native_bytes: 4, minimum_major: 13 },
    Column { name: "history_retention_period", kind: "int", native_bytes: 4, minimum_major: 14 },
    Column { name: "history_retention_period_unit", kind: "int", native_bytes: 4, minimum_major: 14 },
    Column { name: "history_retention_period_unit_desc", kind: "nvarchar", native_bytes: 20, minimum_major: 14 },
];
impl Column {
    fn variable(self) -> String {
        format!("@culprit_{}_availability", self.name)
    }
    fn sql_type(self) -> String {
        if self.kind == "nvarchar" { format!("nvarchar({})", self.native_bytes / 2) } else { self.kind.into() }
    }
    fn capability(self) -> String {
        let Self { name, kind, native_bytes, minimum_major } = self;
        let variable = self.variable();
        format!(
            r#"DECLARE /* culprit */ {variable} int=CASE
 WHEN @culprit_temporal_metadata=0 OR @culprit_temporal_select=0 THEN 2
 WHEN @culprit_temporal_metadata IS NULL OR @culprit_temporal_select IS NULL
  OR @culprit_temporal_major IS NULL OR @culprit_temporal_edition IS NULL
  OR OBJECT_ID(N'sys.tables') IS NULL THEN 3
 WHEN NOT(@culprit_temporal_major>={minimum_major} OR @culprit_temporal_edition IN(5,8)) THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.tables') AND name=N'{name}') THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.tables') AND name=N'{name}'
  AND system_type_id=TYPE_ID(N'{kind}') AND max_length={native_bytes}) THEN 3
 WHEN HAS_PERMS_BY_NAME(N'sys.tables',N'OBJECT',N'SELECT',N'{name}',N'COLUMN')=0 THEN 2
 WHEN HAS_PERMS_BY_NAME(N'sys.tables',N'OBJECT',N'SELECT',N'{name}',N'COLUMN')=1 THEN 0 ELSE 3 END;"#
        )
    }
    fn expression(self) -> String {
        let name = self.name;
        let variable = self.variable();
        let kind = self.sql_type();
        format!(
            "CASE WHEN {variable}=0 THEN N't.{name}' ELSE N'CAST(NULL AS {kind})' END+N' AS {name},{variable} AS {name}_availability'"
        )
    }
}
pub(super) fn declarations() -> String {
    format!("DECLARE /* culprit */ @culprit_temporal_metadata int=HAS_PERMS_BY_NAME(DB_NAME(),N'DATABASE',N'VIEW DEFINITION'),
 @culprit_temporal_select int=HAS_PERMS_BY_NAME(N'sys.tables',N'OBJECT',N'SELECT'),
 @culprit_temporal_major int=TRY_CONVERT(int,SERVERPROPERTY(N'ProductMajorVersion')),
 @culprit_temporal_edition int=TRY_CONVERT(int,SERVERPROPERTY(N'EngineEdition'));
{}", COLUMNS.into_iter().map(Column::capability).collect::<Vec<_>>().join("
"))
}
pub(super) fn expression(parts: QueryParts) -> String {
    // The final statement is dynamically composed: retain monitoring provenance without claiming that a prefix
    // hash is the identity of its fully evaluated native statement.
    let before = format!(
        "{} {}
{}",
        crate::statement_origin::OWN_QUERY_MARKER,
        crate::statement_origin::MONITORING_QUERY_MARKER,
        parts.before_temporal
    );
    let companion = parts.companion_expression;
    let fields = COLUMNS.into_iter().map(Column::expression).collect::<Vec<_>>().join("+N','+");
    // Promote before concatenation: the catalog-only query exceeds nvarchar(4000), even when the assignment
    // target is nvarchar(max). Otherwise SQL Server truncates the intermediate statement before execution.
    format!(
        "CAST({} AS nvarchar(max))+{fields}+N','+{companion}+{}",
        crate::catalog_visibility::sql_literal(&before),
        crate::catalog_visibility::sql_literal(&parts.after_temporal)
    )
}
pub(super) fn parameters() -> String {
    COLUMNS.into_iter().map(|column| format!(",{} int", column.variable())).collect::<String>()
}
pub(super) fn arguments() -> String {
    COLUMNS
        .into_iter()
        .map(|column| {
            let variable = column.variable();
            format!(",{variable}={variable}")
        })
        .collect::<String>()
}
pub(super) fn decode(row: &Rec) -> Result<MssqlTableTemporalConfiguration> {
    let availability = |name: &str| crate::catalog_visibility::availability(row, &format!("{name}_availability"));
    let value = MssqlTableTemporalConfiguration {
        temporal_type_availability: availability("temporal_type")?,
        temporal_type: optional_integer(row, "temporal_type")?
            .map(|value| u8::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
        temporal_type_desc_availability: availability("temporal_type_desc")?,
        temporal_type_desc: optional_text(row, "temporal_type_desc")?,
        history_table_id_availability: availability("history_table_id")?,
        history_table_id: optional_integer(row, "history_table_id")?
            .map(|value| u32::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
        history_retention_period_availability: availability("history_retention_period")?,
        history_retention_period: optional_signed_integer(row, "history_retention_period")?
            .map(|value| i32::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
        history_retention_period_unit_availability: availability("history_retention_period_unit")?,
        history_retention_period_unit: optional_signed_integer(row, "history_retention_period_unit")?
            .map(|value| i32::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
        history_retention_period_unit_desc_availability: availability("history_retention_period_unit_desc")?,
        history_retention_period_unit_desc: optional_text(row, "history_retention_period_unit_desc")?,
    };
    value.is_valid().then_some(value).ok_or_else(invalid_data)
}
#[cfg(test)]
mod tests;
