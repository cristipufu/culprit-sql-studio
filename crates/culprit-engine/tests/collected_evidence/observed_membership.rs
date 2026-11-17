use super::*;
use culprit_collector::analytical::test_support::query_population_evidence;
use culprit_core::{RuntimePopulationMembership, collected_analysis::CollectedQueryPopulation};

#[tokio::test]
async fn physically_complete_archived_observed_groups_keep_exact_denominators_but_never_clean_absence() {
    for observed in [false, true] {
        let (mut source, records) = query_population_evidence();
        if observed {
            let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("native history") };
            history.population_catalog.as_mut().unwrap().membership = RuntimePopulationMembership::ObservedWindow {
                from: history.interval.from.clone(),
                until: history.interval.until.clone(),
            };
        }
        let original = source.clone();
        let fixture = Fixture::new(source, records.clone()).await;
        let capture = fixture.read().await;
        assert!(capture.is_read_complete() && capture.reference_coverage().complete);
        let analysis = collected_evidence::analyze(&capture, None).unwrap();
        assert_eq!(analysis.context.source, original);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(
            analysis
                .context
                .records
                .iter()
                .map(|record| serde_json::to_value(&record.row).unwrap())
                .collect::<Vec<_>>(),
            capture.records().iter().map(|record| serde_json::to_value(record.row()).unwrap()).collect::<Vec<_>>(),
            "original exported body, semantic IDs and native references remain unchanged",
        );
        assert_eq!(analysis.analyses.len(), 2);
        for result in &analysis.analyses {
            let CollectedQueryPopulation::Population { identity } = &result.population else {
                panic!("selected native group")
            };
            let workload = result.workload.as_ref().unwrap();
            assert_eq!(workload.population.as_ref(), Some(identity));
            assert_eq!(workload.kind, None, "no provider identity is invented for generic replay");
            assert!(
                workload.population_catalog.is_none(),
                "original native metadata remains in the one source context"
            );
            assert_eq!(workload.queries.len(), 1);
            let (executions, time, cpu) = match identity.id.as_str() {
                "history-group-z" => (2.0, 20.0, Some(10.0)),
                "history-group-a" => (5.0, 500.0, Some(250.0)),
                _ => panic!("unreturned group"),
            };
            assert_eq!(
                (workload.total_executions, workload.total_time_ms, workload.total_cpu_ms),
                (executions, time, cpu)
            );
            assert_eq!(workload.queries[0].executions, executions);
            let assessment = result.assessments.iter().find(|assessment| assessment.check == "heavy-query").unwrap();
            assert_eq!(assessment.reason.is_some(), observed, "membership qualification survives the same pure checks");
            if observed {
                assert!(matches!(assessment.status, CheckStatus::InsufficientEvidence | CheckStatus::Findings));
                assert!(
                    assessment.assessed_queries.is_empty(),
                    "an overall membership gap is not a per-query clean result"
                );
            }
            let selected = collected_evidence::analyze(&capture, Some(identity)).unwrap();
            assert_eq!(
                serde_json::to_value(&selected.analyses[0].workload).unwrap(),
                serde_json::to_value(&result.workload).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&selected.analyses[0].query_inputs).unwrap(),
                serde_json::to_value(&result.query_inputs).unwrap()
            );
        }
        assert_eq!(
            analysis.context.metrics.queries,
            capture.metrics().queries,
            "analysis does not reopen the native source"
        );
    }
}
