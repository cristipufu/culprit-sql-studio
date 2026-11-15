use super::*;
use crate::{
    cache_entry_memory::tests::{AT, focus, native_rows},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

fn request(focus: CacheEntryMemoryFocus) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::CacheEntryMemory { focus: Box::new(focus) },
        budget: CollectionBudget { page_rows: 1, ..CollectionBudget::default() },
    }
}

fn replies(final_rows: Vec<Rec>, final_owner: Rec) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let statement = crate::cached_statements::context(&crate::cache_entry_memory::tests::statement_row(0, 14)).unwrap();
    vec![
        Ok(vec![crate::grants::tests::platform()]),
        Ok(vec![crate::grants::tests::authority(AT)]),
        Ok(native_rows()),
        Ok(final_rows),
        Ok(vec![Rec::of(&[
            ("plan_handle", Val::Text(statement.identity.plan_handle)),
            ("sql_handle", Val::Text(statement.identity.sql_handle)),
        ])]),
        Ok(vec![final_owner]),
    ]
}

async fn pages<R: NativeRead>(source: &mut CacheEntryMemorySource<R>) -> Vec<TelemetryRecord> {
    let mut cursor = None;
    let mut rows = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    rows
}

#[tokio::test]
async fn explicit_valid_construction_is_lazy_and_absent_selectors_never_open_or_read() {
    let pool = Arc::new(Pool::new(crate::test_support::config(1, "never-opened", "unused", "culprit"), None));
    let mut requested = request(focus().await);
    let mut source = CacheEntryMemorySource::new(pool.clone(), &requested, "unused.invalid".into(), 1).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let CollectionSelection::CacheEntryMemory { focus } = &mut requested.selection else { panic!() };
    focus.statements.clear();
    assert!(CacheEntryMemorySource::new(pool.clone(), &requested, "unused.invalid".into(), 1).is_err());
    requested.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::CacheEntryMemory };
    assert!(CacheEntryMemorySource::new(pool, &requested, "unused.invalid".into(), 1).is_err());
}

#[tokio::test]
async fn the_same_selected_source_freezes_one_whole_entry_and_checks_ownership_before_completion() {
    let requested = request(focus().await);
    let mut changed_metrics = native_rows();
    changed_metrics[3] =
        replace(&changed_metrics[3], &[("usecounts", Val::Int(100)), ("size_in_bytes", Val::Int(65536))]);
    let reader = FakeRead::new(replies(changed_metrics, crate::grants::tests::authority(AT)));
    let mut source = CacheEntryMemorySource::from_reader(reader, &requested, "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(description.observation.snapshot().unwrap().sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(source.query_count(), 3);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 3, "paging owns no native work and incomplete paging cannot verify");
    let records = pages(&mut source).await;
    assert_eq!(records.len(), 1);
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    let OperationalFact::MssqlCacheEntryMemory(entry) = &row.fact else { panic!() };
    assert_eq!(entry.statements.len(), 2);
    assert_eq!(entry.size_bytes, "32768", "the original point is not overwritten by the recheck");
    assert_eq!(entry.lookup_count, "1");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 6);
}

#[tokio::test]
async fn a_changed_member_epoch_role_or_native_clock_cannot_complete_the_point() {
    for (field, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("local_started_ticks", Val::Int(200)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text("2020-01-01T05:59:59Z".into())),
    ] {
        let reader =
            FakeRead::new(replies(native_rows(), replace(&crate::grants::tests::authority(AT), &[(field, value)])));
        let mut source =
            CacheEntryMemorySource::from_reader(reader, &request(focus().await), "fixture.invalid".into(), 1433)
                .unwrap();
        source.describe().await.unwrap();
        assert_eq!(pages(&mut source).await.len(), 1);
        let outcome = source.finish().await;
        if field == "observed_at" {
            assert_eq!(
                CollectionStop::from_error(&outcome.unwrap_err()),
                CollectionStop::InvalidPage,
                "a backwards native clock is malformed evidence, not a publishable point"
            );
        } else {
            assert_eq!(outcome.unwrap(), SourceCompletion::Changed, "{field}");
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn an_original_known_role_or_future_point_is_checked_before_any_entry_lookup() {
    for future in [false, true] {
        let mut original = focus().await;
        if future {
            original.context.observed_until = "2020-01-01T06:00:01Z".into();
        } else {
            original.context.database_writable = Some(false);
        }
        let reader = FakeRead::new(vec![
            Ok(vec![crate::grants::tests::platform()]),
            Ok(vec![crate::grants::tests::authority(AT)]),
        ]);
        let mut source =
            CacheEntryMemorySource::from_reader(reader, &request(original), "fixture.invalid".into(), 1433).unwrap();
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
        assert_eq!(source.query_count(), 2);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn unknown_original_role_stays_unknown_and_does_not_block_a_new_qualified_owner() {
    let mut original = focus().await;
    original.context.database_writable = None;
    let reader = FakeRead::new(replies(native_rows(), crate::grants::tests::authority(AT)));
    let mut source =
        CacheEntryMemorySource::from_reader(reader, &request(original.clone()), "fixture.invalid".into(), 1433)
            .unwrap();
    let description = source.describe().await.unwrap();
    let SourceObservation::CacheEntryMemory { focus, .. } = &description.observation else { panic!() };
    assert_eq!(focus.context.database_writable, None);
    pages(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn altered_sql_handle_binding_or_cache_keys_stops_followup_reads_in_the_same_owner() {
    let original = focus().await;
    let mut changed = native_rows();
    changed[4] = replace(&changed[4], &[("attribute_value", Val::Text("252".into()))]);
    let reader = FakeRead::new(replies(changed, crate::grants::tests::authority(AT)));
    let mut source =
        CacheEntryMemorySource::from_reader(reader, &request(original.clone()), "fixture.invalid".into(), 1433)
            .unwrap();
    source.describe().await.unwrap();
    pages(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 4, "no binding or authority I/O follows a changed native key");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);

    let mut data = replies(native_rows(), crate::grants::tests::authority(AT));
    data[4] = Ok(vec![Rec::of(&[
        ("plan_handle", Val::Text(original.statements[0].identity.plan_handle.clone())),
        ("sql_handle", Val::Text("0x0304".into())),
    ])]);
    let mut source =
        CacheEntryMemorySource::from_reader(FakeRead::new(data), &request(original), "fixture.invalid".into(), 1433)
            .unwrap();
    source.describe().await.unwrap();
    pages(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 5, "no authority read follows a failed original plan binding");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn caller_query_limits_survive_both_phases_and_uncertain_close_is_never_disguised() {
    let mut requested = request(focus().await);
    requested.budget.max_queries = 3;
    let reader = FakeRead::new(replies(native_rows(), crate::grants::tests::authority(AT))).with_query_limit(3);
    let mut source = CacheEntryMemorySource::from_reader(reader, &requested, "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    pages(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 3);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);

    let mut reader = FakeRead::new(replies(native_rows(), crate::grants::tests::authority(AT))).pause_on_query(4);
    reader.shutdown = SourceShutdown::Unconfirmed;
    let mut source =
        CacheEntryMemorySource::from_reader(reader, &request(focus().await), "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    pages(&mut source).await;
    let mut pending = Box::pin(source.finish());
    assert!(futures_util::poll!(pending.as_mut()).is_pending());
    drop(pending);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
}
