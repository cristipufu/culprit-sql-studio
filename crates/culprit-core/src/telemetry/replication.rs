//! Native replication observations retain direction, identity and unknown values before any UI projection.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct AvailabilityReplicaSample {
    pub group_id: String,
    pub group_database_id: String,
    pub replica_id: String,
    pub native_name: Option<String>,
    pub is_local: Option<bool>,
    pub is_primary: Option<bool>,
    pub state: Option<u8>,
    pub native_state: Option<String>,
    pub mode: Option<u8>,
    pub native_mode: Option<String>,
    pub is_suspended: Option<bool>,
    pub suspend_reason: Option<u8>,
    pub synchronization_health: Option<u8>,
    pub log_send_queue_kb: Option<String>,
    pub redo_queue_kb: Option<String>,
    pub log_send_rate_kb_per_second: Option<String>,
    pub redo_rate_kb_per_second: Option<String>,
    pub secondary_lag_seconds: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct GeoReplicationSample {
    pub link_id: String,
    pub partner_server: Option<String>,
    pub partner_database: Option<String>,
    /// Native local-endpoint role: 0 primary, 1 secondary. Other codes remain unknown.
    pub role: Option<u8>,
    pub state: Option<u8>,
    pub native_state: Option<String>,
    /// These measurements are available on the primary only; absence is never zero lag.
    pub lag_seconds: Option<String>,
    pub last_replication_at: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeReplicationSample {
    pub key: String,
    pub measurements: NativeCounterSet,
}
impl NativeReplicationSample {
    pub fn new(measurements: NativeCounterSet) -> Result<Self> {
        if !measurements.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self { key: measurements.key()?, measurements })
    }
    pub fn is_valid(&self) -> bool {
        self.measurements.is_valid() && self.measurements.key().is_ok_and(|key| key == self.key)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum ReplicationFact {
    Native(Box<NativeReplicationSample>),
    AvailabilityGroup(AvailabilityReplicaSample),
    GeoLink(GeoReplicationSample),
}
impl ReplicationFact {
    pub fn key(&self) -> String {
        match self {
            Self::Native(row) => row.key.clone(),
            Self::AvailabilityGroup(row) => {
                format!("ag:{}:{}:{}", row.group_id, row.group_database_id, row.replica_id)
            }
            Self::GeoLink(row) => format!("geo:{}", row.link_id),
        }
    }
    pub fn is_valid(&self) -> bool {
        let name = |value: &str| value.len() <= 2048;
        match self {
            Self::Native(row) => row.is_valid(),
            Self::AvailabilityGroup(row) => {
                [&row.group_id, &row.group_database_id, &row.replica_id].into_iter().all(|id| native_guid(id))
                    && [&row.native_name, &row.native_state, &row.native_mode]
                        .into_iter()
                        .flatten()
                        .all(|value| name(value))
                    && [
                        &row.log_send_queue_kb,
                        &row.redo_queue_kb,
                        &row.log_send_rate_kb_per_second,
                        &row.redo_rate_kb_per_second,
                        &row.secondary_lag_seconds,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| valid_counter(value))
            }
            Self::GeoLink(row) => {
                native_guid(&row.link_id)
                    && [&row.partner_server, &row.partner_database, &row.native_state]
                        .into_iter()
                        .flatten()
                        .all(|value| name(value))
                    && row.lag_seconds.as_deref().is_none_or(valid_counter)
                    && row.last_replication_at.as_deref().is_none_or(|at| at.len() <= 64 && canonical_time(at).is_ok())
                    && (row.role == Some(0) || (row.lag_seconds.is_none() && row.last_replication_at.is_none()))
            }
        }
    }
}
fn native_guid(id: &str) -> bool {
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
        valid_key(&self.source) && self.fact.is_valid()
    }
}
