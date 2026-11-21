//! A one-to-zero-or-one persisted catalog companion, never a full-text population/queue traversal.
use crate::{
    read::{invalid_data, optional_boolean, optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

const CATALOG: &str = "sys.fulltext_indexes";
const COLUMNS: &str = r#"ft.object_id AS full_text_object_id,ft.unique_index_id AS full_text_unique_index_id,
 ft.fulltext_catalog_id AS full_text_catalog_id,ft.is_enabled AS full_text_enabled,
 ft.change_tracking_state AS full_text_change_tracking,ft.has_crawl_completed AS full_text_last_crawl_completed,
 CAST(0 AS int) AS full_text_availability"#;
const UNAVAILABLE_COLUMNS: &str = r#"CAST(NULL AS int) AS full_text_object_id,CAST(NULL AS int) AS full_text_unique_index_id,
 CAST(NULL AS int) AS full_text_catalog_id,CAST(NULL AS bit) AS full_text_enabled,
 CAST(NULL AS char(1)) AS full_text_change_tracking,CAST(NULL AS bit) AS full_text_last_crawl_completed,
 @culprit_full_text_availability AS full_text_availability"#;

/// Both branches come from the SAME table projection. Only the selected branch compiles/executes.
/// Fixed catalog-layout and permission checks do not dispatch a second read or retry an unavailable source.
pub(super) fn query(table_query: impl Fn(&str, &str) -> super::temporal::QueryParts) -> String {
    let joined = super::temporal::expression(table_query(
        COLUMNS,
        &format!("LEFT JOIN {CATALOG} ft ON ft.object_id=t.object_id"),
    ));
    let unavailable = super::temporal::expression(table_query(UNAVAILABLE_COLUMNS, ""));
    let temporal = super::temporal::declarations();
    let parameters = super::temporal::parameters();
    let arguments = super::temporal::arguments();
    format!(
        r#"DECLARE /* culprit */ @culprit_full_text_metadata int=HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION');
DECLARE /* culprit */ @culprit_full_text_select int=HAS_PERMS_BY_NAME(N'{CATALOG}',N'OBJECT',N'SELECT');
DECLARE /* culprit */ @culprit_full_text_availability int=CASE
 WHEN @culprit_full_text_metadata=0 OR @culprit_full_text_select=0 THEN 2
 WHEN @culprit_full_text_metadata IS NULL OR @culprit_full_text_select IS NULL THEN 3
 WHEN (SELECT COUNT(*) FROM sys.all_columns
  WHERE object_id=OBJECT_ID(N'{CATALOG}')
   AND ((name IN(N'object_id',N'unique_index_id',N'fulltext_catalog_id') AND system_type_id=TYPE_ID(N'int'))
    OR (name IN(N'is_enabled',N'has_crawl_completed') AND system_type_id=TYPE_ID(N'bit'))
    OR (name=N'change_tracking_state' AND system_type_id=TYPE_ID(N'char') AND max_length=1)))=6 THEN 0
 ELSE 1 END;
{temporal}
DECLARE /* culprit */ @culprit_table_sql nvarchar(max);
IF @culprit_full_text_availability=0
 SET /* culprit */ @culprit_table_sql={joined};
ELSE
 SET /* culprit */ @culprit_table_sql={unavailable};
EXEC /* culprit */ sys.sp_executesql @culprit_table_sql,
 N'@P1 int,@culprit_full_text_availability int{parameters}',
 @P1=@P1,@culprit_full_text_availability=@culprit_full_text_availability{arguments};"#
    )
}

pub(super) fn decode(row: &Rec, table_object_id: u32) -> Result<MssqlTableFullText> {
    let availability = crate::catalog_visibility::availability(row, "full_text_availability")?;
    let object_id = native_id(row, "full_text_object_id")?;
    let index = MssqlFullTextIndex {
        unique_index_id: native_id(row, "full_text_unique_index_id")?,
        catalog_id: native_id(row, "full_text_catalog_id")?,
        enabled: optional_boolean(row, "full_text_enabled")?,
        change_tracking: match optional_text(row, "full_text_change_tracking")?.as_deref() {
            Some("M") => Some(MssqlFullTextChangeTracking::Manual),
            Some("A") => Some(MssqlFullTextChangeTracking::Automatic),
            Some("O") => Some(MssqlFullTextChangeTracking::Off),
            None => None,
            _ => return Err(invalid_data()),
        },
        last_crawl_completed: optional_boolean(row, "full_text_last_crawl_completed")?,
    };
    if object_id.is_some_and(|id| id != table_object_id)
        || (availability != MssqlCatalogAvailability::Available && object_id.is_some())
    {
        return Err(invalid_data());
    }
    let index = match object_id {
        Some(_) => Some(index),
        None if index.unique_index_id.is_none()
            && index.catalog_id.is_none()
            && index.enabled.is_none()
            && index.change_tracking.is_none()
            && index.last_crawl_completed.is_none() =>
        {
            None
        }
        None => return Err(invalid_data()),
    };
    let state = MssqlTableFullText { availability, index };
    state.is_valid().then_some(state).ok_or_else(invalid_data)
}

fn native_id(row: &Rec, name: &str) -> Result<Option<u32>> {
    optional_integer(row, name)?
        .map(|value| u32::try_from(value).ok().filter(|id| *id > 0 && *id <= i32::MAX as u32).ok_or_else(invalid_data))
        .transpose()
}

#[cfg(test)]
mod tests;
