//! Rebuild refusal only: granted PAGE locks locate the original index; no wait or holder is invented.
use super::*;
use crate::activity::page_targets::rows::HeaderRecord;
use culprit_core::SourceOutcome;
use std::collections::BTreeSet;

// Two identical native observation flows: authority3 + exact PAGE locator1 + headers2 + final authority3.
// The common collection constructor reserves the same client's one SET from this total19 allowance.
const READER_QUERIES: u32 = 18;
const TOTAL_QUERIES: u32 = READER_QUERIES + crate::read::ReadSession::SESSION_SETUP_QUERIES;
// Existing fixture: admin/setup/two gates' SET batches4, ownership1, database1, seed4,
// topology setup/rebuild2, gate begin/rollback4 and positively owned database teardown1.
const FIXTURE_DISPATCH_CAP: u32 = 17;

struct IndexPage {
    context: context::LookupContext,
    address: PageAddress,
    index: LocatedPage,
    first: HeaderRecord,
    last: HeaderRecord,
}

async fn observe(reader: &mut ActivityReader, address: Option<PageAddress>) -> Result<Box<IndexPage>> {
    let initial = Box::pin(context::initial(reader)).await?;
    if initial.unavailable().is_some() || initial.authority.database_id <= 4 {
        return Err(invalid("native rebuild control has no qualified original database"));
    }
    let rows = reader
        .io
        .query(LOCATORS, vec![Parameter::Integer((LOCKED_TARGETS + 1) as i64)], LOCKED_TARGETS + 1)
        .await
        .map_err(ReadError::into_error)?;
    let locators = pages(initial.authority.database_id, &rows)?;
    // Reuse the existing complete locked-tree generation guard; no independent locator interpretation.
    branch::tree(&locators)?;
    let (located_address, index) = locators
        .into_iter()
        .find(|(_, point)| point.table == TREE)
        .ok_or_else(|| invalid("owned original tree locator absent"))?;
    let address = address.unwrap_or(located_address);
    if address.database != initial.authority.database_id {
        return Err(invalid("original page address left its native database"));
    }
    let selected = BTreeSet::from([address]);
    let mut first = reader.page_headers(&selected, &initial).await?;
    let mut last = reader.page_headers(&selected, &initial).await?;
    let checked = Box::pin(context::read(reader, &initial)).await?;
    if first.len() != 1 || last.len() != 1 {
        return Err(invalid("original page endpoints do not retain the exact selected native roster"));
    }
    let first = first.remove(&address).ok_or_else(|| invalid("first original page response missing"))?;
    let last = last.remove(&address).ok_or_else(|| invalid("last original page response missing"))?;
    if !initial.same_source(&checked)
        || !chronological(&[
            &initial.authority.activity.at,
            &index.observed_at,
            &first.observed_at,
            &last.observed_at,
            &checked.authority.activity.at,
        ])
    {
        return Err(invalid("original page/index source or native clock changed"));
    }
    Ok(Box::new(IndexPage { context: checked, address, index, first, last }))
}

fn require_original(point: &IndexPage) -> Result<MappedPageTarget> {
    if !point.address.matches(&point.first.header)?
        || !point.address.matches(&point.last.header)?
        || !point.first.same_mapping(&point.last)
        || point.last.catalog.object_id != Some(point.index.object)
        || point.last.catalog.index_id != Some(point.index.index)
        || point.last.catalog.partition_id.as_ref() != Some(&point.index.partition)
        || point.last.catalog.hobt_id.as_ref() != Some(&point.index.hobt)
    {
        return Err(invalid("original mapped leaf lacks its exact native locked index generation"));
    }
    let target = mapped_target(&point.context, &point.last)?;
    if !target.is_valid() || target.kind != PageWaitTargetKind::IndexLeaf || target.object.is_none() {
        return Err(invalid("native rebuild control needs an actual original mapped index leaf"));
    }
    Ok(target)
}

fn require_rebuilt_index(before: &LocatedPage, after: &LocatedPage) -> Result<()> {
    if before.object != after.object
        || before.index != after.index
        || before.table != TREE
        || after.table != TREE
        || (before.partition == after.partition && before.hobt == after.hobt)
    {
        return Err(invalid("require an observed replacement generation of the same owned index"));
    }
    Ok(())
}

fn require_refused_original(before: &IndexPage, after: &IndexPage) -> Result<()> {
    require_rebuilt_index(&before.index, &after.index)?;
    if before.address != after.address
        || !before.context.same_source(&after.context)
        || !chronological(&[&before.context.authority.activity.at, &after.first.observed_at])
        || before.last.same_mapping(&after.first)
        || before.last.same_mapping(&after.last)
    {
        return Err(invalid("rebuilt native endpoints did not withdraw the original page mapping"));
    }
    Ok(())
}

fn quiet_report(original: &super::super::durable_capture::Captured) -> Result<culprit_core::PageWaitReport> {
    require_readable_capture(original)?;
    let snapshot = original.source()?.observation.snapshot().ok_or_else(|| invalid("original source absent"))?;
    if snapshot.records != 0
        || !original.records.is_empty()
        || snapshot.sources.len() != 1
        || snapshot.sources[0].state != SourceState::Partial
        || snapshot.sources[0].complete_scope
        || original.manifest.source_queries != QUIET_QUERIES
    {
        return Err(invalid("the mapping control cannot manufacture a page waiter or healthy complete population"));
    }
    let source = &snapshot.sources[0];
    culprit_checks::page_waits::explain(
        &[],
        Some(&SourceOutcome { source: source.source.clone(), state: source.state, reason: None, reason_message: None }),
        None,
    )
}

async fn prove(fixture: &mut Fixture) -> Result<()> {
    let setup = format!("USE [{}];{SETUP} USE master;", fixture.database_name());
    tokio::time::timeout(SETUP_TIMEOUT, Box::pin(fixture.execute(&setup)))
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
    Box::pin(fixture.start_holder_gate_with_statement(LOCK_PAGES)).await?;
    let pool = fixture.pool.as_ref().ok_or_else(|| invalid("native fixture pool missing"))?.clone();
    let mut reader = Box::new(ActivityReader::open_collection(
        pool,
        ReadOptions {
            identity: IdentityDetail::Endpoint,
            budget: CollectionBudget {
                max_queries: TOTAL_QUERIES,
                max_snapshot_rows: 4,
                page_rows: 4,
                page_bytes: 64 * 1024,
                total_bytes: 128 * 1024,
                statement_timeout_ms: 1_000,
                total_timeout_ms: 5_000,
                ..Default::default()
            },
        },
        "127.0.0.1".into(),
        fixture.port,
    )?);
    let directory = crate::test_support::receipt_directory();
    let quiet_total_cap = crate::telemetry::recipes()
        .into_iter()
        .find(|recipe| recipe.id == "mssql.page-wait-targets")
        .ok_or_else(|| invalid("the canonical native page source recipe is unavailable"))?
        .budget
        .max_queries;
    let result = AssertUnwindSafe(async {
        let before = Box::pin(observe(&mut reader, None)).await?;
        let target = require_original(&before)?;
        let original_target = serde_json::to_value(&target).unwrap();
        let mut captures = OwnedCaptures::new(fixture)?;
        let quiet = captures.capture(CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }).await?;
        let report = quiet_report(&quiet)?;
        let quiet_context = NativeReadContext::from_snapshot(quiet.source()?)?;
        if !quiet_context.has_qualified_local_owner()
            || quiet_context.target != before.context.authority.activity.database.clone().into()
            || quiet_context.partition != before.context.authority.source_partition()?
            || quiet_context.database_writable != before.context.authority.activity.writable
            || ObservationBounds::new(&before.context.authority.activity.at, &quiet_context.observed_from).is_none()
        {
            return Err(invalid("the original quiet capture has another native owner, role or observation order"));
        }
        crate::test_support::save_cost_receipt(&directory, "page-before-owned-index-rebuild", &serde_json::json!({
            "original_target":target,"quiet_original_manifest":quiet.manifest,"quiet_original_records":quiet.records,
            "quiet_original_report":report,"mapping_is_not_a_wait":true,
        }))?;
        Box::pin(fixture.release_holder_gate()).await?;
        let rebuild = format!("USE [{}];ALTER INDEX PK_page_fixture_topology_tree ON dbo.{TREE} REBUILD WITH(MAXDOP=1);USE master;", fixture.database_name());
        tokio::time::timeout(SETUP_TIMEOUT, Box::pin(fixture.execute(&rebuild)))
            .await
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
        Box::pin(fixture.start_holder_gate_with_statement(LOCK_PAGES)).await?;
        let after = Box::pin(observe(&mut reader, Some(before.address))).await?;
        require_refused_original(&before, &after)?;
        if ObservationBounds::new(&quiet_context.observed_until, &after.first.observed_at).is_none() {
            return Err(invalid("the recorded quiet source and later rebuilt mapping clocks overlap out of order"));
        }
        Box::pin(fixture.release_holder_gate()).await?;
        captures.verify_retained(&quiet).await?;
        assert_eq!(quiet_report(&quiet)?, report);
        assert_eq!(serde_json::to_value(&target).unwrap(), original_target);
        crate::test_support::save_cost_receipt(&directory, "page-after-owned-index-rebuild", &serde_json::json!({
            "original_target":target,"original_partition":before.index.partition,"original_hobt":before.index.hobt,
            "current_partition":after.index.partition,"current_hobt":after.index.hobt,
            "original_mapping_refused_at_both_new_points":true,"original_quiet_capture_and_report_unchanged":true,
            "native_page_wait_proved":false,"native_holder_proved":false,"page_aba_excluded":false,
        }))?;
        Ok::<_,culprit_core::Error>(())
    }).catch_unwind().await;
    let queries = reader.query_count();
    let shutdown = Box::pin(reader.close()).await;
    let saved = crate::test_support::save_cost_receipt(
        &directory,
        "owned-index-rebuild-refusal-outcome",
        &serde_json::json!({
            "body_error":result.as_ref().ok().and_then(|outcome|outcome.as_ref().err()),"panicked":result.is_err(),
            "source_reader_queries":queries,"source_reader_total_allowance_including_setup":TOTAL_QUERIES,
            "reader_metrics":reader.read_metrics(),"reader_shutdown":crate::read::measurement::shutdown_state(shutdown),
        "quiet_canonical_capture_cap":1,"quiet_capture_total_dispatch_cap_including_setup":quiet_total_cap,
        "fixture_dispatch_cap_including_all_session_setup_and_database_cleanup":FIXTURE_DISPATCH_CAP,
        "combined_dispatch_cap":TOTAL_QUERIES + quiet_total_cap + FIXTURE_DISPATCH_CAP,
        "expected_successful_dispatches":TOTAL_QUERIES + QUIET_QUERIES + crate::read::ReadSession::SESSION_SETUP_QUERIES + FIXTURE_DISPATCH_CAP,
        "fixture_added_rows":129,"rebuild_commands_cap":1,"source_cost_qualified":false,
        }),
    );
    match result {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(outcome) => outcome?,
    }
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    if queries != READER_QUERIES {
        return Err(invalid("native rebuild control did not retain its complete fixed reader ledger"));
    }
    saved?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the existing nonce-owned SQL2019+ fixture; actual index rebuild/mapping refusal only, not a page wait or holder"]
async fn local_owned_index_rebuild_refuses_the_original_page_mapping_and_preserves_its_recorded_history() {
    fixture::run(|fixture| Box::pin(prove(fixture))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> LocatedPage {
        pages(9, &super::super::tests::rows()).unwrap().into_values().find(|point| point.table == TREE).unwrap()
    }
    #[test]
    fn rebuild_requires_a_changed_native_generation_of_the_same_owned_index() {
        let before = index();
        let mut after = index();
        after.partition = "72003".into();
        after.hobt = "72003".into();
        assert!(require_rebuilt_index(&before, &after).is_ok());
        for change in ["unchanged", "time", "object", "index", "heap"] {
            let mut after = index();
            match change {
                "time" => after.observed_at = "2026-11-21T01:00:00Z".into(),
                "object" => {
                    after.object += 1;
                    after.hobt = "72003".into();
                }
                "index" => {
                    after.index += 1;
                    after.hobt = "72003".into();
                }
                "heap" => {
                    after.table = HEAP;
                    after.hobt = "72003".into();
                }
                _ => {}
            }
            assert!(require_rebuilt_index(&before, &after).is_err(), "{change}");
        }
    }
    #[test]
    fn unchanged_mapping_or_only_content_changes_do_not_prove_native_mapping_reuse() {
        let original = HeaderRecord::decode(&crate::activity::page_targets::tests::header(0, 42)).unwrap();
        let mut changed = original.clone();
        changed.observed_at = "2026-11-21T01:00:00Z".into();
        changed.header.lsn = Some("different-page-contents".into());
        assert!(original.same_mapping(&changed));
        changed.catalog.hobt_id = Some("72003".into());
        assert!(!original.same_mapping(&changed));
    }
}
