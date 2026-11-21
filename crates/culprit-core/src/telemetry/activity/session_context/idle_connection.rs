//! An original idle session's unique primary transport is context, not request or action authority.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MssqlIdleSessionConnection {
    Observed {
        connection_id: String,
        connected_without_offset: String,
    },
    /// No visible primary connection matched the exact original session association.
    Missing,
    /// The documented native request scope cannot certify absence across the census session population.
    ScopeUnavailable,
    /// At least two primary candidates matched. No representative is retained.
    Ambiguous,
    /// Effective native permission was false or could not be established; no connection population was read.
    VisibilityUnavailable {
        permitted: Option<bool>,
    },
}
impl MssqlIdleSessionConnection {
    pub(super) fn is_valid(&self) -> bool {
        match self {
            Self::Observed { connection_id, connected_without_offset } => {
                native_connection_token(connection_id)
                    && connection_id.bytes().any(|byte| byte != b'0' && byte != b'-')
                    && native_local_time(connected_without_offset)
            }
            Self::VisibilityUnavailable { permitted } => *permitted != Some(true),
            Self::Missing | Self::Ambiguous | Self::ScopeUnavailable => true,
        }
    }
}
