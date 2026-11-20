use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
pub(crate) fn row(active: i64, retained: i64) -> Rec {
    Rec::of(&[
        ("native_error", Val::Int(0)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.5000000Z".into())),
        ("native_database_id", Val::Int(7)),
        ("database_name", Val::Text("sample".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("count", Val::Int(active)),
        ("retained_rows", Val::Int(retained)),
        ("last_native_at", if active == 0 { Val::Null } else { Val::Text("2019-12-30T15:00:00.123".into()) }),
    ])
}
#[tokio::test]
async fn one_native_aggregate_preserves_all_retained_rows_without_promoting_repaired_statuses() {
    let mut io = FakeRead::new(vec![Ok(vec![row(0, 3)])]);
    let super::super::Facts::SuspectPages(point) =
        super::super::read(&mut io, super::super::Kind::SuspectPages, 1).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((point.count.as_str(), point.retained_rows.as_str()), ("0", "3"));
    assert!(point.last_reported_time_without_offset.is_none());
    assert_eq!(io.calls.len(), 1);
    let sql = &io.calls[0].sql;
    assert_eq!(sql.matches("FROM msdb.dbo.suspect_pages").count(), 1);
    assert!(sql.contains("COUNT_BIG(*) AS retained_rows"));
    assert_eq!(sql.matches("CASE WHEN event_type IN(1,2,3)").count(), 2);
    assert!(sql.contains("WHERE database_id=DB_ID()"));
    assert!(!sql.contains("GROUP BY"));
    assert_eq!(
        sql,
        &super::super::Kind::SuspectPages.sql(),
        "the reader must dispatch the complete builder batch unchanged"
    );
    assert!(sql.ends_with("END CATCH;"), "the batch must not acquire a trailing SELECT hint");
    assert_eq!(sql.matches("OPTION(MAXDOP 1)").count(), 2, "both executed SELECT paths own their serial hint");
    assert!(
        sql.contains("WHERE database_id=DB_ID() OPTION(MAXDOP 1)'"),
        "the aggregate hint belongs inside the dynamic SELECT"
    );
    assert!(
        sql.contains("AS last_native_at OPTION(MAXDOP 1); END CATCH;"),
        "the refusal SELECT owns its hint inside CATCH"
    );
}
#[test]
fn active_maximum_stays_native_local_and_does_not_become_acquisition_utc() {
    let point = decode(&[row(2, 4)]).unwrap();
    assert!(point.observed_at.ends_with('Z'));
    assert_eq!(point.last_reported_time_without_offset.as_deref(), Some("2019-12-30T15:00:00.123"));
    let mut null = row(2, 4);
    null = replace(&null, &[("last_native_at", Val::Null)]);
    assert!(decode(&[null]).unwrap().last_reported_time_without_offset.is_none());
}
#[test]
fn denied_unsupported_null_and_malformed_native_results_never_become_known_zeroes() {
    for (code, expected) in [
        (229, ErrorCode::PermissionDenied),
        (297, ErrorCode::PermissionDenied),
        (300, ErrorCode::PermissionDenied),
        (916, ErrorCode::PermissionDenied),
        (207, ErrorCode::Unsupported),
        (208, ErrorCode::Unsupported),
    ] {
        let value = replace(&row(0, 0), &[("native_error", Val::Int(code))]);
        assert_eq!(decode(&[value]).unwrap_err().code, expected);
    }
    for (name, value) in [
        ("count", Val::Null),
        ("count", Val::Int(5)),
        ("retained_rows", Val::Int(-1)),
        ("native_database_id", Val::Int(0)),
        ("observed_at", Val::Text("unknown".into())),
        ("native_error", Val::Int(999)),
    ] {
        assert!(decode(&[replace(&row(2, 4), &[(name, value)])]).is_err(), "{name}");
    }
    assert!(decode(&[replace(&row(0, 3), &[("last_native_at", Val::Text("2020-01-01T00:00:00".into()))])]).is_err());
    assert!(decode(&[]).is_err());
    assert!(decode(&[row(0, 0), row(0, 0)]).is_err());
}
