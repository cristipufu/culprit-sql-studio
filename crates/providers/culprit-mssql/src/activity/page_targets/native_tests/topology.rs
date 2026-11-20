//! Native mapping controls only. Granted PAGE locks locate owned pages; they establish no latch wait or holder.
use super::{
    durable_capture::{OwnedCaptures, require_readable_capture},
    fixture::{self, Fixture, invalid},
};
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    read::{Parameter, ReadError, integer, text},
    value::Rec,
};
use culprit_core::{Result, SourceState, telemetry::*};
use futures_util::FutureExt;
use std::{collections::BTreeMap, panic::AssertUnwindSafe, time::Duration};

use super::super::{chronological, context, mapped_target, rows::PageAddress};

const TREE: &str = "page_fixture_topology_tree";
const HEAP: &str = "page_fixture_topology_heap";
const TARGETS: usize = 3;
const HEADER_QUERIES: u32 = 9;
// Boxed authority 3, population 1, capture-end authority 3 and frozen final verification 3.
// A capture-end point does not replace the delivery owner's independent final verification.
const QUIET_QUERIES: u32 = 10;
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

// Fixed 129 inserted rows, below 0.2MiB of row payload. This SQL exists only in the nonce-owned fixture.
const SETUP: &str = r#"/* culprit fixture topology setup only */
CREATE TABLE dbo.page_fixture_topology_tree(id int NOT NULL CONSTRAINT PK_page_fixture_topology_tree PRIMARY KEY CLUSTERED,payload binary(1000) NOT NULL);
WITH digit(v) AS(SELECT v FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))d(v)),
 seed AS(SELECT TOP(128) ROW_NUMBER() OVER(ORDER BY a.v,b.v,c.v) AS id FROM digit a CROSS JOIN digit b CROSS JOIN digit c)
INSERT dbo.page_fixture_topology_tree SELECT CONVERT(int,id),CONVERT(binary(1000),0x01) FROM seed;
IF @@ROWCOUNT<>128 THROW 51000,'owned tree seed quota incomplete',1;
CREATE TABLE dbo.page_fixture_topology_heap(id int NOT NULL,payload binary(1000) NOT NULL);
INSERT dbo.page_fixture_topology_heap VALUES(1,CONVERT(binary(1000),0x01));"#;

const LOCK_PAGES: &str = r#"/* culprit fixture PAGE locks are locators, never latch owners */
SET LOCK_TIMEOUT 1000;BEGIN TRANSACTION;DECLARE @owned int;
SELECT @owned=id FROM dbo.page_fixture_topology_tree WITH(PAGLOCK,XLOCK,HOLDLOCK,FORCESEEK) WHERE id IN(1,128);
IF @@ROWCOUNT<>2 THROW 51000,'owned endpoint key quota incomplete',1;
-- The unindexed heap needs no SERIALIZABLE range protection. XLOCK alone retains its
-- exclusive page lock until the owned transaction ends.
SELECT @owned=id FROM dbo.page_fixture_topology_heap WITH(PAGLOCK,XLOCK) WHERE id=1;
IF @@ROWCOUNT<>1 THROW 51000,'owned heap key quota incomplete',1;"#;

// Documented PAGE resource_description is file_id:page_id; its optional HoBT must actually match
// the exact owned partition. No unsupported page_allocations, raw-page decoder or Task pointer is used.
const LOCATORS: &str = r#"SELECT TOP(@P1) /* culprit fixture exact owned PAGE locator */
 l.resource_database_id AS database_id,l.resource_description AS file_page,
 l.resource_type,l.request_mode,l.request_status,l.request_session_id,
 CONVERT(varchar(40),s.login_time,126) AS session_generation,
 CONVERT(varchar(33),SYSUTCDATETIME(),126)+N'Z' AS observed_at,
 p.object_id,p.index_id,CONVERT(varchar(24),p.partition_id) AS partition_id,
 CONVERT(varchar(24),p.hobt_id) AS hobt_id,
 CONVERT(varchar(24),l.resource_associated_entity_id) AS lock_hobt_id,
 OBJECT_ID(N'dbo.page_fixture_topology_tree') AS tree_object_id,
 OBJECT_ID(N'dbo.page_fixture_topology_heap') AS heap_object_id
FROM sys.dm_tran_locks l JOIN sys.partitions p ON p.hobt_id=l.resource_associated_entity_id
JOIN sys.dm_exec_sessions s ON s.session_id=l.request_session_id
WHERE l.resource_database_id=DB_ID() AND l.resource_type=N'PAGE' AND l.request_mode=N'X'
 AND l.request_status=N'GRANT' AND p.partition_number=1
 AND ((p.object_id=OBJECT_ID(N'dbo.page_fixture_topology_tree') AND p.index_id=1)
   OR (p.object_id=OBJECT_ID(N'dbo.page_fixture_topology_heap') AND p.index_id=0))"#;

struct LocatedPage {
    object: i32,
    index: i32,
    partition: String,
    hobt: String,
    observed_at: String,
    table: &'static str,
}
fn positive(row: &Rec, column: &str) -> Result<i32> {
    let value = i32::try_from(integer(row, column)?).map_err(|_| invalid("native locator integer overflow"))?;
    (value > 0).then_some(value).ok_or_else(|| invalid("native locator identity unavailable"))
}
fn pages(database: u32, rows: &[Rec]) -> Result<BTreeMap<PageAddress, LocatedPage>> {
    if rows.len() != TARGETS {
        return Err(invalid(&format!(
            "native locator roster must contain exactly two tree pages and one heap page; received {} owned metadata rows: {rows:?}",
            rows.len()
        )));
    }
    let mut targets = BTreeMap::new();
    let mut owner = None;
    let mut objects = None;
    let mut tree_pages = 0;
    let mut heap_pages = 0;
    for row in rows {
        if integer(row, "database_id")? != i64::from(database)
            || text(row, "resource_type")? != "PAGE"
            || text(row, "request_mode")? != "X"
            || text(row, "request_status")? != "GRANT"
        {
            return Err(invalid("native locator scope or granted PAGE mode changed"));
        }
        let current_owner =
            (positive(row, "request_session_id")?, crate::sql_time::native_time(&text(row, "session_generation")?)?);
        if owner.as_ref().is_some_and(|owner| owner != &current_owner) {
            return Err(invalid("native page locks do not belong to one original session generation"));
        }
        owner = Some(current_owner);
        let current_objects = (positive(row, "tree_object_id")?, positive(row, "heap_object_id")?);
        if current_objects.0 == current_objects.1 || objects.is_some_and(|objects| objects != current_objects) {
            return Err(invalid("native owned object roster changed"));
        }
        objects = Some(current_objects);
        let object = positive(row, "object_id")?;
        let index = i32::try_from(integer(row, "index_id")?).map_err(|_| invalid("native index overflow"))?;
        let table = match (object, index) {
            (object, 1) if object == current_objects.0 => {
                tree_pages += 1;
                TREE
            }
            (object, 0) if object == current_objects.1 => {
                heap_pages += 1;
                HEAP
            }
            _ => return Err(invalid("native locator is outside the exact owned index roster")),
        };
        let partition = text(row, "partition_id")?;
        let hobt = text(row, "hobt_id")?;
        if !partition.parse::<u64>().is_ok_and(|id| id > 0)
            || !hobt.parse::<u64>().is_ok_and(|id| id > 0)
            || hobt != text(row, "lock_hobt_id")?
        {
            return Err(invalid("native PAGE locator has no exact partition/HoBT identity"));
        }
        // This DMV's native locator is right-padded with ASCII spaces. The strict shared
        // page-resource parser is unchanged; padding is removed only at this fixture boundary.
        let file_page = text(row, "file_page")?;
        let native = format!("{database}:{}", file_page.trim_end_matches(' '));
        let address = PageAddress::parse(&native).ok_or_else(|| invalid("native PAGE locator is malformed"))?;
        let point = LocatedPage {
            object,
            index,
            partition,
            hobt,
            observed_at: canonical_time(&text(row, "observed_at")?)?,
            table,
        };
        if targets.insert(address, point).is_some() {
            return Err(invalid("duplicate native PAGE locator cannot supply another target"));
        }
    }
    if tree_pages != 2 || heap_pages != 1 {
        return Err(invalid("native PAGE locator population is incomplete"));
    }
    Ok(targets)
}

async fn mapping(reader: &mut ActivityReader) -> Result<Vec<MappedPageTarget>> {
    Box::pin(async {
        let initial = Box::pin(context::initial(reader)).await?;
        if initial.unavailable().is_some() || initial.authority.database_id <= 4 {
            return Err(invalid("native ordinary page mapping authority unavailable"));
        }
        let rows = reader
            .io
            .query(LOCATORS, vec![Parameter::Integer((TARGETS + 1) as i64)], TARGETS + 1)
            .await
            .map_err(ReadError::into_error)?;
        let locators = pages(initial.authority.database_id, &rows)?;
        let selected = locators.keys().copied().collect();
        let before = reader.page_headers(&selected, &initial).await?;
        let after = reader.page_headers(&selected, &initial).await?;
        let checked = Box::pin(context::read(reader, &initial)).await?;
        if !initial.same_source(&checked) || before.len() != TARGETS || after.len() != TARGETS {
            return Err(invalid("native page scope or endpoint roster changed"));
        }
        let bounds = ObservationBounds::new(&initial.authority.activity.at, &checked.authority.activity.at)
            .ok_or_else(|| invalid("native page read clock regressed"))?;
        let mut targets = Vec::new();
        for (address, located) in locators {
            let first = before.get(&address).ok_or_else(|| invalid("first native page missing"))?;
            let last = after.get(&address).ok_or_else(|| invalid("last native page missing"))?;
            if !address.matches(&first.header)?
                || !address.matches(&last.header)?
                || !first.same_mapping(last)
                || !bounds.contains(&located.observed_at)
                || !bounds.contains(&first.observed_at)
                || !bounds.contains(&last.observed_at)
                || !chronological(&[&located.observed_at, &first.observed_at, &last.observed_at])
                || last.catalog.object_id != Some(located.object)
                || last.catalog.object_name.as_deref() != Some(located.table)
                || last.catalog.index_id != Some(located.index)
                || last.catalog.partition_id.as_ref() != Some(&located.partition)
                || last.catalog.hobt_id.as_ref() != Some(&located.hobt)
                || last.header.partition_id.as_ref() != Some(&located.partition)
                || first.header.next_file_id != last.header.next_file_id
                || first.header.next_page_id != last.header.next_page_id
            {
                return Err(invalid("native page/header/owned locator identity or observation bound changed"));
            }
            let target = mapped_target(&initial, last)?;
            if !target.is_valid() || target.identity_basis != PageTargetIdentityBasis::LocalDatabaseFileEpoch {
                return Err(invalid("ordinary native page/file identity unavailable"));
            }
            targets.push(target);
        }
        Ok(targets)
    })
    .await
}

fn require_shapes(targets: &[MappedPageTarget]) -> Result<()> {
    let mut heap = 0;
    let mut followed = 0;
    let mut end = 0;
    for target in targets {
        match (target.kind, target.header.level, target.header.index_id.as_deref()) {
            (PageWaitTargetKind::HeapData, Some(0), Some("0")) => heap += 1,
            (PageWaitTargetKind::IndexLeaf, Some(0), Some("1")) => {
                match (target.header.next_file_id, target.header.next_page_id) {
                    (Some(0), Some(0)) => end += 1,
                    (Some(file), Some(page))
                        if file > 0 && page > 0 && (file != target.header.file_id || page != target.header.page_id) =>
                    {
                        followed += 1;
                    }
                    _ => return Err(invalid("native leaf successor/end identity unavailable")),
                }
            }
            _ => return Err(invalid("native target kind does not match the owned heap/leaf roster")),
        }
    }
    if targets.len() != TARGETS || (heap, followed, end) != (1, 1, 1) {
        return Err(invalid("require one native heap, one followed leaf and one end leaf"));
    }
    Ok(())
}

async fn prove(fixture: &mut Fixture) -> Result<()> {
    let setup = format!("USE [{}];{SETUP} USE master;", fixture.database_name());
    tokio::time::timeout(SETUP_TIMEOUT, Box::pin(fixture.execute(&setup)))
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
    Box::pin(fixture.start_holder_gate_with_statement(LOCK_PAGES)).await?;
    let mut reader = Box::new(ActivityReader::open(
        fixture.pool.as_ref().ok_or_else(|| invalid("native fixture pool missing"))?.clone(),
        ReadOptions {
            identity: IdentityDetail::Endpoint,
            budget: CollectionBudget {
                max_queries: HEADER_QUERIES,
                max_snapshot_rows: 16,
                page_rows: 4,
                page_bytes: 64 * 1024,
                total_bytes: 128 * 1024,
                statement_timeout_ms: 1000,
                total_timeout_ms: 5000,
                ..Default::default()
            },
        },
        "127.0.0.1".into(),
        fixture.port,
    )?);
    let result = AssertUnwindSafe(Box::pin(mapping(&mut reader))).catch_unwind().await;
    let shutdown = Box::pin(reader.close()).await;
    let queries = reader.query_count();
    let metrics = reader.read_metrics();
    drop(reader);
    let targets = match result {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result?,
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    if queries != HEADER_QUERIES {
        return Err(invalid("native header/locator/context accounting changed"));
    }
    require_shapes(&targets)?;
    let mut outbox = OwnedCaptures::new(fixture)?;
    let quiet = outbox.capture(CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }).await?;
    require_readable_capture(&quiet)?;
    let original = quiet.source()?.observation.snapshot().ok_or_else(|| invalid("original page snapshot missing"))?;
    if !quiet.records.is_empty() || original.records != 0 {
        return Err(invalid(&format!(
            "quiet native source published {} rows with {} declared observations; a granted page lock is not an observed latch wait",
            quiet.records.len(),
            original.records
        )));
    }
    if quiet.manifest.source_queries != QUIET_QUERIES {
        return Err(invalid(&format!(
            "quiet native source used {} queries; expected {QUIET_QUERIES} without header enrichment",
            quiet.manifest.source_queries
        )));
    }
    if original
        .sources
        .iter()
        .any(|source| source.records != 0 || source.state != SourceState::Partial || source.complete_scope)
    {
        return Err(invalid(&format!("quiet native source coverage changed: {:?}", original.sources)));
    }
    Box::pin(fixture.release_holder_gate()).await?;
    outbox.verify_retained(&quiet).await?;
    crate::test_support::save_cost_receipt(
        &crate::test_support::receipt_directory(),
        "owned-page-topology-mapping-controls",
        &serde_json::json!({
            "successful":true,"native_targets":targets,"header_owner_queries":queries,"header_read_metrics":metrics,
            "quiet_original_manifest":quiet.manifest,"quiet_source_queries":QUIET_QUERIES,
            "extra_fixture_setup_dispatches":1,"extra_fixture_rows":129,
            "fixture_gate_lock_dispatches":1,"fixture_gate_rollback_dispatches":1,
            "fixture_gate_connections":1,"fixture_gate_is_not_a_latch_holder":true,
            "gate_work":"one owned transaction; two exact clustered endpoint keys and one heap row",
            "native_mapping_only":true,"quiet_source_population_complete":false,"healthy_zero_inferred":false,"actual_page_wait_proved":false,"latch_holder_proved":false,
            "branch_target_proved":false,"physical_io_proved":false,"causal_contribution_proved":false,
            "continuous_allocation_proved":false,"source_cost_qualified":false,
        }),
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the existing nonce-owned loopback SQL2019+ fixture; native heap/leaf mapping and quiet-wait refusal only, no latch holder/cause qualification"]
async fn local_owned_heap_and_leaf_mapping_require_exact_native_roster_without_wait_enrichment() {
    fixture::run(|fixture| Box::pin(prove(fixture))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::replace, value::Val};

    fn rows() -> Vec<Rec> {
        [(1001, 1, "1:128", "72001"), (1001, 1, "1:136", "72001"), (1002, 0, "1:144", "72002")]
            .map(|(object, index, page, hobt)| {
                Rec::of(&[
                    ("database_id", Val::Int(9)),
                    ("file_page", Val::Text(page.into())),
                    ("resource_type", Val::Text("PAGE".into())),
                    ("request_mode", Val::Text("X".into())),
                    ("request_status", Val::Text("GRANT".into())),
                    ("request_session_id", Val::Int(61)),
                    ("session_generation", Val::Text("2026-11-20T00:00:00.003".into())),
                    ("observed_at", Val::Text("2026-11-20T00:00:01Z".into())),
                    ("object_id", Val::Int(object)),
                    ("index_id", Val::Int(index)),
                    ("partition_id", Val::Text(hobt.into())),
                    ("hobt_id", Val::Text(hobt.into())),
                    ("lock_hobt_id", Val::Text(hobt.into())),
                    ("tree_object_id", Val::Int(1001)),
                    ("heap_object_id", Val::Int(1002)),
                ])
            })
            .into()
    }

    #[test]
    fn exact_granted_page_lock_roster_locates_two_tree_pages_and_one_heap_without_wait_facts() {
        let located = pages(9, &rows()).unwrap();
        assert_eq!(located.len(), TARGETS);
        assert_eq!(located.values().filter(|page| page.table == TREE).count(), 2);
        assert_eq!(located.values().filter(|page| page.table == HEAP).count(), 1);
        assert!(located.keys().all(|page| page.database == 9 && page.file == 1));
    }

    #[test]
    fn native_fixed_width_page_locators_discard_only_ascii_right_padding() {
        let mut padded = rows();
        for row in &mut padded {
            let page = text(row, "file_page").unwrap();
            *row = replace(row, &[("file_page", Val::Text(format!("{page:<256}")))]);
        }
        assert_eq!(pages(9, &padded).unwrap().len(), TARGETS);
        for malformed in [" 1:128", "1:128\t", "1:128\n", "1:128 task=0x1234"] {
            let mut changed = padded.clone();
            changed[0] = replace(&changed[0], &[("file_page", Val::Text(malformed.into()))]);
            assert!(pages(9, &changed).is_err(), "{malformed:?}");
        }
    }

    #[test]
    fn absent_partial_extra_or_duplicate_page_rosters_are_not_positive_mapping_controls() {
        let original = rows();
        for invalid in [
            Vec::new(),
            original[..2].to_vec(),
            [original.clone(), vec![original[0].clone()]].concat(),
            vec![original[0].clone(), original[0].clone(), original[2].clone()],
        ] {
            assert!(pages(9, &invalid).is_err());
        }
    }

    #[test]
    fn foreign_database_unknown_hobt_wrong_object_or_non_granted_resource_cannot_locate_a_page() {
        for (column, value) in [
            ("database_id", Val::Int(99)),
            ("object_id", Val::Int(2001)),
            ("index_id", Val::Int(2)),
            ("tree_object_id", Val::Null),
            ("partition_id", Val::Null),
            ("hobt_id", Val::Text("0".into())),
            ("lock_hobt_id", Val::Null),
            ("lock_hobt_id", Val::Text("999".into())),
            ("resource_type", Val::Text("KEY".into())),
            ("request_mode", Val::Text("U".into())),
            ("request_status", Val::Text("WAIT".into())),
            ("file_page", Val::Text("1:128:task=0x1234".into())),
            ("observed_at", Val::Text("unknown".into())),
        ] {
            let mut changed = rows();
            changed[0] = replace(&changed[0], &[(column, value)]);
            assert!(pages(9, &changed).is_err(), "{column}");
        }
    }

    #[test]
    fn locator_session_reuse_or_missing_generation_cannot_borrow_another_page_lock_owner() {
        for (column, value) in [
            ("request_session_id", Val::Int(0)),
            ("request_session_id", Val::Int(62)),
            ("session_generation", Val::Null),
            ("session_generation", Val::Text("2026-11-20T00:00:00.006".into())),
        ] {
            let mut changed = rows();
            changed[1] = replace(&changed[1], &[(column, value)]);
            assert!(pages(9, &changed).is_err(), "{column}");
        }
    }

    #[test]
    fn new_native_mapping_futures_remain_bounded_without_constructing_an_admitted_fixture() {
        const MAX_INLINE_FUTURE_BYTES: usize = 4096;
        fn future_bytes<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
            std::mem::size_of::<F>()
        }
        let case = future_bytes(|fixture: &'static mut Fixture| prove(fixture));
        let mapping = future_bytes(|reader: &'static mut ActivityReader| super::mapping(reader));
        eprintln!("owned topology mapping futures: case={case}; mapping={mapping}");
        assert!(case <= MAX_INLINE_FUTURE_BYTES);
        assert!(mapping <= MAX_INLINE_FUTURE_BYTES);
    }
}
