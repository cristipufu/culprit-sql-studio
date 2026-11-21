//! Native replication observations retain direction, identity and unknown values before any UI projection.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct AvailabilityReplicaSample {
    pub group_id: String,
    pub group_database_id: String,
    pub replica_id: String,
    pub database_id: Option<u32>,
    pub database_name: Option<String>,
    /// Group/replica health rolls up other databases too; it is contextual, not this database's work.
    pub group: Option<Box<AvailabilityGroupContext>>,
    pub replica: Option<Box<AvailabilityReplicaContext>>,
    pub native_name: Option<String>,
    pub is_local: Option<bool>,
    pub is_primary: Option<bool>,
    pub state: Option<u8>,
    pub native_state: Option<String>,
    pub mode: Option<u8>,
    pub native_mode: Option<String>,
    pub is_commit_participant: Option<bool>,
    pub database_state: Option<u8>,
    pub native_database_state: Option<String>,
    pub is_suspended: Option<bool>,
    pub suspend_reason: Option<u8>,
    pub synchronization_health: Option<u8>,
    pub native_synchronization_health: Option<String>,
    pub native_suspend_reason: Option<String>,
    pub log_send_queue_kb: Option<String>,
    pub redo_queue_kb: Option<String>,
    pub log_send_rate_kb_per_second: Option<String>,
    pub redo_rate_kb_per_second: Option<String>,
    pub secondary_lag_seconds: Option<String>,
    pub filestream_send_rate_kb_per_second: Option<String>,
    pub low_water_mark_for_ghosts: Option<String>,
    /// Numeric(25,0) native log positions. Only last_redone_lsn/last_commit_lsn are actual LSNs;
    /// the others are padded log-block identifiers. None supports byte subtraction.
    pub recovery_lsn: Option<String>,
    pub truncation_lsn: Option<String>,
    pub last_sent_lsn: Option<String>,
    pub last_received_lsn: Option<String>,
    pub last_hardened_lsn: Option<String>,
    pub last_redone_lsn: Option<String>,
    pub end_of_log_lsn: Option<String>,
    pub last_commit_lsn: Option<String>,
    /// Native SQL Server datetime values have no recorded UTC offset.
    pub last_sent_without_offset: Option<String>,
    pub last_received_without_offset: Option<String>,
    pub last_hardened_without_offset: Option<String>,
    pub last_redone_without_offset: Option<String>,
    pub last_commit_without_offset: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct AvailabilityGroupContext {
    pub catalog_observed: bool,
    pub state_observed: bool,
    pub name: Option<String>,
    pub primary_replica: Option<String>,
    pub primary_recovery_health: Option<u8>,
    pub native_primary_recovery_health: Option<String>,
    pub secondary_recovery_health: Option<u8>,
    pub native_secondary_recovery_health: Option<String>,
    pub synchronization_health: Option<u8>,
    pub native_synchronization_health: Option<String>,
    pub failure_condition_level: Option<u8>,
    pub health_check_timeout_ms: Option<String>,
    pub automated_backup_preference: Option<u8>,
    pub native_automated_backup_preference: Option<String>,
    pub is_distributed: Option<bool>,
    pub is_basic: Option<bool>,
    pub dtc_support: Option<bool>,
    pub database_failover: Option<bool>,
    pub cluster_type: Option<u8>,
    pub native_cluster_type: Option<String>,
    pub required_synchronized_secondaries_to_commit: Option<String>,
    pub configuration_sequence: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct AvailabilityReplicaContext {
    pub catalog_observed: bool,
    pub state_observed: bool,
    pub role: Option<u8>,
    pub native_role: Option<String>,
    pub operational_state: Option<u8>,
    pub native_operational_state: Option<String>,
    pub connected_state: Option<u8>,
    pub native_connected_state: Option<String>,
    pub recovery_health: Option<u8>,
    pub native_recovery_health: Option<String>,
    pub synchronization_health: Option<u8>,
    pub native_synchronization_health: Option<String>,
    /// Native numeric metadata only; error descriptions can contain private host and login details.
    pub last_connect_error_number: Option<i32>,
    pub last_connect_error_without_offset: Option<String>,
    pub failover_mode: Option<u8>,
    pub native_failover_mode: Option<String>,
    pub session_timeout_seconds: Option<String>,
    pub primary_role_allow_connections: Option<u8>,
    pub native_primary_role_allow_connections: Option<String>,
    pub secondary_role_allow_connections: Option<u8>,
    pub native_secondary_role_allow_connections: Option<String>,
    pub seeding_mode: Option<u8>,
    pub native_seeding_mode: Option<String>,
    pub endpoint_url: Option<String>,
    pub read_only_routing_url: Option<String>,
    pub read_write_routing_url: Option<String>,
    pub backup_priority: Option<u8>,
    pub created_without_offset: Option<String>,
    pub modified_without_offset: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct GeoReplicationSample {
    pub link_id: String,
    pub partner_server: Option<String>,
    pub partner_database: Option<String>,
    /// Native local-endpoint role: 0 primary, 1 secondary. Other codes remain unknown.
    pub role: Option<u8>,
    pub native_role: Option<String>,
    pub state: Option<u8>,
    pub native_state: Option<String>,
    /// These measurements are available on the primary only; absence is never zero lag.
    pub lag_seconds: Option<String>,
    pub last_replication_at: Option<String>,
    pub secondary_allow_connections: Option<u8>,
    pub native_secondary_allow_connections: Option<String>,
    /// Local endpoint's last committed transaction time, retaining its native offset as an instant.
    pub last_commit_at: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeReplicationSample {
    pub key: String,
    pub measurements: NativeCounterSet,
    /// A native sender/consumer link observed with its backend generation. Not the application query causing lag.
    pub session: Option<NativeActivityIdentity>,
    pub unresolved_session_id: Option<String>,
    pub slot_name: Option<String>,
}
impl NativeReplicationSample {
    pub fn new(measurements: NativeCounterSet) -> Result<Self> {
        if !measurements.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self { key: measurements.key()?, measurements, session: None, unresolved_session_id: None, slot_name: None })
    }
    pub fn is_valid(&self) -> bool {
        self.measurements.is_valid()
            && self.measurements.key().is_ok_and(|key| key == self.key)
            && self.session.as_ref().is_none_or(NativeActivityIdentity::is_valid)
            && self.unresolved_session_id.as_deref().is_none_or(valid_key)
            && (self.session.is_none() || self.unresolved_session_id.is_none())
            && self.slot_name.as_deref().is_none_or(valid_key)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum ReplicationFact {
    Native(Box<NativeReplicationSample>),
    AvailabilityGroup(Box<AvailabilityReplicaSample>),
    GeoLink(Box<GeoReplicationSample>),
    HyperscaleReplica(Box<MssqlHyperscaleReplica>),
}
impl ReplicationFact {
    pub fn key(&self) -> String {
        match self {
            Self::Native(row) => row.key.clone(),
            Self::AvailabilityGroup(row) => {
                format!("ag:{}:{}:{}", row.group_id, row.group_database_id, row.replica_id)
            }
            Self::GeoLink(row) => format!("geo:{}", row.link_id),
            Self::HyperscaleReplica(row) => {
                format!(
                    "hs:{}:{}:{}",
                    row.observer_replica_id.to_ascii_lowercase(),
                    row.selected_database_id,
                    row.replica_id.to_ascii_lowercase()
                )
            }
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Native(row) => row.is_valid(),
            Self::HyperscaleReplica(row) => row.is_valid(),
            Self::AvailabilityGroup(row) => {
                [&row.group_id, &row.group_database_id, &row.replica_id].into_iter().all(|id| native_guid(id))
                    && row.database_id != Some(0)
                    && row.group.as_deref().is_none_or(AvailabilityGroupContext::is_valid)
                    && row.replica.as_deref().is_none_or(AvailabilityReplicaContext::is_valid)
                    && [
                        &row.native_name,
                        &row.native_state,
                        &row.native_mode,
                        &row.database_name,
                        &row.native_database_state,
                        &row.native_synchronization_health,
                        &row.native_suspend_reason,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| native_name(value))
                    && [
                        &row.log_send_queue_kb,
                        &row.redo_queue_kb,
                        &row.log_send_rate_kb_per_second,
                        &row.redo_rate_kb_per_second,
                        &row.secondary_lag_seconds,
                        &row.filestream_send_rate_kb_per_second,
                        &row.low_water_mark_for_ghosts,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| valid_counter(value))
                    && [
                        &row.recovery_lsn,
                        &row.truncation_lsn,
                        &row.last_sent_lsn,
                        &row.last_received_lsn,
                        &row.last_hardened_lsn,
                        &row.last_redone_lsn,
                        &row.end_of_log_lsn,
                        &row.last_commit_lsn,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| log_position(value))
                    && [
                        &row.last_sent_without_offset,
                        &row.last_received_without_offset,
                        &row.last_hardened_without_offset,
                        &row.last_redone_without_offset,
                        &row.last_commit_without_offset,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| local_time(value))
            }
            Self::GeoLink(row) => {
                native_guid(&row.link_id)
                    && [
                        &row.partner_server,
                        &row.partner_database,
                        &row.native_state,
                        &row.native_role,
                        &row.native_secondary_allow_connections,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| native_name(value))
                    && row.lag_seconds.as_deref().is_none_or(valid_counter)
                    && row.last_replication_at.as_deref().is_none_or(|at| at.len() <= 64 && canonical_time(at).is_ok())
                    && row.last_commit_at.as_deref().is_none_or(|at| at.len() <= 64 && canonical_time(at).is_ok())
                    && (row.role == Some(0) || (row.lag_seconds.is_none() && row.last_replication_at.is_none()))
            }
        }
    }
}

impl AvailabilityGroupContext {
    pub fn is_valid(&self) -> bool {
        (self.catalog_observed || self.state_observed)
            && [
                &self.name,
                &self.primary_replica,
                &self.native_primary_recovery_health,
                &self.native_secondary_recovery_health,
                &self.native_synchronization_health,
                &self.native_automated_backup_preference,
                &self.native_cluster_type,
            ]
            .into_iter()
            .flatten()
            .all(|value| native_name(value))
            && [
                &self.health_check_timeout_ms,
                &self.required_synchronized_secondaries_to_commit,
                &self.configuration_sequence,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && (self.state_observed
                || (self.primary_replica.is_none()
                    && self.primary_recovery_health.is_none()
                    && self.native_primary_recovery_health.is_none()
                    && self.secondary_recovery_health.is_none()
                    && self.native_secondary_recovery_health.is_none()
                    && self.synchronization_health.is_none()
                    && self.native_synchronization_health.is_none()))
            && (self.catalog_observed
                || (self.name.is_none()
                    && self.failure_condition_level.is_none()
                    && self.health_check_timeout_ms.is_none()
                    && self.automated_backup_preference.is_none()
                    && self.native_automated_backup_preference.is_none()
                    && self.is_distributed.is_none()
                    && self.is_basic.is_none()
                    && self.dtc_support.is_none()
                    && self.database_failover.is_none()
                    && self.cluster_type.is_none()
                    && self.native_cluster_type.is_none()
                    && self.required_synchronized_secondaries_to_commit.is_none()
                    && self.configuration_sequence.is_none()))
    }
}

impl AvailabilityReplicaContext {
    pub fn is_valid(&self) -> bool {
        (self.catalog_observed || self.state_observed)
            && [
                &self.native_role,
                &self.native_operational_state,
                &self.native_connected_state,
                &self.native_recovery_health,
                &self.native_synchronization_health,
                &self.native_failover_mode,
                &self.native_primary_role_allow_connections,
                &self.native_secondary_role_allow_connections,
                &self.native_seeding_mode,
                &self.endpoint_url,
                &self.read_only_routing_url,
                &self.read_write_routing_url,
            ]
            .into_iter()
            .flatten()
            .all(|value| native_name(value))
            && self.session_timeout_seconds.as_deref().is_none_or(valid_counter)
            && [&self.last_connect_error_without_offset, &self.created_without_offset, &self.modified_without_offset]
                .into_iter()
                .flatten()
                .all(|value| local_time(value))
            && (self.state_observed
                || (self.role.is_none()
                    && self.native_role.is_none()
                    && self.operational_state.is_none()
                    && self.native_operational_state.is_none()
                    && self.connected_state.is_none()
                    && self.native_connected_state.is_none()
                    && self.recovery_health.is_none()
                    && self.native_recovery_health.is_none()
                    && self.synchronization_health.is_none()
                    && self.native_synchronization_health.is_none()
                    && self.last_connect_error_number.is_none()
                    && self.last_connect_error_without_offset.is_none()))
            && (self.catalog_observed
                || (self.failover_mode.is_none()
                    && self.native_failover_mode.is_none()
                    && self.session_timeout_seconds.is_none()
                    && self.primary_role_allow_connections.is_none()
                    && self.native_primary_role_allow_connections.is_none()
                    && self.secondary_role_allow_connections.is_none()
                    && self.native_secondary_role_allow_connections.is_none()
                    && self.seeding_mode.is_none()
                    && self.native_seeding_mode.is_none()
                    && self.endpoint_url.is_none()
                    && self.read_only_routing_url.is_none()
                    && self.read_write_routing_url.is_none()
                    && self.backup_priority.is_none()
                    && self.created_without_offset.is_none()
                    && self.modified_without_offset.is_none()))
    }
}

pub(super) fn native_name(value: &str) -> bool {
    value.len() <= 2048 && !value.chars().any(char::is_control)
}
fn log_position(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 25
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}
fn local_time(value: &str) -> bool {
    use chrono::{Datelike, Timelike};
    value.len() <= 27
        && chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
            .is_ok_and(|at| (1753..=9999).contains(&at.year()) && at.nanosecond().is_multiple_of(100))
}
pub(super) fn native_guid(id: &str) -> bool {
    id.len() == 36
        && id
            .bytes()
            .enumerate()
            .all(|(index, byte)| if [8, 13, 18, 23].contains(&index) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ReplicationObservation {
    pub source: String,
    pub fact: ReplicationFact,
}
impl ReplicationObservation {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.source)
            && self.fact.is_valid()
            && (matches!(self.fact, ReplicationFact::HyperscaleReplica(_))
                == (self.source == MSSQL_HYPERSCALE_REPLICAS_SOURCE))
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        if !self.is_valid() {
            return false;
        }
        let ReplicationFact::HyperscaleReplica(point) = &self.fact else { return true };
        let Ok(Some(scope)) = mssql_replication_scope(source) else { return false };
        NativeReadContext::from_snapshot(source).is_ok_and(|context| point.fits_read(scope, &context))
    }
}
