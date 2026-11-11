use super::Collected;
use culprit_core::{Coverage, ForcedPlan, QueryHint, TuningAdvice, TuningRecommendation, telemetry::*};

impl Collected {
    pub(crate) fn finish(
        self,
        coverage: &mut Coverage,
        grant_hint: &str,
        shutdown: SourceShutdown,
        query_store_readable: bool,
    ) -> Option<TuningAdvice> {
        let mut advice = TuningAdvice::default();
        let mut successful = 0;
        let mut failed = 0;
        let mut partial = false;
        for (kind, result) in self.parts {
            let result = if shutdown == SourceShutdown::Unconfirmed {
                Err(CollectionStop::CancellationUnconfirmed.error())
            } else {
                result
            };
            let result = result.map_err(|error| {
                if error.code == culprit_core::ErrorCode::PermissionDenied && error.hint.is_none() {
                    error.with_hint(grant_hint)
                } else {
                    error
                }
            });
            match result {
                Ok(None) => coverage
                    .not_applicable(kind.source(), culprit_core::message!("provider.mssql.tuning.not_applicable")),
                Ok(Some(facts)) => {
                    successful += 1;
                    let facts = coverage.read_rows(kind.source(), Ok(facts)).unwrap_or_default();
                    if facts
                        .iter()
                        .any(|fact| matches!(fact,TuningFact::QueryHint(value) if !value.hint.is_available() || value.scope==culprit_core::QueryStoreHintScope::Unknown))
                    {
                        coverage.partial(kind.source(), culprit_core::message!("provider.mssql.tuning.partial"));
                        partial = true;
                    }
                    for fact in facts {
                        // Configured forcing/hints remain collectible when Query Store is OFF. The app grades only
                        // its readable workload; background telemetry retains the native configuration independently.
                        if query_store_readable || !matches!(fact, TuningFact::ForcedPlan(_) | TuningFact::QueryHint(_))
                        {
                            append_application(&mut advice, fact);
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    coverage.source_failed(kind.source(), &error);
                }
            }
        }
        if failed > 0 || partial {
            coverage.partial(&crate::sources::TUNING, culprit_core::message!("provider.mssql.tuning.partial"));
        } else {
            coverage.read(&crate::sources::TUNING, Ok(()));
        }
        (successful > 0 || failed == 0).then_some(advice)
    }
}

pub(super) fn append_application(advice: &mut TuningAdvice, fact: TuningFact) {
    match fact {
        TuningFact::AutomaticOption(value) => advice.automatic_tuning.push(value),
        TuningFact::Recommendation(value) => {
            let gain = match (value.regressed.avg_cpu_us, value.recommended.avg_cpu_us) {
                (Some(bad), Some(good)) if bad > 0.0 => Some(((bad - good) / bad).clamp(0.0, 1.0)),
                _ => None,
            };
            let error_prone = match (&value.regressed.aborted, &value.recommended.aborted) {
                (Some(bad), Some(good)) => {
                    bad.parse::<u64>().ok().zip(good.parse::<u64>().ok()).is_some_and(|(bad, good)| bad > good)
                }
                _ => false,
            };
            advice.recommendations.push(TuningRecommendation {
                name: value.name,
                kind: value.kind,
                reason: value.reason,
                state: value.state,
                query_id: value.query_id,
                regressed_plan_id: value.regressed.plan_id,
                recommended_plan_id: value.recommended.plan_id,
                estimated_gain: gain,
                error_prone,
                script: payload_text(value.script),
                valid_since: value.valid_since_utc_estimate,
                last_execution: value.last_execution_at,
                score: value.score.map(f64::from),
                applied_at: value.applied_utc_estimate,
                index: value.index,
            });
        }
        TuningFact::ForcedPlan(value) => advice.forced_plans.push(ForcedPlan {
            query_id: value.query_id,
            plan_id: value.plan_id,
            failures: value.failures.parse::<u64>().expect("validated native count") as f64,
            last_failure_reason: meaningful(value.last_failure_reason),
            last_execution: value.last_execution_at,
            forcing: meaningful(value.forcing),
        }),
        TuningFact::QueryHint(value) => {
            if let Some(hint) = payload_text(value.hint) {
                advice.hints.push(QueryHint {
                    query_id: value.query_id,
                    hint_id: value.hint_id,
                    scope: value.scope,
                    hint,
                    failures: value.failures.parse::<u64>().expect("validated native count") as f64,
                    last_failure_reason: meaningful(value.last_failure_reason),
                });
            }
        }
    }
}
fn meaningful(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty() && v != "NONE")
}
fn payload_text(payload: DefinitionPayload) -> Option<String> {
    match payload {
        DefinitionPayload::Available { text, .. } => Some(text),
        DefinitionPayload::Unavailable { .. } => None,
    }
}
