//! One bounded escaped SQL excerpt and native-unavailability wording for observation output.
use culprit_core::{
    t,
    telemetry::{DefinitionPayload, PayloadUnavailable},
};

const SQL_PREVIEW_CHARS: usize = 100;

pub(super) fn sql(payload: Option<&DefinitionPayload>) -> String {
    match payload {
        Some(DefinitionPayload::Available { text, .. }) => {
            t!("evidence.cli.page_holders.sql", sql = preview(text))
        }
        Some(DefinitionPayload::Unavailable { reason }) => match reason {
            PayloadUnavailable::Missing => t!("evidence.cli.page_holders.sql_missing"),
            PayloadUnavailable::Restricted => t!("evidence.cli.page_holders.sql_restricted"),
            PayloadUnavailable::Encrypted => t!("evidence.cli.page_holders.sql_encrypted"),
            PayloadUnavailable::Oversized => t!("evidence.cli.page_holders.sql_oversized"),
            PayloadUnavailable::RedactionFailed => t!("evidence.cli.page_holders.sql_redaction_failed"),
            PayloadUnavailable::NotCollected => t!("evidence.cli.page_holders.sql_not_collected"),
            PayloadUnavailable::SourceUnavailable => t!("evidence.cli.page_holders.sql_source_unavailable"),
        },
        None => t!("evidence.cli.page_holders.sql_not_retained"),
    }
}

pub(super) fn preview(text: &str) -> String {
    let mut characters = text.chars().flat_map(char::escape_debug);
    let mut preview = characters.by_ref().take(SQL_PREVIEW_CHARS).collect::<String>();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}
