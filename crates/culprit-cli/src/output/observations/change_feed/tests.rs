use super::*;
use culprit_collector::analytical::test_support::{ChangeFeedCase, change_feed_evidence};
use culprit_core::{t, telemetry::*};
fn report(case: ChangeFeedCase) -> ChangeFeedReport {
    let (source, rows) = change_feed_evidence(case);
    let rows = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { unreachable!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::change_feed::explain(culprit_checks::change_feed::ChangeFeedEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn change_feed_text_preserves_canonical_native_context_actions_unknowns_and_no_delivery_verdict() {
    for case in ChangeFeedCase::ALL {
        let r = report(case);
        let text = change_feed(&r);
        assert!(text.contains(&r.summary));
        for limitation in &r.limitations {
            assert!(text.contains(limitation));
        }
        for e in r.entries.iter().take(3) {
            assert!(text.contains(&e.title) && text.contains(&e.investigation) && text.contains(&e.verify));
        }
        for private in
            ["error_message", "00000016:00000058:0001", "original-instance", "original.invalid", "source_sha256"]
        {
            assert!(!text.contains(private), "{case:?}:{private}");
        }
        assert!(text.contains(&t!("view.change_feed.limit.delivery")));
    }
}
#[test]
fn change_feed_native_fields_are_escaped_and_fixed_entry_field_and_text_budgets_hold() {
    let mut r = report(ChangeFeedCase::ProgressAndError);
    let first = &mut r.entries[0];
    first.fields[0].value = Some(format!("\x1b[31m\n{}", "x".repeat(4096)));
    let extra = first.fields[0].clone();
    first.fields.extend(vec![extra; 100]);
    let text = change_feed(&r);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains(&"x".repeat(141)));
    assert!(text.len() < 32768);
}

#[test]
fn withdrawn_change_feed_has_no_fabricated_read_window_or_healthy_empty() {
    let r = culprit_checks::change_feed::unavailable(&culprit_core::message!(
        "view.change_capture.history.sharing_withheld"
    ));
    assert!(r.context.is_none());
    let text = change_feed(&r);
    assert!(text.contains(&r.summary));
    assert!(!text.contains("2020-01-01"));
    assert!(r.entries.is_empty() && !r.read_complete);
}
