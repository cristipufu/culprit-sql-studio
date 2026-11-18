//! Controlled original grant inputs and the actual shared interpretation, kept together for consumer tests.
//! This is no live source, native-cost qualification, current business owner or exclusive-cause proof.
use culprit_collector::analytical::test_support::{GrantOutputCase, grant_output_evidence};
use culprit_core::{SourceOutcome, telemetry::*};
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let mut cases = BTreeMap::new();
    for case in GrantOutputCase::ALL {
        let (source, records) = grant_output_evidence(case);
        let capture = CaptureIdentity { observation_id: "controlled-grant-output".into(), revision: None };
        let observations = records
            .into_iter()
            .map(|record| {
                let record = ExportedRecord::new(&source, &capture, record)?;
                match record.record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::ExecutionGrant(grant),
                        ..
                    }) => Ok(*grant),
                    _ => Err(DeliveryProblem::Integrity.error()),
                }
            })
            .collect::<culprit_core::Result<Vec<_>>>()?;
        let native = source.observation.snapshot().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let coverage = &native.sources[0];
        let context = NativeReadContext::from_snapshot(&source)?;
        let outcome = SourceOutcome {
            source: coverage.source.clone(),
            state: coverage.state,
            reason: None,
            reason_message: None,
        };
        let rows = observations.iter().collect::<Vec<_>>();
        let report = culprit_checks::request_memory::explain(
            culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &rows },
            Some(&outcome),
        )?;
        let evidence = RequestMemorySnapshot {
            context,
            source: coverage.source.clone(),
            observed_at: native.observed_from.clone(),
            observations,
        };
        cases.insert(case.key(), serde_json::json!({"evidence":evidence,"report":report}));
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/memory-participants.sample.json")
    });
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
