use super::super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    performance::test_support::{Reader, authority},
    read::ReadResult,
    resources::test_support::{instance_pressure_row, replace},
    value::{Rec, Val},
};
use culprit_core::SourceState;
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::SuspectPages },
        budget: CollectionBudget::default(),
    }
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(3))])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:00Z")]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![replace(
            &crate::operations::native::suspect_pages::tests::row(0, 3),
            &[("database_name", Val::Text("shop".into()))],
        )]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:01Z")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:02Z")]),
    ]
}
fn source(rows: Vec<ReadResult<Vec<Rec>>>) -> StateSource<Reader> {
    let mut reader = Reader::new(rows);
    reader.inner = reader.inner.with_query_limit(local_point_budget(&request().budget).max_queries);
    StateSource::with_io(reader, &request(), "fixture.invalid".into(), 1433).unwrap()
}
async fn page(source: &mut StateSource<Reader>) -> Vec<TelemetryRecord> {
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert!(next_cursor.is_none());
    records
}
#[tokio::test]
async fn selected_history_uses_existing_one_native_aggregate_and_original_final_owner() {
    let mut source = source(replies());
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    let rows = page(&mut source).await;
    let [TelemetryRecord::Operational(row)] = rows.as_slice() else { panic!() };
    let OperationalFact::SuspectPages(point) = &row.fact else { panic!() };
    assert_eq!((point.count.as_str(), point.retained_rows.as_str()), ("0", "3"));
    assert!(point.fits_source(&description));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.inner.closed, 1);
    assert_eq!(source.io.inner.calls.iter().filter(|call| call.sql.contains("FROM msdb.dbo.suspect_pages")).count(), 1);
}
#[tokio::test]
async fn original_denial_is_separate_from_empty_and_never_adds_a_fallback_dispatch() {
    let mut rows = replies();
    rows[6] = Ok(vec![replace(
        &crate::operations::native::suspect_pages::tests::row(0, 0),
        &[("native_error", Val::Int(229))],
    )]);
    let mut source = source(rows);
    let description = source.describe().await.unwrap();
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    assert_eq!(coverage.state, SourceState::Unavailable);
    assert_eq!(coverage.error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(page(&mut source).await.is_empty());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn point_id_role_native_clock_socket_or_final_database_change_refuses_original_capture() {
    for change in ["id", "role", "clock", "socket", "final-generation"] {
        let mut rows = replies();
        if change == "final-generation" {
            rows[10] = Ok(vec![replace(
                &authority("2020-01-01T06:00:02Z"),
                &[("database_guid", Val::Text("different-generation".into()))],
            )]);
        } else if change != "socket" {
            let (name, value) = match change {
                "id" => ("native_database_id", Val::Int(8)),
                "role" => ("updateability", Val::Text("READ_ONLY".into())),
                _ => ("observed_at", Val::Text("2019-01-01T00:00:00Z".into())),
            };
            let point = replace(
                &crate::operations::native::suspect_pages::tests::row(0, 3),
                &[("database_name", Val::Text("shop".into())), (name, value)],
            );
            rows[6] = Ok(vec![point]);
        }
        let mut source = source(rows);
        if change == "socket" {
            source.io.change_at = Some(7);
        }
        if change == "final-generation" {
            let description = source.describe().await.unwrap_or_else(|error| {
                panic!("{change}: original description must precede the later change: {:?}", error.code)
            });
            let delivery = source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap_or_else(|error| {
                panic!("{change}: original provisional page must be readable: {:?}", error.code)
            });
            let CollectionRead::Page { records, next_cursor } = delivery else {
                panic!("{change}: expected one original provisional page");
            };
            assert!(next_cursor.is_none(), "{change}: the singleton page must reach EOF");
            let [TelemetryRecord::Operational(row)] = records.as_slice() else {
                panic!("{change}: exactly one original provisional fact must be retained");
            };
            let OperationalFact::SuspectPages(point) = &row.fact else {
                panic!("{change}: the original fact must retain its native kind");
            };
            assert!(point.fits_source(&description), "{change}: original scope must remain valid before recheck");
            // A successful recheck can observe a different owner. Changed is the explicit nonverified
            // completion; the coordinator refuses publication of this provisional page.
            let completion = source.finish().await.unwrap_or_else(|error| {
                panic!("{change}: this native recheck succeeds and reports a changed owner: {:?}", error.code)
            });
            assert_eq!(
                completion,
                SourceCompletion::Changed,
                "{change}: changed database generation must refuse completion"
            );
            assert!(!completion.is_verified(), "{change}: provisional rows cannot acquire verified publication");
            assert_eq!(source.query_count(), 11, "{change}: the same original owner must perform the final recheck");
        } else {
            let error =
                source.describe().await.expect_err(&format!("{change}: changed point owner must fence description"));
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{change}");
            assert!(source.rows.is_empty(), "{change}: a foreign point must never be staged");
            assert!(
                source.read_page(TelemetryStream::OperationalSamples, None).await.is_err(),
                "{change}: a fenced source must expose no page"
            );
            assert_eq!(source.query_count(), 7, "{change}: refusal must not dispatch a later clock or recheck");
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent, "{change}: the original reader must settle");
        assert_eq!(source.io.inner.closed, 1, "{change}: the same source must close exactly once");
    }
}
