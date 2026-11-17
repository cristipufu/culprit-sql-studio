//! Scan, saved-file and agent views use one presence/source policy over the original retained workload.
use super::*;

pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<PageServerReadReport>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Ok(None) };
    if !workload.queries.iter().flat_map(|query| &query.plans).any(|plan| plan.page_server_reads.is_some()) {
        return Ok(None);
    }
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::QUERY_STORE);
    explain_workload(workload, source).map(Some)
}
