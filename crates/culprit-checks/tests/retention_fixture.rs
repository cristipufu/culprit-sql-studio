//! Explicit native retention fixtures shared by unit and integration tests.
use culprit_core::{CleanupHolder, MeasurementScope, OpenTransaction, Snapshot, SourceState, telemetry::*};

pub(crate) fn observation(snapshot: &Snapshot, display: &OpenTransaction) -> RetentionObservation {
    let source = match display.holder {
        CleanupHolder::Session => "pg_stat_activity",
        CleanupHolder::ReplicationSlot => "pg_replication_slots",
        CleanupHolder::PreparedTransaction => "pg_prepared_xacts",
    };
    RetentionObservation {
        source: source.into(),
        identity: NativeIdentity::new(source).qualified("native_id", &display.session),
        observed_at: snapshot.taken_at.clone(),
        scope: if display.holder == CleanupHolder::ReplicationSlot {
            MeasurementScope::Server
        } else {
            MeasurementScope::Database
        },
        holder: display.holder,
        name: display.session.clone(),
        horizons: vec![RetentionHorizon {
            protects: if display.catalog_only {
                vec![CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
            } else {
                vec![CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
            },
            native_name: if display.catalog_only { "catalog_xmin" } else { "xmin" }.into(),
            native_value: "100".into(),
            transactions_behind: Some((display.xid_age.unwrap_or(0.0) as u64).to_string()),
        }],
        session: (display.holder == CleanupHolder::Session).then(|| NativeActivityIdentity {
            id: display.session.clone(),
            generation: "2026-01-01T00:00:00.000001Z".into(),
        }),
        unresolved_session_id: None,
        transaction: None,
        query: None,
        client: Some(ActivityClient { application: display.program.clone(), user: None, host: None }),
        activity: display.activity,
        native_state: display.state.clone(),
        age_seconds: Some(display.seconds),
        database: display.database.clone(),
        code_ownership: PayloadUnavailable::NotCollected,
    }
}
pub(crate) fn install(snapshot: &mut Snapshot, display: OpenTransaction) {
    let holder = observation(snapshot, &display);
    let sources = vec![RetentionSourceCoverage { source: holder.source.clone(), state: SourceState::Collected }];
    let pressure = snapshot.pressure.get_or_insert_with(Default::default);
    pressure.retention = Some(RetentionSnapshot {
        coverage: RetentionCoverage {
            native_sources: vec![holder.source.clone()],
            qualified_horizons: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
        },
        holders: vec![holder],
        sources,
    });
    pressure.oldest_transaction = Some(display);
}
