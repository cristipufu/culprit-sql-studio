//! Synthetic native Query Store reads share one logical namespace and retain independent immutable receipts.
use super::*;

pub fn compilation_interval_evidence() -> [(CollectionDescription, Vec<TelemetryRecord>); 2] {
    [
        point("2020-01-01T12:00:00Z", "2020-01-01T12:00:01Z", "9007199254740993", "2020-01-01T11:59:00Z"),
        point("2020-01-01T12:01:00Z", "2020-01-01T12:01:01Z", "9007199254740994", "2020-01-01T12:00:30Z"),
    ]
}

fn point(from: &str, until: &str, count: &str, last: &str) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = query_compilation_evidence();
    let CollectionTarget::Database(database) = &mut source.target else { panic!("synthetic native database") };
    database.incarnation = Some("synthetic-query-store-logical-database".into());
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!("compilation snapshot") };
    snapshot.observed_from = from.into();
    snapshot.observed_until = until.into();
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlQueryCompilation(native),
        ..
    }) = &mut rows[0]
    else {
        panic!("native compilation counters")
    };
    native.compilations = count.into();
    native.last_compile_at = last.into();
    (source, rows)
}
