use super::*;
use crate::{
    resources::test_support::replace,
    tables::tests::{row, without_full_text},
    value::Val,
};

#[test]
fn enabled_disabled_tracking_and_last_crawl_remain_independent_native_catalog_values() {
    for (native_tracking, tracking) in [
        ("M", MssqlFullTextChangeTracking::Manual),
        ("A", MssqlFullTextChangeTracking::Automatic),
        ("O", MssqlFullTextChangeTracking::Off),
    ] {
        for enabled in [false, true] {
            let row = replace(
                &row(),
                &[
                    ("full_text_enabled", Val::Int(i64::from(enabled))),
                    ("full_text_change_tracking", Val::Text(native_tracking.into())),
                ],
            );
            let state = decode(&row, 100).unwrap();
            assert_eq!(state.availability, MssqlCatalogAvailability::Available);
            let index = state.index.unwrap();
            assert_eq!(index.enabled, Some(enabled));
            assert_eq!(index.change_tracking, Some(tracking));
            assert_eq!(index.last_crawl_completed, Some(true));
            assert_eq!(index.unique_index_id, Some(1));
            assert_eq!(index.catalog_id, Some(3));
        }
    }
}

#[test]
fn native_null_fields_do_not_erase_known_index_presence_or_become_false() {
    let indexed = replace(&without_full_text(0), &[("full_text_object_id", Val::Int(100))]);
    let state = decode(&indexed, 100).unwrap();
    let index = state.index.unwrap();
    assert!(index.enabled.is_none());
    assert!(index.change_tracking.is_none());
    assert!(index.last_crawl_completed.is_none());
    assert!(index.unique_index_id.is_none());
    assert!(index.catalog_id.is_none());
    let absent = decode(&without_full_text(0), 100).unwrap();
    assert_eq!(absent.availability, MssqlCatalogAvailability::Available);
    assert!(absent.index.is_none());
    for availability in [1, 2, 3] {
        let unknown = decode(&without_full_text(availability), 100).unwrap();
        assert_ne!(unknown.availability, MssqlCatalogAvailability::Available);
        assert!(unknown.index.is_none());
    }
}

#[test]
fn foreign_table_partial_join_and_unavailable_payloads_cannot_supply_full_text_identity() {
    assert!(decode(&row(), 101).is_err());
    for availability in [1, 2, 3] {
        assert!(decode(&replace(&row(), &[("full_text_availability", Val::Int(availability))]), 100).is_err());
    }
    assert!(decode(&replace(&row(), &[("full_text_object_id", Val::Null)]), 100).is_err());
    for name in ["full_text_unique_index_id", "full_text_catalog_id"] {
        for id in [0, -1, i64::from(i32::MAX) + 1] {
            assert!(decode(&replace(&row(), &[(name, Val::Int(id))]), 100).is_err());
        }
    }
}

#[test]
fn unsupported_codes_and_malformed_native_fields_are_not_guessed_or_coerced() {
    for (name, value) in [
        ("full_text_availability", Val::Int(4)),
        ("full_text_availability", Val::Null),
        ("full_text_enabled", Val::Int(2)),
        ("full_text_last_crawl_completed", Val::Int(2)),
        ("full_text_change_tracking", Val::Text("AUTO".into())),
        ("full_text_change_tracking", Val::Text("X".into())),
        ("full_text_object_id", Val::Text("100".into())),
    ] {
        assert!(decode(&replace(&row(), &[(name, value)]), 100).is_err());
    }
}

#[test]
fn optional_catalog_binding_preserves_one_bounded_table_query_and_has_no_live_full_text_work() {
    let query = crate::tables::select_sql(&crate::tables::Layout::new(true));
    assert!(query.contains("IF @culprit_full_text_availability=0"));
    assert!(query.contains("HAS_PERMS_BY_NAME(N'sys.fulltext_indexes',N'OBJECT',N'SELECT')"));
    assert!(query.contains("object_id=OBJECT_ID(N'sys.fulltext_indexes')"));
    assert!(query.contains("LEFT JOIN sys.fulltext_indexes ft ON ft.object_id=t.object_id"));
    assert!(query.contains("@P1=@P1"));
    assert!(
        query.matches(crate::statement_origin::MONITORING_QUERY_MARKER).count() >= 2,
        "both nested statements retain monitoring provenance"
    );
    assert!(query.contains("TOP (@P1) DB_ID() AS database_id"));
    assert!(query.contains("ft.is_enabled AS full_text_enabled"));
    assert!(query.contains("@culprit_full_text_availability AS full_text_availability"));
    for forbidden in [
        "TableFulltext",
        "sys.dm_fts",
        "sys.fulltext_index_fragments",
        "crawl_start_date",
        "crawl_end_date",
        "IsIndexable",
        "DBCC",
    ] {
        assert!(!query.contains(forbidden), "{forbidden}");
    }
}
