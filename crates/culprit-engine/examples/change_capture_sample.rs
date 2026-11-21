//! Controlled presentation cases through the canonical native-history interpreters, never native qualification.
use culprit_collector::analytical::test_support::{
    CdcCase, ChangeFeedCase, cdc_evidence, change_capture_multiple_errors, change_feed_evidence,
};
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeMap, path::PathBuf};
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut cases = BTreeMap::new();
    for (name, case) in [
        ("cdc_exact", CdcCase::ErrorAndProgress),
        ("cdc_empty", CdcCase::NoRetainedErrors),
        ("cdc_disabled", CdcCase::Disabled),
        ("cdc_denied", CdcCase::PermissionDenied),
        ("cdc_partial", CdcCase::PartialErrors),
    ] {
        cases.insert(name, explain(MssqlChangeCaptureFamily::Cdc, cdc_evidence(case))?);
    }
    for (name, case) in [
        ("change_feed_exact", ChangeFeedCase::ProgressAndError),
        ("change_feed_empty", ChangeFeedCase::Empty),
        ("change_feed_denied", ChangeFeedCase::PermissionDenied),
        ("change_feed_partial", ChangeFeedCase::PartialErrors),
        ("change_feed_null", ChangeFeedCase::NativeNulls),
        ("change_feed_unknown", ChangeFeedCase::UnknownTask),
    ] {
        cases.insert(name, explain(MssqlChangeCaptureFamily::ChangeFeed, change_feed_evidence(case))?);
    }
    for (name, family) in
        [("cdc_many", MssqlChangeCaptureFamily::Cdc), ("change_feed_many", MssqlChangeCaptureFamily::ChangeFeed)]
    {
        cases.insert(name, explain(family, change_capture_multiple_errors(family, 13))?);
    }
    let reason = culprit_core::message!("view.change_capture.history.sharing_withheld");
    cases.insert("withdrawn",serde_json::json!({"marker":RecordedOperationalRead::Unavailable {reason:reason.clone()},"report":culprit_checks::cdc::unavailable(&reason)}));
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/change-capture.sample.json"));
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
fn explain(
    family: MssqlChangeCaptureFamily,
    (source, rows): (CollectionDescription, Vec<TelemetryRecord>),
) -> Result<serde_json::Value> {
    let rows = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    let report = match family {
        MssqlChangeCaptureFamily::Cdc => {
            serde_json::to_value(culprit_checks::cdc::explain(culprit_checks::cdc::CdcEvidence {
                source: &source,
                observations: &rows,
                read_complete: true,
            })?)
        }
        MssqlChangeCaptureFamily::ChangeFeed => serde_json::to_value(culprit_checks::change_feed::explain(
            culprit_checks::change_feed::ChangeFeedEvidence {
                source: &source,
                observations: &rows,
                read_complete: true,
            },
        )?),
    }
    .map_err(|_| DeliveryProblem::Integrity.error())?;
    Ok(serde_json::json!({"source":source,"report":report}))
}
