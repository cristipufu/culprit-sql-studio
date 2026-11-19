use super::*;
use crate::{
    activity::{
        ActivityReader, IdentityDetail, ReadOptions,
        page_targets::{
            self, context,
            shared_tempdb::tests::{replies, resource, tempdb, tempdb_header},
            tests::{boxed_context, page_wait, participant},
        },
    },
    instance::test_support::{epoch, metadata},
    read::{NativeRead, Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

struct ConnectionRead {
    native: FakeRead,
    generation: Option<u64>,
    changed_at: Option<usize>,
}
#[async_trait::async_trait]
impl NativeRead for ConnectionRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
            .map(|generation| generation + u64::from(self.changed_at.is_some_and(|at| self.native.calls.len() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.native.close().await
    }
}
fn reader(
    rows: Vec<ReadResult<Vec<Rec>>>,
    generation: Option<u64>,
    changed_at: Option<usize>,
) -> ActivityReader<ConnectionRead> {
    ActivityReader::new(
        ConnectionRead { native: FakeRead::new(rows), generation, changed_at },
        ReadOptions { budget: CollectionBudget::default(), identity: IdentityDetail::Endpoint },
        "fixture.invalid".into(),
        1433,
    )
}
async fn authority(generation: Option<u64>) -> LookupContext {
    let mut reader = reader(
        vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![boxed_context()]), Ok(vec![tempdb(None)])],
        generation,
        None,
    );
    let context = context::initial_scan(&mut reader, true).await.unwrap();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    context
}
fn primary_header(page: u32, page_type: i64) -> Rec {
    replace(
        &tempdb_header(0),
        &[
            ("target_page_id", Val::Int(i64::from(page))),
            ("page_id", Val::Int(i64::from(page))),
            ("file_guid", Val::Null),
            ("file_create_lsn", Val::Null),
            ("page_header_version", Val::Null),
            ("page_type", Val::Int(page_type)),
            ("catalog_object_id", Val::Null),
        ],
    )
}
fn witnessed(context: &LookupContext, row: &Rec) -> HeaderRecord {
    let mut header = HeaderRecord::decode(row).unwrap();
    header.file_epoch = FileEpoch::select(context, &header);
    header
}

#[tokio::test]
async fn qualified_primary_maps_keep_null_file_facts_and_their_explicit_lifetime_basis() {
    let context = authority(Some(7)).await;
    for (page, native_type, kind) in
        [(1, 11, PageWaitTargetKind::Pfs), (2, 8, PageWaitTargetKind::Gam), (3, 9, PageWaitTargetKind::Sgam)]
    {
        let header = witnessed(&context, &primary_header(page, native_type));
        assert!(header.same_mapping(&header));
        let target = page_targets::mapped_target(&context, &header).unwrap();
        assert_eq!(target.identity_basis, PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch);
        assert_eq!(target.kind, kind);
        assert_eq!(target.file.id, "1");
        assert_eq!(target.file.qualifiers["kind"], MSSQL_TEMPDB_PRIMARY_FILE_KIND);
        assert_eq!(target.file.qualifiers["file_type"], "0");
        assert_eq!(target.file.qualifiers["file_state"], "0");
        assert!(!target.file.qualifiers.contains_key("create_lsn"));
        assert!(header.catalog.file_guid.is_none() && header.catalog.file_create_lsn.is_none());
        assert!(target.is_valid() && target.object.is_none());
    }
}

#[tokio::test]
async fn missing_guids_do_not_acquire_a_primary_lifetime_from_another_file_role_or_unknown_layout() {
    let context = authority(Some(7)).await;
    for change in [
        "file",
        "app",
        "iam",
        "heap",
        "leaf",
        "unknown-layout",
        "object",
        "native-object",
        "creation-lsn",
        "offline",
        "log",
        "missing-state",
    ] {
        let mut row = primary_header(1, 11);
        let changes = match change {
            "file" => vec![("target_file_id", Val::Int(2)), ("file_id", Val::Int(2))],
            "app" => vec![("target_database_id", Val::Int(7)), ("database_id", Val::Int(7))],
            "iam" => vec![("page_type", Val::Int(10))],
            "heap" => vec![("page_type", Val::Int(1)), ("index_id", Val::Int(0))],
            "leaf" => vec![("page_type", Val::Int(2)), ("page_level", Val::Int(0)), ("index_id", Val::Int(1))],
            "unknown-layout" => vec![("page_header_version", Val::Int(999))],
            "object" => vec![("catalog_object_id", Val::Int(123))],
            "native-object" => vec![("object_id", Val::Int(123))],
            "creation-lsn" => vec![("file_create_lsn", Val::Text("0".into()))],
            "offline" => vec![("file_state", Val::Int(6))],
            "log" => vec![("file_type", Val::Int(1))],
            _ => vec![("file_state", Val::Null)],
        };
        row = replace(&row, &changes);
        let header = witnessed(&context, &row);
        assert!(header.file_epoch.is_none(), "{change}");
        assert!(!header.same_mapping(&header), "{change}");
        assert!(page_targets::mapped_target(&context, &header).is_err(), "{change}");
    }
    let unknown = authority(None).await;
    assert!(witnessed(&unknown, &primary_header(1, 11)).file_epoch.is_none());
    for missing in ["process", "shared", "permission", "version", "managed"] {
        let mut unknown = context.clone();
        match missing {
            "process" => unknown.authority.instance = None,
            "shared" => unknown.shared = None,
            "permission" => unknown.unavailable = Some(PageWaitResolution::PermissionDenied),
            "version" => unknown.authority.activity.major = 14,
            _ => unknown.authority.activity.edition = 8,
        }
        assert!(witnessed(&unknown, &primary_header(1, 11)).file_epoch.is_none(), "{missing}");
    }
}

#[tokio::test]
async fn runtime_guid_and_primary_lifetime_cannot_substitute_for_each_other_at_mapping_bounds() {
    let context = authority(Some(7)).await;
    let primary = witnessed(&context, &primary_header(1, 11));
    let native =
        witnessed(&context, &replace(&primary_header(1, 11), &[("file_guid", Val::Text("native-file".into()))]));
    assert!(native.file_epoch.is_some());
    assert!(!primary.same_mapping(&native));
    assert!(!native.same_mapping(&primary));
    let another = authority(Some(8)).await;
    assert!(!primary.same_mapping(&witnessed(&another, &primary_header(1, 11))));
}

fn capture_replies() -> Vec<ReadResult<Vec<Rec>>> {
    let mut scripted = replies();
    let wait = replace(&page_wait(42, "PAGELATCH_EX", "2:1:1"), &[("edition", Val::Int(3))]);
    scripted[4] = Ok(vec![wait.clone()]);
    for index in [5, 9] {
        scripted[index] = Ok(vec![participant(0, &wait)]);
    }
    for index in [6, 8] {
        scripted[index] = Ok(vec![primary_header(1, 11)]);
    }
    scripted.extend([
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![boxed_context()]),
        Ok(vec![replace(&tempdb(Some(1)), &[("file_guid", Val::Null), ("file_create_lsn", Val::Null)])]),
    ]);
    scripted
}

#[tokio::test]
async fn shared_capture_and_final_file_verification_use_the_same_null_preserving_witness() {
    let resource = resource().await;
    let mut reader = reader(capture_replies(), Some(7), None);
    let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await.unwrap();
    let verified = reader.verify_page_wait_context(&captured.guard).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let row = &captured.snapshot.observations[0];
    assert_eq!(row.request.database.as_deref(), Some("shop"));
    assert_eq!(row.target.as_ref().unwrap().identity_basis, PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch);
    assert!(verified.unwrap());
    assert_eq!(reader.query_count(), 18);
    let Parameter::Text(files) = &reader.io.native.calls.last().unwrap().parameters[0] else { panic!("file roster") };
    assert_eq!(files, "<files><file id=\"1\"/></files>");
}

#[tokio::test]
async fn connection_and_tempdb_incarnation_changes_cannot_publish_a_primary_file_lifetime() {
    let resource = resource().await;
    for cause in ["connection", "created", "process", "permission"] {
        let mut scripted = capture_replies();
        let changed_at = (cause == "connection").then_some(8);
        match cause {
            "created" => {
                scripted[13] = Ok(vec![replace(
                    &tempdb(None),
                    &[("tempdb_created_without_offset", Val::Text("2020-01-01T04:01:00".into()))],
                )])
            }
            "process" => scripted[11] = Ok(vec![epoch(200)]),
            "permission" => scripted[13] = Ok(vec![replace(&tempdb(None), &[("tempdb_page_permission", Val::Int(0))])]),
            _ => {}
        }
        let mut reader = reader(scripted, Some(7), changed_at);
        let captured = reader.capture_shared_tempdb_page_wait_targets(&resource).await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(CollectionStop::from_error(&captured.err().expect(cause)), CollectionStop::SourceChanged, "{cause}");
    }
}
