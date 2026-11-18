//! Only the ordinary census requests these native session facts. Focused request projections stay unchanged.
use super::rows::{age, value};
use crate::{
    read::{integer, invalid_data, optional_signed_integer, optional_text, text},
    sql_time::native_time,
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

/// Fixed native establishment policy for held request inspection and administrative preflight/final guards.
/// Each query uses the native sessions alias `s`; NULL cannot become a positive establishment verdict.
pub(crate) const ESTABLISHED_SESSION_SQL: &str = "s.login_time IS NOT NULL \
 AND CONVERT(date,s.login_time)<>CONVERT(date,'19000101',112) \
 AND LOWER(s.status) NOT IN(N'dormant',N'preconnect')";

pub(super) const COLUMNS: &str = "1 AS session_context_collected,s.status AS native_session_state,\
 s.client_interface_name,s.host_process_id,s.client_version,\
 CONVERT(varchar(36),r.connection_id) AS request_connection_id";

pub(super) fn decode(
    row: &Rec,
    key: &ActivityKey,
    idle: bool,
    no_current_request: bool,
) -> Result<Option<Box<ActivitySessionContext>>> {
    if value(row, "session_context_collected").is_err() {
        return Ok(None);
    }
    if integer(row, "session_context_collected")? != 1 {
        return Err(invalid_data());
    }
    let native_state = text(row, "native_session_state")?;
    let lifecycle = MssqlSessionContext::lifecycle_from_native_state(&native_state);
    let established = MssqlSessionContext::login_is_established(&key.session.generation, lifecycle);
    let context = ActivitySessionContext {
        lifecycle,
        login_age_ms: if established { age(row, "session_generation")? } else { None },
        last_request_end_age_ms: age(row, "last_request_end")?,
        client_library: optional_text(row, "client_interface_name")?,
        client_process_id: optional_signed_integer(row, "host_process_id")?.map(|native| native.to_string()),
        native: Some(Box::new(SessionNativeContext::Mssql(MssqlSessionContext {
            native_state,
            login_established: established,
            tds_protocol_version: optional_signed_integer(row, "client_version")?.map(|native| native.to_string()),
            has_current_request: key.request.is_some() || !no_current_request,
            request_connection_id: optional_text(row, "request_connection_id")?,
            times: MssqlSessionTimeTokens {
                login_without_offset: key.session.generation.clone(),
                last_request_start_without_offset: optional_text(row, "last_request_start")?
                    .map(|v| native_time(&v))
                    .transpose()?,
                last_request_end_without_offset: optional_text(row, "last_request_end")?
                    .map(|v| native_time(&v))
                    .transpose()?,
            },
        }))),
    };
    if !context.is_valid_for(key, idle) {
        return Err(invalid_data());
    }
    Ok(Some(Box::new(context)))
}

pub(crate) fn established_login(native: &str) -> bool {
    MssqlSessionContext::login_is_established(native, SessionLifecycle::Unknown)
}

pub(super) fn can_identify_session(row: &ActivityObservation) -> bool {
    row.has_session_generation_authority()
}
