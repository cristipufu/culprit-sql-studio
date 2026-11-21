//! Bounded native context only: no raw source/endpoint, SQL, manifest or private authentication material.
use super::*;
use culprit_core::telemetry::digest;
use culprit_engine::LocalSessionCensus;

const ENTRY_LIMIT: usize = 5;

pub(super) fn captured(analysis: &LocalSessionCensus) -> Result<Value> {
    let report = analysis.report();
    let original = analysis.original();
    let reference = digest(&serde_json::to_vec(original.capture()).map_err(|_| DeliveryProblem::Integrity.error())?);
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "reference":entry.reference.as_ref().map(|row|digest(row.record_id.as_bytes())),
                "session":entry.activity.session.id,
                "request":entry.activity.request.as_ref().map(|request|request.id.as_str()),
                "scope":entry.scope,"idle":entry.idle,"title":entry.title,"state":entry.state,
                "fields":entry.fields,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "session_census":{
            "title":report.title,"summary":report.summary,"limitations":report.limitations,
            "historical_context":true,"reference":reference,
            "logical_database":report.context.target.database().map(|database|database.database.as_str()),
            "native_read_bounds":{"from":report.context.observed_from,"until":report.context.observed_until},
            "native_observed_at":report.observed_at,"scope":report.scope,
            "database_writable":report.context.database_writable,
            "qualified_local_owner":report.context.has_qualified_local_owner(),
            "native_population":report.population,"source":report.native_source,
            "sources":report.sources,"read_complete":report.read_complete,
            "native_rows":report.native_rows,"observed_rows":report.observed_rows,
            "observed_sessions":report.observed_sessions,"observed_current_requests":report.observed_requests,
            "entries_available":report.entries.len(),"entries_presented":entries.len(),
            "entries_omitted":report.entries.len().saturating_sub(entries.len()),
            "entries_limited":report.entries_limited,"entries":entries,
            "client_groups":{
                "title":report.client_groups.title,"summary":report.client_groups.summary,
                "limitations":report.client_groups.limitations,
                "grouped_sessions":report.client_groups.grouped_sessions,
                "ungrouped_sessions":report.client_groups.ungrouped_sessions,
                "groups_available":report.client_groups.available_groups,
                "entries_limited":report.client_groups.entries_limited,
                "groups_presented":report.client_groups.entries.len().min(ENTRY_LIMIT),
                "groups_omitted":report.client_groups.available_groups as usize
                    - report.client_groups.entries.len().min(ENTRY_LIMIT),
                "entries":report.client_groups.entries.iter().take(ENTRY_LIMIT).map(|group|json!({
                    "observed_sessions":group.observed_sessions,"observed_current_requests":group.observed_requests,
                    "fields":group.fields,"limitations":group.limitations,
                })).collect::<Vec<_>>(),
            },
        },
        "collected_from":original.collected_from(),"collected_until":original.collected_until(),
        "local_verification_bytes":original.bytes_read(),
    }))
}
