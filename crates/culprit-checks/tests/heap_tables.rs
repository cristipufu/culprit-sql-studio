//! A heap costs the extra reads it causes, not its size: an insert-only log heap costs nothing. Medium needs forwarded
//! reads measured as a rate (at least an hour of counters), at 100K a day.

use culprit_checks::analyze;
use culprit_core::{Severity, Snapshot};

fn snapshot(forwarded: Option<f64>, counter_hours: f64) -> (Snapshot, String) {
    let text = std::fs::read_to_string(format!("{}/../../fixtures/mssql-demo.json", env!("CARGO_MANIFEST_DIR")))
        .expect("fixture");
    let mut s: Snapshot = serde_json::from_str(&text).expect("snapshot");
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(counter_hours);
    let t = s.tables.first_mut().expect("a table");
    t.is_heap = Some(true);
    t.rows = Some(10_000_000.0);
    t.forwarded_fetches = forwarded;
    let id = format!("heap-table:{}", t.qualified());
    (s, id)
}

fn severity(forwarded: Option<f64>, counter_hours: f64) -> Option<Severity> {
    let (s, id) = snapshot(forwarded, counter_hours);
    analyze(s).findings.into_iter().find(|f| f.id == id).map(|f| f.severity)
}

#[test]
fn a_big_heap_without_forwarded_reads_is_low() {
    assert_eq!(severity(None, 240.0), Some(Severity::Low));
}

#[test]
fn measured_forwarded_reads_make_it_medium() {
    // 5M over ten days: 500K a day.
    assert_eq!(severity(Some(5_000_000.0), 240.0), Some(Severity::Medium));
}

#[test]
fn a_total_from_half_an_hour_of_counters_is_no_rate() {
    assert_eq!(severity(Some(5_000_000.0), 0.5), Some(Severity::Low));
}

// What the finding says, word for word, with and without forwarded reads: its messages come from locales/en.json.
#[test]
fn a_heap_says_what_it_costs_in_english() {
    let (s, id) = snapshot(Some(5_000_000.0), 240.0);
    let f = analyze(s).findings.into_iter().find(|f| f.id == id).expect("a heap finding");
    assert_eq!(f.title, "Reads of customers follow forwarding pointers about 500K times a day");
    assert_eq!(
        f.summary,
        "shop.customers is stored as a heap. Updates that grow a row leave forwarding pointers behind, deleted space \
         isn't reclaimed well, and every nonclustered index lookup has to go through a row identifier. Reads have \
         followed a forwarding pointer to another page 5.0M times in the 10 days since the server last restarted (about \
         500K a day): each one is an extra random read. It does have a primary key, just not a clustered one."
    );
    assert_eq!(f.impact.label, "10.0M rows");
    assert_eq!(
        f.metrics.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
        ["Forwarded reads per day", "Rows", "Size"]
    );
    let fix = f.fix.expect("a fix");
    assert!(fix.summary.starts_with("Add a clustered index, usually on the primary key"), "{}", fix.summary);
    assert!(fix.cost.is_some_and(|c| c.starts_with("Rewrites about ") && c.ends_with(" of data.")));
    assert_eq!(f.caveats, ["Staging tables that are bulk-loaded and truncated can be fine as heaps."]);

    let (s, id) = snapshot(None, 240.0);
    let f = analyze(s).findings.into_iter().find(|f| f.id == id).expect("a heap finding");
    assert_eq!(f.title, "customers (10.0M rows) has no clustered index");
    assert!(
        f.summary.ends_with("row identifier. It does have a primary key, just not a clustered one."),
        "{}",
        f.summary
    );
}
