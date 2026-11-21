//! Closed native log-capture families share scope and retained-row admission, never their native data.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MssqlChangeCaptureFamily {
    Cdc,
    ChangeFeed,
}
impl MssqlChangeCaptureFamily {
    pub fn dataset(self) -> SnapshotDataset {
        match self {
            Self::Cdc => SnapshotDataset::Cdc,
            Self::ChangeFeed => SnapshotDataset::ChangeFeed,
        }
    }
    pub fn source(self) -> &'static str {
        match self {
            Self::Cdc => "mssql-cdc",
            Self::ChangeFeed => "mssql-change-feed",
        }
    }
    pub fn sources(self) -> [&'static str; 3] {
        match self {
            Self::Cdc => [MSSQL_CDC_STATUS_SOURCE, MSSQL_CDC_SCANS_SOURCE, MSSQL_CDC_ERRORS_SOURCE],
            Self::ChangeFeed => {
                [MSSQL_CHANGE_FEED_STATUS_SOURCE, MSSQL_CHANGE_FEED_SCANS_SOURCE, MSSQL_CHANGE_FEED_ERRORS_SOURCE]
            }
        }
    }
    pub fn maximum(self, population: usize) -> usize {
        match (self, population) {
            (_, 0) => 1,
            (Self::Cdc, 1) => MAX_MSSQL_CDC_SCAN_ROWS,
            (Self::Cdc, 2) => MAX_MSSQL_CDC_ERROR_ROWS,
            (Self::ChangeFeed, 1) => MAX_MSSQL_CHANGE_FEED_SCAN_ROWS,
            (Self::ChangeFeed, 2) => MAX_MSSQL_CHANGE_FEED_ERROR_ROWS,
            _ => 0,
        }
    }
    pub fn valid_header(self, header: &StateSnapshotDescription) -> bool {
        header.dataset == self.dataset()
            && header.is_valid()
            && header.sources.len() == 3
            && self.sources().iter().enumerate().all(|(index, name)| {
                header
                    .sources
                    .iter()
                    .any(|source| source.source == *name && source.records as usize <= self.maximum(index))
            })
    }
}

/// Borrows only existing typed original rows; neither serialized JSON nor matching display text adds proof.
pub enum MssqlChangeCapturePoint<'a> {
    Cdc(&'a MssqlCdcObservation),
    ChangeFeed(&'a MssqlChangeFeedObservation),
}
impl<'a> MssqlChangeCapturePoint<'a> {
    pub fn from_row(row: &'a OperationalObservation, family: MssqlChangeCaptureFamily) -> Option<Self> {
        if !row.is_valid() {
            return None;
        }
        match (&row.fact, family) {
            (OperationalFact::MssqlCdc(point), MssqlChangeCaptureFamily::Cdc) => Some(Self::Cdc(point)),
            (OperationalFact::MssqlChangeFeed(point), MssqlChangeCaptureFamily::ChangeFeed) => {
                Some(Self::ChangeFeed(point))
            }
            _ => None,
        }
    }
    pub fn context(&self) -> &'a NativeReadContext {
        match self {
            Self::Cdc(point) => &point.context,
            Self::ChangeFeed(point) => &point.context,
        }
    }
    pub fn database_id(&self) -> u32 {
        match self {
            Self::Cdc(point) => point.database_id,
            Self::ChangeFeed(point) => point.database_id,
        }
    }
    pub fn position(&self) -> u32 {
        match self {
            Self::Cdc(point) => point.position,
            Self::ChangeFeed(point) => point.position,
        }
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        match self {
            Self::Cdc(point) => point.fits_source(source),
            Self::ChangeFeed(point) => point.fits_source(source),
        }
    }
}

pub fn change_capture_owner_is_qualified(context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && matches!(context.partition,SourcePartition::InstanceLocal{member:Some(ref member),..} if matches!(member.kind,SharedResourceKind::Instance|SharedResourceKind::Node))
        && context.database_writable.is_some()
        && matches!(context.target, CollectionTarget::Database(ref database) if database.engine == crate::Engine::Mssql)
}
pub(super) fn counter(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(valid_counter)
}
pub(super) fn local_time(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(super::catalog::local_time)
}
pub(super) fn lsn(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| {
        !value.is_empty() && value.len() <= 23 && value.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b':')
    })
}
