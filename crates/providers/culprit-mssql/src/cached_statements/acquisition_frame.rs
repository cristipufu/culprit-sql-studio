//! One native role point at payload-batch start. It does not establish uninterrupted role coverage.
use crate::{read::database_writable, value::Rec};
use culprit_core::{Result, telemetry::CollectionStop};

#[derive(Clone, Copy)]
pub(crate) enum Projection {
    Statement,
    AcquisitionFrame,
}

impl Projection {
    pub(crate) fn context_columns(self, cache: &str, text: &str, database: &str) -> String {
        match self {
            Self::Statement => super::context_columns_from(cache, text, database),
            Self::AcquisitionFrame => format!(
                "{},@culprit_cache_updateability AS updateability",
                Self::Statement.context_columns(cache, text, database),
            ),
        }
    }

    pub(crate) fn batch(self, statement: String) -> String {
        match self {
            Self::Statement => statement,
            Self::AcquisitionFrame => format!(
                "DECLARE /* culprit */ @culprit_cache_updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability'));\n{statement}",
            ),
        }
    }
}

pub(crate) fn require_role(row: &Rec, expected: Option<bool>) -> Result<()> {
    require_writable(database_writable(row)?, expected)
}

pub(crate) fn require_writable(actual: Option<bool>, expected: Option<bool>) -> Result<()> {
    let (Some(expected), Some(actual)) = (expected, actual) else {
        return Err(CollectionStop::SourceUnavailable.error());
    };
    if actual != expected {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}
