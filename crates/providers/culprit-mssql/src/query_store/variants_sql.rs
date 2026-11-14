//! Query-variant relationships are native metadata, never a second runtime population.
use super::{catalog_sql, decode};
use crate::{read::integer, value::Rec};
use culprit_core::{QueryVariantFamily, Result, telemetry::CollectionStop};

macro_rules! readable {
    () => {
        "CASE WHEN OBJECT_ID(N'sys.query_store_query_variant') IS NOT NULL THEN CASE WHEN HAS_PERMS_BY_NAME(N'sys.query_store_query_variant',N'OBJECT',N'SELECT')=1 THEN 1 ELSE 2 END WHEN CAST(SERVERPROPERTY('ProductMajorVersion') AS int)<16 AND CAST(SERVERPROPERTY('EngineEdition') AS int) IN(2,3,4) THEN 0 ELSE 2 END"
    };
}
pub(crate) use readable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Unsupported,
    Readable,
    Unavailable,
}

impl Availability {
    pub(crate) fn decode(row: &Rec, column: &str) -> Result<Self> {
        match integer(row, column)? {
            0 => Ok(Self::Unsupported),
            1 => Ok(Self::Readable),
            2 => Ok(Self::Unavailable),
            _ => Err(crate::read::invalid_data()),
        }
    }
    pub(crate) fn native_property(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported_native_version",
            Self::Readable => "native_view_readable",
            Self::Unavailable => "visibility_unverified",
        }
    }
}

pub(crate) const COLUMNS: &str = concat!(
    ",v.query_variant_query_id AS family_variant_id,",
    catalog_sql::query_identity_columns!("v.parent_query_id", "parent", "parent_"),
    ",",
    catalog_sql::plan_identity_columns!("dispatcher", "dispatcher_"),
    ",dispatcher.query_id AS dispatcher_query_id"
);
pub(crate) const JOINS: &str = r#"
LEFT JOIN sys.query_store_query_variant v ON v.query_variant_query_id=ids.id
LEFT JOIN sys.query_store_query parent ON parent.query_id=v.parent_query_id
LEFT JOIN sys.query_store_plan dispatcher ON dispatcher.plan_id=v.dispatcher_plan_id"#;

pub(crate) fn decode(row: &Rec) -> Result<Option<QueryVariantFamily>> {
    if row.str("family_variant_id").is_none() {
        if row.columns().any(|(name, _)| {
            (name.starts_with("parent_") || name.starts_with("dispatcher_")) && row.str(name).is_some()
        }) {
            return Err(CollectionStop::SourceChanged.error());
        }
        return Ok(None);
    }
    let decode = || {
        let variant = decode::query_identity(row)?;
        if variant.query_id != integer(row, "family_variant_id")?.to_string() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let parent = decode::prefixed_query_identity(row, "parent_")?;
        if parent.query_id != integer(row, "dispatcher_query_id")?.to_string() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let dispatcher = decode::plan_identity_for_query(row, "dispatcher_", parent.clone())?
            .ok_or_else(|| CollectionStop::SourceChanged.error())?;
        let family = QueryVariantFamily { variant, parent, dispatcher };
        family.is_valid().then_some(family).ok_or_else(|| CollectionStop::SourceChanged.error())
    };
    decode().map(Some).map_err(|_| CollectionStop::SourceChanged.error())
}
