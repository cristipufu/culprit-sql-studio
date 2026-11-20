//! Actual registered source + immutable Outbox; the existing nonce-owned two-row columnstore fixture owns DDL.
use super::*;
use crate::test_support::owned_captures::{Captured, OwnedCaptures};

fn selection(target: &MssqlColumnstoreTarget) -> CollectionSelection {
    CollectionSelection::ColumnstoreRowgroups { target: Box::new(target.clone()) }
}
fn points(capture: &Captured) -> Vec<&MssqlColumnstoreRowgroup> {
    capture
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.record else { panic!("original rowgroup record") };
            let OperationalFact::MssqlColumnstoreRowgroup(point) = &row.fact else {
                panic!("original rowgroup native fact")
            };
            assert!(point.fits_source(capture.source().unwrap()));
            point.as_ref()
        })
        .collect()
}
async fn capture(captures: &mut OwnedCaptures, target: &MssqlColumnstoreTarget) -> Captured {
    let captured = captures.capture(selection(target)).await.expect("actual registered selected rowgroup source");
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    assert_eq!(captured.manifest.source_queries, 11);
    assert_eq!(captured.source().unwrap().continuity, SourceContinuity::Unverified);
    assert!(captured.source().unwrap().partition.is_qualified());
    let recipe =
        crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mssql.columnstore-rowgroups").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    captured
}
async fn denied_target(fixture: &mut Fixture<'_>, target: &MssqlColumnstoreTarget) {
    let pool = Arc::new(Pool::new(
        config(
            fixture.owned.port,
            &fixture.owned.database,
            fixture.owned.password(),
            "culprit-rowgroup-denied-control",
        ),
        None,
    ));
    let mut reader = ReadSession::application(pool);
    let result = AssertUnwindSafe(async {
        reader
            .query(&format!("EXECUTE AS USER=N'{}'", fixture.user), vec![], 1)
            .await
            .map_err(ReadError::into_error)
            .unwrap();
        let visible = denied_visibility(&mut reader, fixture, &target.table)
            .await
            .expect("native denied permission and exact owned SELECT");
        let Err(error) = crate::columnstore_rowgroups::target(&mut reader, target).await else {
            panic!("the native denied target cannot produce an available observation");
        };
        assert!(
            error.code == culprit_core::ErrorCode::PermissionDenied
                || !visible && CollectionStop::from_error(&error) == CollectionStop::SourceUnavailable,
            "only the proved native permission/visibility refusal is expected"
        );
        assert_eq!(denied_visibility(&mut reader, fixture, &target.table).await.unwrap(), visible);
    })
    .catch_unwind()
    .await;
    let shutdown = reader.close().await;
    if shutdown != SourceShutdown::Quiescent {
        fixture.control_available = false;
    }
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    if let Err(panic) = result {
        resume_unwind(panic);
    }
}
#[tokio::test]
#[ignore = "requires explicit nonce-owned SQL2022+ fixture; only existing two-row fixture DDL/DML, no production scan or cost qualification"]
async fn owned_selected_rowgroups_keep_compressed_deleted_empty_and_native_target_refusal_separate() {
    let owned = OwnedSourceFixture::open("culprit-rowgroup-fixture").await.unwrap();
    let suffix = format!("{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let mut fixture = Fixture {
        owned: &owned,
        table: format!("culprit_rowgroups_{suffix}"),
        user: format!("culprit_rowgroups_reader_{suffix}"),
        owns: false,
        control_available: true,
    };
    let result=AssertUnwindSafe(async{
        fixture.initialize().await;
        let parent=compare(&fixture,"rowgroup-parent",None).await.expect("actual native CSI configuration");
        let target=MssqlColumnstoreTarget{table:parent.table.clone(),index_id:parent.index_id,table_modified_without_offset:parent.table_modified_without_offset.clone(),partition_number:Some(1)};
        let mut captures=OwnedCaptures::from_pool(owned.pool.clone(),owned.port).unwrap();
        let initial=capture(&mut captures,&target).await;
        assert!(points(&initial).iter().any(|point|point.state==3&&point.total_rows.as_deref()==Some("2")&&point.deleted_rows.as_deref()==Some("0")),"the real index build must supply a compressed native two-row group");
        assert!(points(&initial).iter().all(|point|point.delta_store_hobt_id.is_none()));
        denied_target(&mut fixture,&target).await;
        fixture.command(&format!("DELETE FROM dbo.[{}] WHERE id=1; IF @@ROWCOUNT<>1 THROW 51000,'owned rowgroup delete quota incomplete',1;",fixture.table)).await;
        let deleted=capture(&mut captures,&target).await;
        assert!(points(&deleted).iter().any(|point|point.state==3&&point.total_rows.as_deref()==Some("2")&&point.deleted_rows.as_deref()==Some("1")),"native deleted rows remain physically stored; no reclaimable-byte assertion");
        fixture.command(&format!("TRUNCATE TABLE dbo.[{}]",fixture.table)).await;
        let empty_parent=compare(&fixture,"empty-rowgroup-parent",None).await.expect("still a real CSI after truncation");
        let empty_target=MssqlColumnstoreTarget{table:empty_parent.table.clone(),index_id:empty_parent.index_id,table_modified_without_offset:empty_parent.table_modified_without_offset.clone(),partition_number:Some(1)};
        let empty=capture(&mut captures,&empty_target).await;assert!(empty.records.is_empty());
        assert_eq!(empty.source().unwrap().observation.snapshot().unwrap().sources[0].state,culprit_core::SourceState::Empty);
        let mut wrong=empty_target.clone();wrong.table_modified_without_offset="1900-01-01T00:00:00.000".into();
        let request=CollectionRequest{subject:CollectionSubject::Database,selection:selection(&wrong),budget:CollectionBudget::default()};
        let mut refused=crate::telemetry::source(owned.pool.clone(),&request,"127.0.0.1".into(),owned.port).unwrap();
        let error=refused.describe().await.unwrap_err();let shutdown=refused.close().await;
        if shutdown!=SourceShutdown::Quiescent{fixture.control_available=false;}
        assert_eq!(shutdown,SourceShutdown::Quiescent);assert_eq!(CollectionStop::from_error(&error),CollectionStop::SourceChanged);
        for original in [&initial,&deleted,&empty]{captures.verify_retained(original).await.unwrap();}
    }).catch_unwind().await;
    if owned.pool.source_shutdown() != SourceShutdown::Quiescent {
        fixture.control_available = false;
    }
    let cleanup = AssertUnwindSafe(fixture.cleanup()).catch_unwind().await;
    if let Err(panic) = result {
        resume_unwind(panic);
    }
    cleanup.expect("original fixture cleanup");
}
