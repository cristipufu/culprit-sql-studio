use super::*;

fn completed() -> PlanScanCompletion {
    PlanScanCompletion {
        parallel: Some(false),
        threads: vec![PlanScanThread {
            thread: 0,
            executions: Some("1".into()),
            end_of_scans: Some("1".into()),
            rebinds: None,
            rewinds: None,
        }],
    }
}

#[test]
fn exact_native_counts_survive_roundtrip_including_zero_missing_and_large_values() {
    for count in [None, Some("0"), Some("9007199254740993"), Some("18446744073709551615")] {
        let mut observed = completed();
        observed.threads[0].end_of_scans = count.map(str::to_owned);
        assert!(observed.is_valid());
        assert_eq!(
            serde_json::from_value::<PlanScanCompletion>(serde_json::to_value(&observed).unwrap()).unwrap(),
            observed
        );
        assert!(!observed.is_single_completed_serial_scan());
    }
    assert!(completed().is_single_completed_serial_scan());
}

#[test]
fn worker_reexecution_and_rewind_populations_do_not_become_one_completed_input() {
    for case in [
        "parallel",
        "unknown-parallel",
        "worker",
        "missing-eof",
        "unfinished",
        "reexecuted",
        "rebound",
        "rewound",
        "extra-thread",
    ] {
        let mut observed = completed();
        match case {
            "parallel" => observed.parallel = Some(true),
            "unknown-parallel" => observed.parallel = None,
            "worker" => observed.threads[0].thread = 1,
            "missing-eof" => observed.threads[0].end_of_scans = None,
            "unfinished" => observed.threads[0].end_of_scans = Some("0".into()),
            "reexecuted" => observed.threads[0].executions = Some("2".into()),
            "rebound" => observed.threads[0].rebinds = Some("2".into()),
            "rewound" => observed.threads[0].rewinds = Some("1".into()),
            "extra-thread" => {
                let mut worker = observed.threads[0].clone();
                worker.thread = 1;
                observed.threads.push(worker);
            }
            _ => unreachable!(),
        }
        assert!(observed.is_valid(), "{case}");
        assert!(!observed.is_single_completed_serial_scan(), "{case}");
    }
}

#[test]
fn malformed_duplicate_and_unbounded_populations_withhold_the_completion_fact() {
    for count in ["", "-1", "1.0", "01", "NaN", "18446744073709551616"] {
        let mut observed = completed();
        observed.threads[0].end_of_scans = Some(count.into());
        assert!(!observed.is_valid(), "{count}");
        let mut value = serde_json::to_value(crate::PlanNodeFacts::default()).unwrap();
        value["scan_completion"] = serde_json::to_value(observed).unwrap();
        let facts: crate::PlanNodeFacts = serde_json::from_value(value).unwrap();
        assert!(facts.scan_completion.is_none(), "{count}");
    }
    let mut duplicate = completed();
    duplicate.threads.push(duplicate.threads[0].clone());
    assert!(!duplicate.is_valid());
    let mut unbounded = completed();
    let original = completed().threads.remove(0);
    unbounded.threads = (0..=MAX_PLAN_SCAN_THREADS)
        .map(|thread| PlanScanThread { thread: thread as u32, ..original.clone() })
        .collect();
    assert!(!unbounded.is_valid());
}
