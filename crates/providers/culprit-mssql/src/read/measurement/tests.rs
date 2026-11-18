use super::*;
use crate::value::Val;

#[test]
fn only_an_active_sample_of_the_exact_template_establishes_cancellation_observation() {
    let sql = "SELECT /* culprit */ database_id FROM sys.databases";
    let watch = TemplateObservation { id: digest(sql.as_bytes()), observed: Arc::new(AtomicBool::new(false)) };
    watch.observe(sql, false);
    watch.observe("SELECT /* culprit */ name FROM sys.databases", true);
    assert!(!watch.was_active());
    watch.observe(sql, true);
    assert!(watch.was_active());
}

#[test]
fn native_deltas_keep_integer_precision_and_refuse_regressing_lifetimes() {
    let before = Counters { cpu: 2, logical: 9_007_199_254_740_993, physical: 3, writes: 4 };
    let after = Counters { cpu: 2, logical: 9_007_199_254_740_994, physical: 4, writes: 4 };
    let delta = before.delta(&after).unwrap();
    assert_eq!(delta["logical_reads"], "1");
    assert_eq!(delta["physical_reads"], "1");
    assert_eq!(delta["cpu_ms"], "0");
    assert!(delta["cpu_resolution"].as_str().unwrap().contains("not a zero-cost"));
    assert!(after.delta(&before).is_err());
}

#[test]
fn changed_transport_ownership_cannot_authorize_even_plausible_native_counter_deltas() {
    let before = Counters { cpu: 1, logical: 10, physical: 1, writes: 0 };
    let after = Counters { cpu: 2, logical: 12, physical: 1, writes: 0 };
    assert_eq!(qualified_delta(&before, &after, 1, 1).unwrap()["logical_reads"], "2");
    let changed = qualified_delta(&before, &after, 1, 2).unwrap_err();
    assert_eq!(changed.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
}

#[test]
fn missing_negative_or_inexact_native_counters_cannot_become_zero_cost_samples() {
    for value in [Val::Null, Val::Int(-1), Val::Float(1.0)] {
        let row = Rec::of(&[
            ("cpu_ms", value),
            ("logical_reads", Val::Int(0)),
            ("reads", Val::Int(0)),
            ("writes", Val::Int(0)),
        ]);
        assert!(Counters::read(&row).is_err());
    }
    let zero = Rec::of(&[
        ("cpu_ms", Val::Int(0)),
        ("logical_reads", Val::Int(0)),
        ("reads", Val::Int(0)),
        ("writes", Val::Int(0)),
    ]);
    assert_eq!(Counters::read(&zero).unwrap().cpu, 0);
}

#[test]
fn an_unobserved_grant_is_unknown_and_a_wait_sentinel_does_not_identify_a_blocker() {
    let mut phase = Phase {
        sql: "SELECT 1".into(),
        parameters: json!([]),
        row_limit: 1,
        before: Counters { cpu: 0, logical: 0, physical: 0, writes: 0 },
        started: Instant::now(),
        active_samples: 0,
        waits: Default::default(),
        blocking_samples: 0,
        maximum_sampled_granted_kb: None,
        maximum_sampled_requested_kb: None,
        maximum_sampled_used_kb: None,
        observer_error: None,
    };
    phase.observe(&Sample {
        counters: phase.before.clone(),
        active: true,
        wait: Some("PAGELATCH_EX".into()),
        blocker: Some(-5),
        granted_kb: None,
        requested_kb: None,
        used_kb: None,
    });
    assert_eq!(phase.blocking_samples, 0);
    assert_eq!(phase.maximum_sampled_granted_kb, None);
    phase.observe(&Sample {
        counters: phase.before.clone(),
        active: true,
        wait: Some("RESOURCE_SEMAPHORE".into()),
        blocker: None,
        granted_kb: Some(0),
        requested_kb: Some(1024),
        used_kb: Some(0),
    });
    assert_eq!(phase.maximum_sampled_granted_kb, Some(0));
    assert_eq!(phase.maximum_sampled_requested_kb, Some(1024));
}
