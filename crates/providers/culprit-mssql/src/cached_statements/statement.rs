//! A complete native batch prefix can supply only its own UTF-16 statement slice, never a hash representative.
use crate::query_store::payload::Payload;
use culprit_core::{
    Engine,
    telemetry::{CachedStatementContext, DefinitionPayload, PayloadUnavailable},
};

pub(crate) fn statement_from_prefix(context: &CachedStatementContext, prefix: Option<&str>) -> DefinitionPayload {
    match native_statement_from_prefix(context, prefix) {
        Payload::Text(statement) => DefinitionPayload::sql(&statement, Engine::Mssql),
        Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason },
    }
}

/// The same slice remains private while binding the original selected statement to native plan XML.
pub(crate) fn native_statement_from_prefix(context: &CachedStatementContext, prefix: Option<&str>) -> Payload {
    let unavailable = Payload::Unavailable;
    if !context.identity.has_sql_handle() || !context.identity.has_plan_handle() {
        return unavailable(PayloadUnavailable::SourceUnavailable);
    }
    let Some(prefix) = prefix else { return unavailable(PayloadUnavailable::Missing) };
    if !context.source_text_complete {
        return unavailable(PayloadUnavailable::NotCollected);
    }
    let utf16 = prefix.encode_utf16().collect::<Vec<_>>();
    if context.source_text_bytes.as_deref().and_then(|bytes| bytes.parse::<usize>().ok()) != Some(utf16.len() * 2) {
        return unavailable(PayloadUnavailable::SourceUnavailable);
    }
    let start = context.identity.statement_start_bytes as usize / 2;
    let end = match context.identity.statement_end_bytes {
        -1 => utf16.len(),
        offset if offset >= 0 && offset as usize <= utf16.len() * 2 => (offset as usize / 2 + 1).min(utf16.len()),
        _ => return unavailable(PayloadUnavailable::SourceUnavailable),
    };
    let Some(selected) = utf16.get(start..end) else { return unavailable(PayloadUnavailable::SourceUnavailable) };
    let Ok(statement) = String::from_utf16(selected) else { return unavailable(PayloadUnavailable::SourceUnavailable) };
    if statement.trim().is_empty() { unavailable(PayloadUnavailable::Missing) } else { Payload::Text(statement) }
}
