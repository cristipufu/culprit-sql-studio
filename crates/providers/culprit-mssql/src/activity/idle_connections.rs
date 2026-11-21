//! Optional census-only connection context. The ordinary Live/incident frame is unchanged.
use super::{IdentityDetail, page_server_reads::Availability, sql};
use crate::{
    read::{invalid_data, optional_boolean, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::MssqlIdleSessionConnection};

const COLUMNS: &str =
    ",@idle_connection_scope AS idle_connection_scope,@idle_connection_visibility AS idle_connection_visibility,
 pc.candidates AS idle_connection_candidates,pc.connection_id AS idle_connection_id,
 pc.connected AS idle_connected_without_offset";
const UNAVAILABLE: &str =
    ",@idle_connection_scope AS idle_connection_scope,@idle_connection_visibility AS idle_connection_visibility,
 CAST(NULL AS int) AS idle_connection_candidates,CAST(NULL AS varchar(36)) AS idle_connection_id,
 CAST(NULL AS varchar(40)) AS idle_connected_without_offset";

// TOP(2) detects ambiguity; it never selects a representative among several native parents. The
// aggregate returns one row per existing session/request row. This is additional metadata work,
// not an internal-work bound or a source-cost qualification.
const RELATION: &str = r#"
 OUTER APPLY (
 SELECT COUNT(*) AS candidates,MIN(candidate.connection_id) AS connection_id,MIN(candidate.connected) AS connected
 FROM (
  SELECT TOP (2) CONVERT(varchar(36),c.connection_id) AS connection_id,
   CONVERT(varchar(40),CAST(c.connect_time AS datetime2),126) AS connected
  FROM sys.dm_exec_connections c
  WHERE r.request_id IS NULL AND s.login_time IS NOT NULL
   AND CONVERT(date,s.login_time)<>CONVERT(date,'19000101',112)
   AND LOWER(s.status) NOT IN(N'dormant',N'preconnect')
   AND NOT EXISTS(SELECT 1 FROM sys.dm_exec_requests current_request WHERE current_request.session_id=s.session_id)
   AND c.session_id=s.session_id AND c.most_recent_session_id=s.session_id AND c.parent_connection_id IS NULL
 ) candidate
 ) pc"#;

pub(super) fn query(identity: IdentityDetail, availability: Availability) -> String {
    let select = |columns, relation| {
        let rows = sql::metadata_rows(availability, columns, relation);
        sql::request_frame_select_with_context(&rows, identity, sql::QueryPolicy::Activity, "")
    };
    format!(
        "{clock}DECLARE /* culprit */ @idle_connection_scope bit={scope};\nDECLARE @idle_connection_visibility bit={permission};\nIF @idle_connection_scope=1 AND @idle_connection_visibility=1\nBEGIN\n{observed}\nEND\nELSE\nBEGIN\n{unavailable}\nEND",
        clock = sql::FRAME_CLOCK,
        scope = super::visibility::connection_scope(),
        permission = super::visibility::connections(),
        observed = select(COLUMNS, RELATION),
        unavailable = select(UNAVAILABLE, "")
    )
}

pub(super) fn decode(row: &Rec, eligible: bool) -> Result<Option<MssqlIdleSessionConnection>> {
    // The existing ordinary frame deliberately has no optional connection columns.
    if !row.columns().any(|(name, _)| name == "idle_connection_visibility") {
        return Ok(None);
    }
    if !eligible {
        return Ok(None);
    }
    let scope = optional_boolean(row, "idle_connection_scope")?;
    let permitted = optional_boolean(row, "idle_connection_visibility")?;
    if scope != Some(true) {
        if permitted.is_some() {
            return Err(invalid_data());
        }
        empty_candidates(row)?;
        return Ok(Some(MssqlIdleSessionConnection::ScopeUnavailable));
    }
    if permitted != Some(true) {
        empty_candidates(row)?;
        return Ok(Some(MssqlIdleSessionConnection::VisibilityUnavailable { permitted }));
    }
    let count = crate::read::integer(row, "idle_connection_candidates")?;
    let connection = optional_text(row, "idle_connection_id")?;
    let connected = optional_text(row, "idle_connected_without_offset")?;
    Ok(Some(match count {
        0 if connection.is_none() && connected.is_none() => MssqlIdleSessionConnection::Missing,
        1 => MssqlIdleSessionConnection::Observed {
            connection_id: connection.ok_or_else(invalid_data)?,
            connected_without_offset: crate::sql_time::native_time(&connected.ok_or_else(invalid_data)?)?,
        },
        2 => MssqlIdleSessionConnection::Ambiguous,
        _ => return Err(invalid_data()),
    }))
}

fn empty_candidates(row: &Rec) -> Result<()> {
    if ["idle_connection_candidates", "idle_connection_id", "idle_connected_without_offset"]
        .iter()
        .any(|name| !row.columns().any(|(column, value)| column == *name && matches!(value, crate::value::Val::Null)))
    {
        return Err(invalid_data());
    }
    Ok(())
}
