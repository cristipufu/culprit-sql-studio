//! One MariaDB node's original Galera status snapshot. Native state UUIDs are not process/reset epochs.
use super::*;

pub const MARIADB_SERVER_UID_NAMESPACE: &str = "mariadb-server-uid";
pub const MARIADB_GALERA_SOURCE: &str = "mariadb-galera";
pub const MARIADB_GALERA_FIELDS: [&str; 17] = [
    "wsrep_ready",
    "wsrep_connected",
    "wsrep_cluster_status",
    "wsrep_local_state",
    "wsrep_local_state_comment",
    "wsrep_cluster_size",
    "wsrep_cluster_state_uuid",
    "wsrep_local_state_uuid",
    "wsrep_gcomm_uuid",
    "wsrep_local_recv_queue",
    "wsrep_local_send_queue",
    "wsrep_local_cert_failures",
    "wsrep_local_bf_aborts",
    "wsrep_local_replays",
    "wsrep_local_commits",
    "wsrep_flow_control_paused",
    "wsrep_flow_control_paused_ns",
];

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MariadbGaleraMetadata {
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub enabled: Option<bool>,
}
impl MariadbGaleraMetadata {
    pub fn is_valid(&self) -> bool {
        clock(&self.observed_at)
    }
}

/// Queue lengths are current write-set gauges. Totals/fraction have independent, unknown reset histories.
/// SHOW has no row timestamp: the native UTC endpoints bound its read, rather than inventing a point time.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MariadbGaleraPoint {
    pub observed_from: String,
    pub observed_until: String,
    /// A present variable with native NULL differs from an absent/unsupported variable.
    pub available_variables: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub ready: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub connected: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub component_status: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_state: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_state_comment: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cluster_size: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cluster_state_uuid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_state_uuid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub node_uuid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub receive_queue: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub send_queue: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub certification_failures: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub applier_aborts: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub replays: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_commits: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub paused_fraction: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub paused_nanoseconds: Option<String>,
}
impl MariadbGaleraPoint {
    pub fn is_valid(&self) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        clock(&self.observed_from)
            && clock(&self.observed_until)
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && !self.available_variables.is_empty()
            && self
                .available_variables
                .iter()
                .all(|name| MARIADB_GALERA_FIELDS.contains(&name.as_str()) && seen.insert(name))
            && self.values().into_iter().all(|(name, value)| value.is_none() || self.has(name))
            && [&self.component_status, &self.local_state_comment]
                .into_iter()
                .flatten()
                .all(|value| value.len() <= 1024)
            && [&self.cluster_state_uuid, &self.local_state_uuid, &self.node_uuid]
                .into_iter()
                .flatten()
                .all(|value| value.len() <= 128)
            && [
                &self.cluster_size,
                &self.receive_queue,
                &self.send_queue,
                &self.certification_failures,
                &self.applier_aborts,
                &self.replays,
                &self.local_commits,
                &self.paused_nanoseconds,
            ]
            .into_iter()
            .flatten()
            .all(|value| value.parse::<u64>().is_ok_and(|n| n.to_string() == *value))
            && self
                .local_state
                .as_deref()
                .is_none_or(|value| value.parse::<i64>().is_ok_and(|n| n.to_string() == value))
            && self.paused_fraction.as_deref().is_none_or(|value| {
                value.len() <= 64
                    && value.trim() == value
                    && value.parse::<f64>().is_ok_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
            })
    }
    pub fn has(&self, name: &str) -> bool {
        self.available_variables.iter().any(|field| field == name)
    }
    fn values(&self) -> [(&'static str, Option<()>); 17] {
        [
            ("wsrep_ready", self.ready.map(|_| ())),
            ("wsrep_connected", self.connected.map(|_| ())),
            ("wsrep_cluster_status", self.component_status.as_ref().map(|_| ())),
            ("wsrep_local_state", self.local_state.as_ref().map(|_| ())),
            ("wsrep_local_state_comment", self.local_state_comment.as_ref().map(|_| ())),
            ("wsrep_cluster_size", self.cluster_size.as_ref().map(|_| ())),
            ("wsrep_cluster_state_uuid", self.cluster_state_uuid.as_ref().map(|_| ())),
            ("wsrep_local_state_uuid", self.local_state_uuid.as_ref().map(|_| ())),
            ("wsrep_gcomm_uuid", self.node_uuid.as_ref().map(|_| ())),
            ("wsrep_local_recv_queue", self.receive_queue.as_ref().map(|_| ())),
            ("wsrep_local_send_queue", self.send_queue.as_ref().map(|_| ())),
            ("wsrep_local_cert_failures", self.certification_failures.as_ref().map(|_| ())),
            ("wsrep_local_bf_aborts", self.applier_aborts.as_ref().map(|_| ())),
            ("wsrep_local_replays", self.replays.as_ref().map(|_| ())),
            ("wsrep_local_commits", self.local_commits.as_ref().map(|_| ())),
            ("wsrep_flow_control_paused", self.paused_fraction.as_ref().map(|_| ())),
            ("wsrep_flow_control_paused_ns", self.paused_nanoseconds.as_ref().map(|_| ())),
        ]
    }
    pub fn source(&self) -> &'static str {
        MARIADB_GALERA_SOURCE
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        Ok(MARIADB_GALERA_SOURCE.into())
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let Ok(context) = galera_context(source) else { return false };
        let SourceObservation::Galera { snapshot, metadata } = &source.observation else { return false };
        self.is_valid()
            && self.observed_from == snapshot.observed_from
            && self.observed_until == snapshot.observed_until
            && metadata.enabled == Some(true)
            && snapshot.sources[0].records == 1
            && matches!(snapshot.sources[0].state, crate::SourceState::Collected | crate::SourceState::Partial)
            && (snapshot.sources[0].state != crate::SourceState::Collected
                || snapshot.sources[0].complete_scope && self.available_variables.len() == MARIADB_GALERA_FIELDS.len())
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_from) && bounds.contains(&self.observed_until))
    }
}
pub fn galera_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::Galera
        && snapshot.records <= 1
        && matches!(snapshot.sources.as_slice(), [source] if source.source == MARIADB_GALERA_SOURCE && source.records == snapshot.records)
}
pub fn galera_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::Galera { snapshot, metadata } = &source.observation else { return Err(invalid()) };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: None,
    };
    if source.source != MARIADB_GALERA_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !galera_header_is_valid(snapshot)
        || snapshot.sources[0].state == crate::SourceState::Empty
        || !metadata.is_valid()
        || metadata.observed_at != snapshot.observed_from
        || !context.has_qualified_local_owner()
        || !matches!((&context.target, &context.partition),
            (CollectionTarget::Shared(resource), SourcePartition::InstanceLocal { member: Some(member), .. })
            if resource == member && resource.engine == crate::Engine::Mysql && resource.kind == SharedResourceKind::Instance
                && resource.namespace.id == MARIADB_SERVER_UID_NAMESPACE)
        || !ObservationBounds::new(&context.observed_from, &context.observed_until)
            .is_some_and(|bounds| bounds.contains(&metadata.observed_at))
        || (snapshot.records > 0
            || matches!(snapshot.sources[0].state, crate::SourceState::Collected | crate::SourceState::Empty))
            && metadata.enabled != Some(true)
        || snapshot.sources[0].state == crate::SourceState::NotApplicable && metadata.enabled != Some(false)
    {
        return Err(invalid());
    }
    Ok(context)
}
fn clock(value: &str) -> bool {
    value.len() <= 64 && ObservationBounds::new(value, value).is_some()
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
