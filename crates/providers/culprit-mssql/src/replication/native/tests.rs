use super::*;
use crate::{
    replication::tests::{geo, unavailable},
    resources::test_support::FakeRead,
    value::Val,
};

#[tokio::test]
async fn documented_availability_markers_are_results_on_the_existing_read_not_empty_replication() {
    for (number, code) in [
        (229, ErrorCode::PermissionDenied),
        (297, ErrorCode::PermissionDenied),
        (300, ErrorCode::PermissionDenied),
        (208, ErrorCode::Unsupported),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![unavailable(number)])]);
        let NativeRows::Unavailable(error) = read(&mut reader, GEO, 7).await.unwrap() else {
            panic!("native availability outcome")
        };
        assert_eq!(error.code, code);
        assert!(error.detail.is_none());
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.calls[0].limit, 7, "one original row/byte/deadline envelope");
        assert!(reader.calls[0].sql.contains("EXEC sys.sp_executesql"));
        assert!(reader.calls[0].sql.contains("/* culprit:monitor */"), "the nested SELECT is also monitoring-owned");
        assert!(reader.calls[0].sql.contains("IF ERROR_NUMBER() NOT IN(229,297,300,208) THROW;"));
        assert!(!reader.calls[0].sql.contains("ERROR_MESSAGE"), "native message text does not drive availability");
    }
}

#[test]
fn mixed_duplicate_malformed_and_unknown_markers_cannot_become_successful_family_coverage() {
    let mut cases = vec![
        vec![unavailable(229), geo()],
        vec![geo(), unavailable(208)],
        vec![unavailable(229), unavailable(229)],
        vec![Rec::of(&[(AVAILABILITY_ERROR, Val::Int(229)), ("extra", Val::Int(1))])],
    ];
    cases.extend(
        [
            Val::Null,
            Val::Float(229.0),
            Val::Text("229".into()),
            Val::Int(-1),
            Val::Int(0),
            Val::Int(207),
            Val::Int(1222),
        ]
        .into_iter()
        .map(|value| vec![Rec::of(&[(AVAILABILITY_ERROR, value)])]),
    );
    for rows in cases {
        let error = availability(rows).err().expect("invalid native availability protocol");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    }
    assert!(matches!(availability(vec![]).unwrap(), NativeRows::Available(rows) if rows.is_empty()));
    assert!(matches!(availability(vec![geo()]).unwrap(), NativeRows::Available(rows) if rows.len()==1));
}

#[tokio::test]
async fn unexpected_native_errors_are_not_reinterpreted_as_completed_source_availability() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        assert_eq!(CollectionStop::from_error(&read(&mut reader, GEO, 7).await.err().unwrap()), stop);
        assert_eq!(reader.queries(), 1);
    }
    let error = Error::new(ErrorCode::PermissionDenied, "controlled error outside the completed native boundary");
    let mut reader = FakeRead::new(vec![Err(ReadError::Source(error))]);
    assert_eq!(read(&mut reader, GEO, 7).await.err().unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(reader.queries(), 1);
}
