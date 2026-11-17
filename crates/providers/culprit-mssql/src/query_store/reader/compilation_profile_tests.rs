use super::*;
use crate::{
    query_store::window_sql::tests::layout_row,
    resources::test_support::{FakeRead, replace},
    value::Val,
};

#[tokio::test]
async fn readable_but_unsupported_native_profiles_never_dispatch_compilation_select_or_fake_empty() {
    for edition in [0, 1, 6, 7, 9, 12, 99] {
        let layout = replace(&layout_row(), &[("engine_edition", Val::Int(edition))]);
        let mut reader =
            Reader::new(FakeRead::new(vec![Ok(vec![layout]), Ok(vec![Rec::of(&[("other_dataset", Val::Int(1))])])]));
        assert_eq!(reader.native_layout().await.unwrap().edition, edition);
        let acquired = reader.queries();
        for ids in [vec!["3".into()], vec![]] {
            let error = reader.compilations(&ids).await.err().expect("unsupported compilation profile");
            assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
            assert_eq!(reader.queries(), acquired, "no selected compilation SQL or replacement discovery");
        }
        assert!(
            reader.retirement_reason().is_none(),
            "a source-specific unsupported profile does not retire other Query Store data"
        );
        assert_eq!(reader.read_rows("SELECT /* culprit */ 1 AS other_dataset").await.unwrap().len(), 1);
        assert_eq!(reader.queries(), acquired + 1);
        assert!(reader.io.calls.iter().all(|call| !call.sql.contains("q.count_compiles")));
    }
}

#[tokio::test]
async fn absent_native_layout_cannot_assume_sql_server_or_dispatch_discovery() {
    let mut reader = Reader::new(FakeRead::new(vec![]));
    for ids in [vec![], vec!["3".into()]] {
        assert_eq!(reader.compilations(&ids).await.err().unwrap().code, culprit_core::ErrorCode::Unsupported);
    }
    assert_eq!(reader.queries(), 0);
}

#[tokio::test]
async fn supported_native_profiles_reuse_the_same_query_and_decoder_without_more_metadata() {
    for edition in COMPILATION_NATIVE_EDITIONS {
        let layout = replace(&layout_row(), &[("engine_edition", Val::Int(*edition))]);
        let mut reader =
            Reader::new(FakeRead::new(vec![Ok(vec![layout]), Ok(vec![crate::query_store::tests::compilation_row()])]));
        reader.native_layout().await.unwrap();
        let rows = reader.compilations(&["3".into()]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].observation().is_valid());
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.io.calls[1].sql, crate::query_store::costs::compilation_sql());
    }
}

#[tokio::test]
async fn compilation_reads_do_not_require_or_invent_a_runtime_replica_population() {
    for mode in ["READ_WRITE", "READ_ONLY", "READ_CAPTURE_SECONDARY"] {
        let layout =
            replace(&layout_row(), &[("replicas", Val::Int(1)), ("actual_state_desc", Val::Text(mode.into()))]);
        let mut reader = Reader::new(FakeRead::new(vec![
            Ok(vec![layout]),
            Ok(vec![crate::query_store::tests::compilation_row()]),
            Ok(vec![]),
        ]));
        reader.native_layout().await.expect("native compiler profile is independent of runtime replica membership");
        let rows = reader.compilations(&["3".into()]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].observation().is_valid());
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.io.calls[1].sql, crate::query_store::costs::compilation_sql());
        assert!(reader.population_catalog().is_none(), "compiler metadata does not establish a runtime population");
        assert!(reader.selected_population().unwrap().is_none());

        let error = reader.population().await.expect_err("runtime still needs its actual native replica catalog");
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert_eq!(reader.io.calls[2].sql, crate::query_store::catalog_sql::REPLICAS);
        assert_eq!(reader.queries(), 3);
        assert!(reader.population_catalog().is_none(), "an empty native catalog never becomes unpartitioned");
        assert!(reader.compilations(&["3".into()]).await.is_err());
        assert_eq!(reader.queries(), 3, "a retired owner never dispatches further compiler work");
    }
}
