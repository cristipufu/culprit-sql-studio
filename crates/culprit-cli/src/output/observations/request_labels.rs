//! One escaped, bounded rendering of native client labels; these labels do not establish service ownership.
use super::statement;
use culprit_core::{t, telemetry::ActivityClient};

pub(super) fn client(client: Option<&ActivityClient>) -> String {
    t!(
        "evidence.cli.page_writers.client",
        application = optional(client.and_then(|client| client.application.as_deref())),
        user = optional(client.and_then(|client| client.user.as_deref())),
        host = optional(client.and_then(|client| client.host.as_deref()))
    )
}

pub(super) fn optional(value: Option<&str>) -> String {
    value.map_or_else(|| t!("evidence.cli.page_holders.not_recorded"), statement::preview)
}
