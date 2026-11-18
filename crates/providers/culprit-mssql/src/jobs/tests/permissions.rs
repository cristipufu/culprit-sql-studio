use super::*;

fn native_layout(sysadmin: i64, server_permission: Val) -> Vec<Rec> {
    layout_rows()
        .iter()
        .map(|row| {
            let mut fields =
                vec![("is_sysadmin", Val::Int(sysadmin)), ("service_visibility", server_permission.clone())];
            if integer(row, "kind").unwrap() == 5 {
                fields.extend([
                    ("object_select", Val::Null),
                    ("msdb_access", Val::Null),
                    ("object_id", Val::Int(-305100784)),
                ]);
            }
            replace(row, &fields)
        })
        .collect()
}

#[tokio::test]
async fn a_native_server_grant_admits_services_with_null_object_select_without_extra_source_dispatch() {
    for sysadmin in [0, 1] {
        let mut inputs = replies();
        inputs[3] = Ok(native_layout(sysadmin, Val::Int(1)));
        inputs[9] = Ok(native_layout(sysadmin, Val::Int(1)));
        let mut io = FakeRead::new(inputs);
        let (read, _) =
            collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433).await;
        let read = read.unwrap();
        let service = read.metadata.capability(MssqlAgentObject::Services).unwrap();
        assert_eq!(service.access, MssqlAgentAccess::Available);
        assert_eq!(service.object_id.as_deref(), Some("-305100784"));
        assert!(service.columns_sha256.is_some());
        assert_eq!(read.metadata.recheck, MssqlAgentMetadataRecheck::Unchanged);
        assert_eq!(
            read.metadata.visibility,
            if sysadmin == 1 { MssqlAgentVisibility::Sysadmin } else { MssqlAgentVisibility::ExplicitSelect }
        );
        assert_eq!(io.queries(), 13);
        assert_eq!(io.closed, 1);
        assert_eq!(
            io.calls.iter().filter(|call| call.sql.contains("FROM sys.dm_server_services WHERE")).count(),
            2,
            "both existing context bounds retain their independently authorized service projection"
        );
        let points = read
            .observations
            .iter()
            .filter_map(|row| match &row.fact {
                OperationalFact::MssqlAgentContext(point) => Some(point),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(points.len(), 2);
        assert!(points.iter().all(|point| !point.services.is_empty()));
    }
}

#[tokio::test]
async fn denied_or_null_server_permission_is_not_repaired_by_sysadmin_or_table_access() {
    for sysadmin in [0, 1] {
        for permission in [Val::Int(0), Val::Null] {
            let mut io = FakeRead::new(vec![Ok(native_layout(sysadmin, permission.clone()))]);
            let metadata = layout::read(&mut io, 3, 16).await.unwrap();
            let services = metadata.capability(MssqlAgentObject::Services).unwrap();
            assert_eq!(
                services.access,
                if permission == Val::Int(0) {
                    MssqlAgentAccess::PermissionDenied
                } else {
                    MssqlAgentAccess::Unreported
                }
            );
            assert!(services.columns_sha256.is_some(), "the permission gap is independent of the valid layout");
            assert!(
                layout::OBJECTS
                    .iter()
                    .filter(|object| **object != MssqlAgentObject::Services)
                    .all(|object| { metadata.available(*object) })
            );
            assert_eq!(io.queries(), 1, "no rescue permission query is dispatched");
        }
    }
}

#[tokio::test]
async fn a_missing_relevant_server_permission_column_cannot_become_available() {
    let rows = native_layout(1, Val::Int(1))
        .iter()
        .map(|row| {
            if integer(row, "kind").unwrap() != 5 {
                return row.clone();
            }
            Rec::of(
                &row.columns()
                    .filter(|(name, _)| *name != "service_visibility")
                    .map(|(name, value)| (name, value.clone()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut io = FakeRead::new(vec![Ok(rows)]);
    assert!(layout::read(&mut io, 3, 16).await.is_err());
    assert_eq!(io.queries(), 1);
}

#[tokio::test]
async fn msdb_objects_still_require_both_native_table_select_and_database_access() {
    for kind in 0..5 {
        for field in ["object_select", "msdb_access"] {
            for value in [Val::Int(1), Val::Int(0), Val::Null] {
                let rows = native_layout(0, Val::Null)
                    .iter()
                    .map(|row| {
                        if integer(row, "kind").unwrap() == kind {
                            replace(row, &[(field, value.clone())])
                        } else {
                            row.clone()
                        }
                    })
                    .collect();
                let mut io = FakeRead::new(vec![Ok(rows)]);
                let metadata = layout::read(&mut io, 3, 16).await.unwrap();
                assert_eq!(
                    metadata.capabilities[kind as usize].access,
                    match value {
                        Val::Int(1) => MssqlAgentAccess::Available,
                        Val::Int(0) => MssqlAgentAccess::PermissionDenied,
                        _ => MssqlAgentAccess::Unreported,
                    },
                    "native object {kind}, relevant permission {field}"
                );
                assert_eq!(io.queries(), 1);
            }
        }
    }
}

#[test]
fn service_permission_uses_the_existing_native_major_version_policy() {
    let old = layout::query(15);
    assert!(old.contains("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER STATE') AS service_visibility"));
    assert!(!old.contains("VIEW SERVER SECURITY STATE"));
    for major in [16, 17] {
        assert!(
            layout::query(major)
                .contains("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER SECURITY STATE') AS service_visibility")
        );
    }
}
