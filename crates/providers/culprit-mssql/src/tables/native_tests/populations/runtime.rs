//! One native runtime-positive original; no retry, synthetic status or inference from a control command.
use super::*;
use crate::test_support::{
    owned_captures::{Captured, OwnedCaptures},
    receipt_directory, save_cost_receipt,
};
use culprit_core::{FullTextPopulationReport, SourceState};

struct Original {
    owner: OwnedCaptures,
    capture: Captured,
    report: serde_json::Value,
}

pub(in crate::tables::native_tests) async fn run() {
    let directory = receipt_directory().join("full-text-runtime-positive");
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&directory).expect("fresh private receipt child for one owned FTS runtime case");
    let started = std::time::Instant::now();
    let owned = OwnedSourceFixture::open("culprit-full-text-runtime-fixture").await.unwrap();
    let mut fixture = Fixture::new(&owned);
    let body = AssertUnwindSafe(async {
        fixture.initialize(true).await?;
        fixture.start_full_population().await?;
        let (mut owner, original) = *capture(&mut fixture).boxed().await?;
        // Preserve the sealed original before any positive-state assertion, stop control or later failure.
        save_cost_receipt(
            &directory,
            "runtime-original-before-stop",
            &serde_json::json!({
                "manifest":original.manifest,"original_records":original.records,
                "requested_start_is_not_an_observed_status":true,"native_cost_approved":false,
            }),
        )?;
        let report = serde_json::to_value(report(&original)?).unwrap();
        fixture.stop_original_population().await;
        owner.verify_retained(&original).boxed().await?;
        assert_eq!(serde_json::to_value(report_from_capture(&original)?).unwrap(), report);
        Ok::<_, culprit_core::Error>(Box::new(Original { owner, capture: original, report }))
    })
    .catch_unwind()
    .await;
    // Always settle/retire the original command/capture owners before propagation. No replacement SQL
    // follows uncertainty; started/stopped native background work stays with the nonce supervisor.
    let cleanup = AssertUnwindSafe(fixture.cleanup()).catch_unwind().await;
    let saved = save_cost_receipt(
        &directory,
        "runtime-case",
        &serde_json::json!({
            "body_error":body.as_ref().ok().and_then(|result|result.as_ref().err()),
            "panicked":body.is_err(),"cleanup_error":cleanup.as_ref().ok().and_then(|result|result.as_ref().err()),
            "cleanup_panicked":cleanup.is_err(),"fixture":fixture.runtime_accounting(),
            "canonical_capture_cap":1,"canonical_source_query_cap_including_setup":12,
            "native_queries_after_original_stop_or_cleanup_for_replay":0,
            "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "background_idle_proved":false,"native_cost_approved":false,
        }),
    );
    let mut original = match body {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("one genuine selected native full-population runtime row is required"),
    };
    cleanup.expect("original cleanup owner returned").expect("unsafe fixture SQL remained withheld");
    saved.expect("write the private case receipt after original outcome and settlement");
    original.owner.verify_retained(&original.capture).boxed().await.unwrap();
    assert_eq!(serde_json::to_value(report_from_capture(&original.capture).unwrap()).unwrap(), original.report);
    assert_eq!(original.capture.manifest.source_queries, 10);
    save_cost_receipt(
        &directory,
        "runtime-original-retained",
        &serde_json::json!({
            "successful_runtime_positive":true,"original_manifest":original.capture.manifest,
            "original_records":original.capture.records,"original_report":original.report,
            "original_replay_after_stop_command_and_withheld_ddl_unchanged":true,
            "background_idle_proved":false,"background_cleanup_owner":"nonce-owned disposable supervisor",
            "native_runtime_status_observed_not_inferred_from_start":true,
            "aborted_population_established":false,"native_cost_approved":false,
        }),
    )
    .unwrap();
}

fn report_from_capture(captured: &Captured) -> Result<FullTextPopulationReport> {
    let rows = captured
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.record else {
                return Err(crate::read::invalid_data());
            };
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    culprit_checks::full_text_populations::explain(culprit_checks::full_text_populations::FullTextPopulationEvidence {
        source: captured.source()?,
        observations: &rows,
        read_complete: true,
    })
}
fn report(captured: &Captured) -> Result<FullTextPopulationReport> {
    let report = report_from_capture(captured)?;
    let observations = captured.records.iter().map(|record| &record.record).collect::<Vec<_>>();
    require_positive(&report, &observations)?;
    Ok(report)
}
fn require_positive(report: &FullTextPopulationReport, records: &[&TelemetryRecord]) -> Result<()> {
    if report.source.state != SourceState::Collected || report.observed_rows == 0 || !report.read_complete {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    for record in records {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlFullTextPopulation(point),
            ..
        }) = record
        else {
            return Err(crate::read::invalid_data());
        };
        if point.population_type != Some(1) || !matches!(point.status, Some(3 | 5 | 7)) {
            return Err(CollectionStop::SourceUnavailable.error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_collector::analytical::test_support::{FullTextPopulationCase, full_text_population_evidence};

    #[test]
    fn runtime_positive_needs_actual_full_population_rows_and_qualified_native_status() {
        for case in FullTextPopulationCase::ALL {
            let (source, records) = full_text_population_evidence(case);
            let rows = records
                .iter()
                .map(|record| {
                    let TelemetryRecord::Operational(row) = record else { panic!() };
                    row
                })
                .collect::<Vec<_>>();
            let report = culprit_checks::full_text_populations::explain(
                culprit_checks::full_text_populations::FullTextPopulationEvidence {
                    source: &source,
                    observations: &rows,
                    read_complete: true,
                },
            )
            .unwrap();
            let native = records.iter().collect::<Vec<_>>();
            let expected = matches!(case, FullTextPopulationCase::Processing | FullTextPopulationCase::Stopped);
            assert_eq!(require_positive(&report, &native).is_ok(), expected, "{case:?}");
        }
    }
}
