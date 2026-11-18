//! The public summary retains exact rendered limits without private database or execution-owner metadata.
use culprit_core::{Report, telemetry::CONFIGURED_LOG_RATES_SOURCE};
use serde_json::{Value, json};

pub(super) fn attach(response: &mut Value, report: &Report) {
    match culprit_engine::explain_configured_log_rates(&report.snapshot) {
        Ok(Some(rates)) => {
            response["configured_log_rates"] = json!({
                "title":rates.title,"summary":rates.summary,"source_state":rates.source_state,
                "read_complete":rates.read_complete,"native_rows":rates.native_rows,"observed_rows":rates.observed_rows,
                "observed_at":rates.observed_at,"limitations":rates.limitations,
                "entries":rates.entries.iter().take(3).map(|entry|json!({
                    "scope":entry.scope,"title":entry.title,"explanation":entry.explanation,"metric":entry.metric,
                })).collect::<Vec<_>>(),
            });
        }
        Ok(None) if report.snapshot.source_problem(&[CONFIGURED_LOG_RATES_SOURCE]).is_some() => {
            response["configured_log_rates_unavailable"] =
                json!(culprit_core::t!("view.configured_log_rates.unavailable"));
        }
        Ok(None) => {}
        Err(_) => {
            response["configured_log_rates_unavailable"] = json!(culprit_core::t!("view.configured_log_rates.invalid"));
        }
    }
}

#[cfg(test)]
mod tests;
