pub(crate) use super::super::hyperscale::tests::owner;
use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn metadata(at: &str, local: bool) -> Rec {
    replace(
        &super::super::hyperscale::tests::metadata("Hyperscale", at),
        &[("local_state_columns", Val::Int(if local { LOCAL_STATE_COLUMNS } else { 0 }))],
    )
}
pub(crate) async fn layout(owner: &Context) -> Layout {
    let mut reader = FakeRead::new(vec![Ok(vec![metadata("2020-01-01T06:00:01Z", true)])]);
    let scope = scope(owner).unwrap();
    let layout = read(&mut reader, owner, &scope).await.unwrap().unwrap();
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    layout
}
#[test]
fn both_fixed_native_layouts_share_one_metadata_statement_and_no_quorum_or_pool_fallback() {
    let sql = layout_sql();
    assert!(sql.contains("sys.dm_hs_database_replicas"));
    assert!(sql.contains("sys.dm_database_replica_states"));
    assert!(sql.contains("precision=25 AND scale=0"));
    assert!(!sql.contains("quorum_"));
    assert_eq!(HS_COLUMNS.len(), 8);
    assert_eq!(LOCAL_COLUMNS.len(), 36);
}
#[tokio::test]
async fn changed_final_layout_identity_role_or_clock_cannot_repair_an_original_read() {
    let owner = owner().await;
    let layout = layout(&owner).await;
    let scope = layout.scope.clone();
    for change in [
        "local_state_columns",
        "supported_columns",
        "observer_replica_id",
        "selected_database_id",
        "updateability",
        "observed_at",
    ] {
        let value = match change {
            "local_state_columns" => Val::Int(0),
            "supported_columns" => Val::Int(7),
            "selected_database_id" => Val::Int(8),
            "updateability" => Val::Text("READ_ONLY".into()),
            "observed_at" => Val::Text("2019-01-01T00:00:00Z".into()),
            _ => Val::Text("11111111-1111-1111-1111-111111111111".into()),
        };
        let mut reader =
            FakeRead::new(vec![Ok(vec![replace(&metadata("2020-01-01T06:00:02Z", true), &[(change, value)])])]);
        let witness = Witness { layout: layout.clone(), observed_at: layout.observed_at.clone() };
        let result = witness.verify(&mut reader, &owner).await;
        assert!(result.is_err() || matches!(result, Ok(false)), "{change}");
        assert_eq!(reader.queries(), 1);
    }
    assert_eq!(scope.database_id, 7);
}
