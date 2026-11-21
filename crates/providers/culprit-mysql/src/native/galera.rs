//! Fixed native status inventory shared by the existing scan and explicit original captures.
//! SHOW reads native metadata/counters only. No FLUSH, configuration change or cluster-address collection.
//! https://mariadb.com/docs/galera-cluster/reference/galera-cluster-status-variables
use super::{instance, invalid, one, text};
use crate::{
    connect::Flavor,
    read::QueryRead,
    rows::Rec,
    vars::{self, VariableSet},
};
use culprit_core::{Error, ErrorCode, Result, operations::ClusterHealth, telemetry::*};
use std::collections::BTreeMap;

const SCAN_FIELDS: [&str; 5] = [
    "wsrep_ready",
    "wsrep_connected",
    "wsrep_cluster_status",
    "wsrep_local_state_comment",
    "wsrep_flow_control_paused",
];

#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub instance: instance::Identity,
    pub metadata: MariadbGaleraMetadata,
}
impl Context {
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.instance == other.instance && self.metadata.enabled == other.metadata.enabled
    }
}
pub(crate) fn supported(flavor: Flavor, version: &str) -> bool {
    flavor == Flavor::Mariadb && super::local_context::has_mariadb_uid(version)
}
pub(crate) fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::message!("provider.mysql.galera.unsupported"))
}
pub(crate) async fn context(reader: &mut impl QueryRead) -> Result<Context> {
    let platform = instance::platform(reader).await?;
    if !supported(platform.flavor, &platform.version) {
        return Err(unsupported());
    }
    let configuration = one(
        reader,
        "SELECT @@GLOBAL.server_uid AS server_uid,
        (SELECT VARIABLE_VALUE FROM information_schema.GLOBAL_VARIABLES WHERE VARIABLE_NAME='WSREP_ON') AS wsrep_on,
        DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at",
        vec![],
    )
    .await?;
    let instance = platform.identify(reader, &configuration).await?;
    let metadata = MariadbGaleraMetadata {
        observed_at: text(&configuration, "observed_at")?,
        enabled: flag(configuration.native_text("wsrep_on")?.as_deref())?,
    };
    if !metadata.is_valid() {
        return Err(invalid());
    }
    Ok(Context { instance, metadata })
}

pub(crate) struct Status {
    values: BTreeMap<String, Option<String>>,
}
impl Status {
    fn decode(rows: Vec<Rec>, expected: &[&str]) -> Result<Self> {
        if rows.len() > expected.len() {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut values = BTreeMap::new();
        for row in rows {
            let name = text(&row, "variable_name")?.to_ascii_lowercase();
            if !expected.contains(&name.as_str()) || values.insert(name, row.native_text("value")?).is_some() {
                return Err(invalid());
            }
        }
        Ok(Self { values })
    }
    fn text(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|value| value.as_deref())
    }
    fn flag(&self, name: &str) -> Result<Option<bool>> {
        flag(self.text(name))
    }
    fn count(&self, name: &str) -> Result<Option<String>> {
        self.text(name).map(|value| value.parse::<u64>().map(|n| n.to_string()).map_err(|_| invalid())).transpose()
    }
    fn state(&self) -> Result<Option<String>> {
        self.text("wsrep_local_state")
            .map(|value| value.parse::<i64>().map(|n| n.to_string()).map_err(|_| invalid()))
            .transpose()
    }
    fn uuid(&self, name: &str) -> Option<String> {
        self.text(name).map(str::to_owned)
    }
    fn fraction(&self) -> Result<Option<String>> {
        self.text("wsrep_flow_control_paused")
            .map(|value| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
                    .filter(|_| value.len() <= 64 && value.trim() == value)
                    .map(|_| value.to_owned())
                    .ok_or_else(invalid)
            })
            .transpose()
    }
    pub(crate) fn point(&self, from: &str, until: &str) -> Result<MariadbGaleraPoint> {
        let point = MariadbGaleraPoint {
            observed_from: from.into(),
            observed_until: until.into(),
            available_variables: self.values.keys().cloned().collect(),
            ready: self.flag("wsrep_ready")?,
            connected: self.flag("wsrep_connected")?,
            component_status: self.text("wsrep_cluster_status").map(str::to_owned),
            local_state: self.state()?,
            local_state_comment: self.text("wsrep_local_state_comment").map(str::to_owned),
            cluster_size: self.count("wsrep_cluster_size")?,
            cluster_state_uuid: self.uuid("wsrep_cluster_state_uuid"),
            local_state_uuid: self.uuid("wsrep_local_state_uuid"),
            node_uuid: self.uuid("wsrep_gcomm_uuid"),
            receive_queue: self.count("wsrep_local_recv_queue")?,
            send_queue: self.count("wsrep_local_send_queue")?,
            certification_failures: self.count("wsrep_local_cert_failures")?,
            applier_aborts: self.count("wsrep_local_bf_aborts")?,
            replays: self.count("wsrep_local_replays")?,
            local_commits: self.count("wsrep_local_commits")?,
            paused_fraction: self.fraction()?,
            paused_nanoseconds: self.count("wsrep_flow_control_paused_ns")?,
        };
        point.is_valid().then_some(point).ok_or_else(invalid)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub(crate) fn complete(&self) -> bool {
        self.values.len() == MARIADB_GALERA_FIELDS.len()
    }
}
fn flag(value: Option<&str>) -> Result<Option<bool>> {
    value
        .map(|value| match value.to_ascii_uppercase().as_str() {
            "ON" | "1" | "TRUE" => Ok(true),
            "OFF" | "0" | "FALSE" => Ok(false),
            _ => Err(invalid()),
        })
        .transpose()
}
async fn status(reader: &mut impl QueryRead, names: &[&str]) -> Result<Status> {
    Status::decode(vars::records(reader, VariableSet::Status, names).await?, names)
}
pub(crate) async fn capture_status(reader: &mut impl QueryRead) -> Result<Status> {
    status(reader, &MARIADB_GALERA_FIELDS).await
}
pub(crate) async fn scan(reader: &mut impl QueryRead) -> Result<ClusterHealth> {
    let status = status(reader, &SCAN_FIELDS).await?;
    Ok(ClusterHealth {
        ready: status.flag("wsrep_ready")?,
        connected: status.flag("wsrep_connected")?,
        primary_component: status.text("wsrep_cluster_status").map(|value| value.eq_ignore_ascii_case("Primary")),
        local_state: status.text("wsrep_local_state_comment").map(str::to_owned),
        flow_control_paused: status.fraction()?.map(|value| value.parse().map_err(|_| invalid())).transpose()?,
    })
}
#[cfg(test)]
pub(crate) mod tests;
