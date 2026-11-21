//! One exact original group/member corpus drives SDK, stores and local consumers without a provider.
use super::*;
use culprit_core::{Engine, ErrorCode, SourceState};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupReplicationCase {
    Queued,
    OnlineZero,
    NativeNulls,
    Recovering,
    MemberError,
    Unreachable,
    Empty,
    StatisticsDenied,
    MembersDenied,
    Disabled,
    UnsupportedLayout,
    Partial,
}
impl GroupReplicationCase {
    pub const ALL: [Self; 12] = [
        Self::Queued,
        Self::OnlineZero,
        Self::NativeNulls,
        Self::Recovering,
        Self::MemberError,
        Self::Unreachable,
        Self::Empty,
        Self::StatisticsDenied,
        Self::MembersDenied,
        Self::Disabled,
        Self::UnsupportedLayout,
        Self::Partial,
    ];
}
pub fn group_replication_evidence(case: GroupReplicationCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let group = "aaaaaaaa-0000-4000-8000-000000000001";
    let member = "bbbbbbbb-0000-4000-8000-000000000002";
    let instance = SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", member),
        native: NativeIdentity::new(member),
    };
    let mut metadata = MysqlGroupReplicationMetadata {
        observed_at: "2020-01-01T00:00:00Z".into(),
        group_id: Some(group.into()),
        plugin_status: Some("ACTIVE".into()),
        members_layout: true,
        statistics_layout: true,
    };
    if case == GroupReplicationCase::Disabled {
        metadata.plugin_status = None;
        metadata.group_id = None;
    }
    if case == GroupReplicationCase::UnsupportedLayout {
        metadata.members_layout = false;
        metadata.statistics_layout = false;
    }
    let unavailable = matches!(case, GroupReplicationCase::Disabled | GroupReplicationCase::UnsupportedLayout);
    let mut points = Vec::new();
    if !unavailable && !matches!(case, GroupReplicationCase::Empty | GroupReplicationCase::MembersDenied) {
        let state = match case {
            GroupReplicationCase::Recovering => "RECOVERING",
            GroupReplicationCase::MemberError => "ERROR",
            GroupReplicationCase::Unreachable => "UNREACHABLE",
            _ => "ONLINE",
        };
        points.push(MysqlGroupReplicationPoint::Member(Box::new(MysqlGroupMember {
            group_id: group.into(),
            member_id: member.into(),
            channel: "group_replication_applier".into(),
            observed_at: "2020-01-01T00:00:00.4Z".into(),
            state: state.into(),
            role: (case != GroupReplicationCase::NativeNulls).then(|| "SECONDARY".into()),
            version: Some("8.4.7".into()),
        })));
    }
    if !unavailable && !matches!(case, GroupReplicationCase::Empty | GroupReplicationCase::StatisticsDenied) {
        let value = |n: &str| {
            if case == GroupReplicationCase::NativeNulls {
                None
            } else if case == GroupReplicationCase::OnlineZero {
                Some("0".into())
            } else {
                Some(n.into())
            }
        };
        points.push(MysqlGroupReplicationPoint::Statistics(Box::new(MysqlGroupStatistics {
            group_id: group.into(),
            member_id: member.into(),
            channel: "group_replication_applier".into(),
            observed_at: "2020-01-01T00:00:00.5Z".into(),
            view_id: Some("view-original".into()),
            certification_queue: value("2"),
            applier_queue: value("3"),
            rows_validating: value("4"),
            checked: value("9007199254740993"),
            conflicts: value("7"),
            remote_applied: value("13"),
            local_proposed: value("17"),
            local_rollback: value("1"),
        })));
    }
    let records = points
        .into_iter()
        .map(|point| {
            TelemetryRecord::Operational(OperationalObservation {
                source: point.source().into(),
                key: point.key().unwrap(),
                fact: OperationalFact::MysqlGroupReplication(Box::new(point)),
            })
        })
        .collect::<Vec<_>>();
    let sources = [MYSQL_GROUP_MEMBERS_SOURCE, MYSQL_GROUP_STATISTICS_SOURCE]
        .into_iter()
        .map(|name| {
            let denied = (name == MYSQL_GROUP_MEMBERS_SOURCE && case == GroupReplicationCase::MembersDenied)
                || (name == MYSQL_GROUP_STATISTICS_SOURCE && case == GroupReplicationCase::StatisticsDenied);
            if denied || unavailable {
                return StateSourceCoverage::unavailable(
                    name.into(),
                    if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported },
                    None,
                );
            }
            let count = records
                .iter()
                .filter(|record| matches!(record, TelemetryRecord::Operational(row) if row.source == name))
                .count() as u32;
            let mut source = StateSourceCoverage::observed(name.into(), count, case != GroupReplicationCase::Partial);
            if case == GroupReplicationCase::Partial {
                source.state = SourceState::Partial;
                source.stopped = Some(CollectionStop::RowLimit);
            }
            source
        })
        .collect();
    let description = CollectionDescription {
        target: CollectionTarget::Shared(instance.clone()),
        partition: SourcePartition::InstanceLocal {
            member: Some(instance),
            epoch: Some("private-observer-process-canary".into()),
        },
        source: MYSQL_GROUP_REPLICATION_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::GroupReplication {
            metadata: Box::new(metadata),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::GroupReplication,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: records.len() as u32,
                sources,
            },
        },
    };
    assert!(group_replication_context(&description).is_ok());
    for record in &records {
        assert!(
            ExportedRecord::new(
                &description,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                record.clone()
            )
            .is_ok()
        );
    }
    (description, records)
}
