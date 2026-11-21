//! One original MariaDB node corpus for all offline consumers, without a provider/native qualification claim.
use super::*;
use culprit_core::{Engine, ErrorCode, SourceState};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaleraCase {
    Queued,
    Zero,
    NativeNulls,
    MissingVariable,
    Disconnected,
    NonPrimary,
    Disabled,
    UnknownConfiguration,
    UnsupportedLayout,
    Denied,
    Partial,
}
impl GaleraCase {
    pub const ALL: [Self; 11] = [
        Self::Queued,
        Self::Zero,
        Self::NativeNulls,
        Self::MissingVariable,
        Self::Disconnected,
        Self::NonPrimary,
        Self::Disabled,
        Self::UnknownConfiguration,
        Self::UnsupportedLayout,
        Self::Denied,
        Self::Partial,
    ];
}
pub fn galera_evidence(case: GaleraCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let instance = SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mariadb-server-uid"),
        native: NativeIdentity::new("iFL88RG7XPipmQ5HOnA9iLqJw6w="),
    };
    let enabled = match case {
        GaleraCase::Disabled => Some(false),
        GaleraCase::UnknownConfiguration => None,
        _ => Some(true),
    };
    let absent = matches!(
        case,
        GaleraCase::Disabled | GaleraCase::UnknownConfiguration | GaleraCase::UnsupportedLayout | GaleraCase::Denied
    );
    let mut point = MariadbGaleraPoint {
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        available_variables: MARIADB_GALERA_FIELDS.map(str::to_owned).into(),
        ready: Some(true),
        connected: Some(true),
        component_status: Some("Primary".into()),
        local_state: Some("4".into()),
        local_state_comment: Some("Synced".into()),
        cluster_size: Some("3".into()),
        cluster_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        local_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        node_uuid: Some("bbbbbbbb-0000-4000-8000-000000000002".into()),
        receive_queue: Some("2".into()),
        send_queue: Some("3".into()),
        certification_failures: Some("9007199254740993".into()),
        applier_aborts: Some("7".into()),
        replays: Some("11".into()),
        local_commits: Some("18446744073709551615".into()),
        paused_fraction: Some("0.125000".into()),
        paused_nanoseconds: Some("123456789".into()),
    };
    if case == GaleraCase::Zero {
        point.receive_queue = Some("0".into());
        point.send_queue = Some("0".into());
        point.certification_failures = Some("0".into());
    }
    if case == GaleraCase::NativeNulls {
        point.receive_queue = None;
        point.certification_failures = None;
        point.ready = None;
    }
    if case == GaleraCase::MissingVariable {
        point.available_variables.retain(|name| name != "wsrep_local_send_queue");
        point.send_queue = None;
    }
    if case == GaleraCase::Disconnected {
        point.connected = Some(false);
        point.ready = Some(false);
        point.component_status = Some("Disconnected".into());
    }
    if case == GaleraCase::NonPrimary {
        point.component_status = Some("Non-Primary".into());
        point.local_state_comment = Some("Donor/Desynced".into());
    }
    let records = if absent {
        Vec::new()
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: MARIADB_GALERA_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MariadbGalera(Box::new(point)),
        })]
    };
    let coverage = if case == GaleraCase::Disabled {
        StateSourceCoverage {
            source: MARIADB_GALERA_SOURCE.into(),
            state: SourceState::NotApplicable,
            records: 0,
            complete_scope: false,
            error_code: None,
            stopped: None,
        }
    } else if absent {
        StateSourceCoverage::unavailable(
            MARIADB_GALERA_SOURCE.into(),
            if case == GaleraCase::Denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported },
            None,
        )
    } else {
        StateSourceCoverage::observed(
            MARIADB_GALERA_SOURCE.into(),
            1,
            !matches!(case, GaleraCase::Partial | GaleraCase::MissingVariable),
        )
    };
    let description = CollectionDescription {
        target: CollectionTarget::Shared(instance.clone()),
        partition: SourcePartition::InstanceLocal {
            member: Some(instance),
            epoch: Some("private-galera-process-canary".into()),
        },
        source: MARIADB_GALERA_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Galera {
            metadata: Box::new(MariadbGaleraMetadata { observed_at: "2020-01-01T00:00:00Z".into(), enabled }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Galera,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: records.len() as u32,
                sources: vec![coverage],
            },
        },
    };
    assert!(galera_context(&description).is_ok());
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
