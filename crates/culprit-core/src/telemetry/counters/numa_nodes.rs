//! CPU/SQLOS nodes and physical NUMA memory nodes are independent native populations.
use super::*;
pub const MSSQL_NUMA_NODES_SOURCE: &str = "mssql-numa-node-state";
pub const MSSQL_CPU_NODES_SOURCE: &str = "cpu-node-state";
pub const MSSQL_MEMORY_NODES_SOURCE: &str = "memory-node-state";
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlNumaNodeKind {
    Cpu,
    Memory,
}
impl MssqlNumaNodeKind {
    pub const ALL: [Self; 2] = [Self::Cpu, Self::Memory];
    pub fn object(self) -> &'static str {
        match self {
            Self::Cpu => "sys.dm_os_nodes",
            Self::Memory => "sys.dm_os_memory_nodes",
        }
    }
    pub fn source(self) -> &'static str {
        match self {
            Self::Cpu => MSSQL_CPU_NODES_SOURCE,
            Self::Memory => MSSQL_MEMORY_NODES_SOURCE,
        }
    }
    pub fn fields(self) -> &'static [MssqlNativeField] {
        match self {
            Self::Cpu => CPU_FIELDS,
            Self::Memory => MEMORY_FIELDS,
        }
    }
    pub fn identity_column(self) -> &'static str {
        match self {
            Self::Cpu => "node_id",
            Self::Memory => "memory_node_id",
        }
    }
    pub fn from_sample(sample: &NativeCounterSet) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| sample.identity.qualifiers.get("native_object").map(String::as_str) == Some(kind.object()))
            .ok_or_else(invalid)
    }
}
use CounterUnit::{Count, Kibibytes};
use MssqlNativeFieldKind::{BooleanProperty, Identity, IntegerProperty, TextProperty};
const fn property(name: &'static str, kind: MssqlNativeFieldKind, required_column: bool) -> MssqlNativeField {
    MssqlNativeField { name, kind, required_column, introduced_in: None }
}
const fn gauge(name: &'static str, unit: CounterUnit) -> MssqlNativeField {
    MssqlNativeField::counter(name, unit, CounterNature::Gauge)
}
const CPU_FIELDS: &[MssqlNativeField] = &[
    property("node_id", Identity, true),
    property("node_state_desc", TextProperty, true),
    property("memory_node_id", IntegerProperty, false),
    property("cpu_affinity_mask", IntegerProperty, false),
    gauge("online_scheduler_count", Count),
    gauge("idle_scheduler_count", Count),
    gauge("active_worker_count", Count),
    gauge("avg_load_balance", Count),
    property("timer_task_affinity_mask", IntegerProperty, false),
    property("permanent_task_affinity_mask", IntegerProperty, false),
    property("resource_monitor_state", BooleanProperty, false),
    property("online_scheduler_mask", IntegerProperty, false),
    property("processor_group", IntegerProperty, false),
    gauge("cpu_count", Count),
];
const MEMORY_FIELDS: &[MssqlNativeField] = &[
    property("memory_node_id", Identity, true),
    gauge("virtual_address_space_reserved_kb", Kibibytes),
    gauge("virtual_address_space_committed_kb", Kibibytes),
    gauge("locked_page_allocations_kb", Kibibytes),
    gauge("pages_kb", Kibibytes),
    gauge("shared_memory_reserved_kb", Kibibytes),
    gauge("shared_memory_committed_kb", Kibibytes),
    gauge("foreign_committed_kb", Kibibytes),
    MssqlNativeField::counter_since("target_kb", Kibibytes, CounterNature::Gauge, 13),
];
pub fn numa_node_candidate(sample: &NativeCounterSet) -> bool {
    MssqlNumaNodeKind::from_sample(sample).is_ok()
        || sample
            .counters
            .iter()
            .any(|counter| MssqlNumaNodeKind::ALL.into_iter().any(|kind| counter.native_object == kind.object()))
}
pub struct MssqlNumaNodePoint<'a> {
    kind: MssqlNumaNodeKind,
    fields: MssqlNativeFieldRoster<'a>,
}
impl<'a> MssqlNumaNodePoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        let kind = MssqlNumaNodeKind::from_sample(sample)?;
        if sample.identity.id.parse::<i16>().is_err() || sample.identity.id.starts_with('-') {
            return Err(invalid());
        }
        let fields = MssqlNativeFieldRoster::from_sample(sample, kind.object(), kind.fields())?;
        for field in kind.fields().iter().filter(|field| {
            matches!(field.name, "memory_node_id" | "processor_group")
                && !matches!(field.kind, MssqlNativeFieldKind::Identity)
        }) {
            if let MssqlNativeFieldValue::Observed(value) = fields.field(field)?
                && !value.parse::<i16>().is_ok_and(|value| value >= 0)
            {
                return Err(invalid());
            }
        }

        Ok(Self { kind, fields })
    }
    pub fn kind(&self) -> MssqlNumaNodeKind {
        self.kind
    }
    pub fn node_id(&self) -> &'a str {
        self.fields.native_id()
    }
    pub fn sample(&self) -> &'a NativeCounterSet {
        self.fields.sample()
    }
    pub fn field(&self, field: &MssqlNativeField) -> Result<MssqlNativeFieldValue<'a>> {
        self.fields.field(field)
    }
}
pub fn numa_node_header_is_valid(source: &CollectionDescription) -> bool {
    let SourceObservation::Snapshot(header) = &source.observation else { return false };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: header.observed_from.clone(),
        observed_until: header.observed_until.clone(),
        database_writable: None,
    };
    source.source == MSSQL_NUMA_NODES_SOURCE
        && header.dataset == SnapshotDataset::NumaNodes
        && header.is_valid()
        && header.sources.len() == 2
        && MssqlNumaNodeKind::ALL
            .into_iter()
            .all(|kind| header.sources.iter().any(|coverage| coverage.source == kind.source()))
        && context.has_qualified_local_owner()
        && matches!((&context.target,&context.partition),(CollectionTarget::Shared(target),SourcePartition::InstanceLocal{member:Some(member),..}) if target==member && target.engine==crate::Engine::Mssql && target.kind==SharedResourceKind::Instance)
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
