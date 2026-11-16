//! A bounded search of native per-backend views; no row proves neither absence nor completion.
use super::{analyze, cluster, index, vacuum};
use crate::{native, read::QueryRead, rows::Row};
use culprit_core::{ErrorCode, Result, telemetry::*};

pub(super) const SOURCE: &str = "postgres-maintenance-progress";

pub(super) struct Read {
    pub source: &'static str,
    pub observation: Option<MaintenanceProgressObservation>,
    pub unavailable: Option<ErrorCode>,
}

#[derive(Clone, Copy)]
enum View {
    Index,
    Vacuum,
    Analyze,
    Cluster,
}

impl View {
    fn minimum_version(self) -> u32 {
        match self {
            Self::Index | Self::Cluster => 120_000,
            Self::Vacuum => 90_600,
            Self::Analyze => 130_000,
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Index => index::SOURCE,
            Self::Vacuum => vacuum::SOURCE,
            Self::Analyze => analyze::SOURCE,
            Self::Cluster => cluster::SOURCE,
        }
    }

    fn sql(self, database: &str, actor: &ActivityKey, version: u32) -> Result<String> {
        match self {
            Self::Index => index::sql(database, actor),
            Self::Vacuum => vacuum::sql(database, actor, version),
            Self::Analyze => analyze::sql(database, actor, version),
            Self::Cluster => cluster::sql(database, actor),
        }
    }

    fn decode(self, row: &Row, database: &str, actor: &ActivityKey) -> Result<MaintenanceProgressObservation> {
        match self {
            Self::Index => index::decode(row, database, actor),
            Self::Vacuum => vacuum::decode(row, database, actor),
            Self::Analyze => analyze::decode(row, database, actor),
            Self::Cluster => cluster::decode(row, database, actor),
        }
    }
}

pub(super) async fn read<R: QueryRead>(
    reader: &mut R,
    database_oid: &str,
    actor: &ActivityKey,
    version: u32,
) -> Result<Read> {
    let mut unavailable = None;
    for view in [View::Index, View::Vacuum, View::Analyze, View::Cluster]
        .into_iter()
        .filter(|view| version >= view.minimum_version())
    {
        match reader.query(&view.sql(database_oid, actor, version)?, 2).await {
            Ok(rows) if rows.len() > 1 => return Err(native::invalid()),
            Ok(rows) => {
                if let Some(row) = rows.first() {
                    return Ok(Read {
                        source: view.source(),
                        observation: Some(view.decode(row, database_oid, actor)?),
                        unavailable: None,
                    });
                }
            }
            Err(error) if super::reader::optional(&error) => {
                if unavailable != Some(ErrorCode::PermissionDenied) {
                    unavailable = Some(error.code);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Read { source: SOURCE, observation: None, unavailable })
}
