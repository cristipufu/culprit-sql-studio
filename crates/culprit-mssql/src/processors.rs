//! Host processors and SQL user schedulers are distinct server-scoped measurements.
use crate::read::{NativeRead, ReadError, ReadSession, integer, invalid_data};
use culprit_core::{Coverage, MeasurementScope, Result, telemetry::*};

const TOPOLOGY: &str = r#"SELECT i.cpu_count AS host_processors,s.online_schedulers,s.numa_nodes
 FROM sys.dm_os_sys_info i CROSS JOIN (
 SELECT COUNT_BIG(*) AS online_schedulers,COUNT_BIG(DISTINCT parent_node_id) AS numa_nodes
 FROM sys.dm_os_schedulers WHERE status=N'VISIBLE ONLINE' AND scheduler_id<1048576
 ) s OPTION(MAXDOP 1)"#;

pub(crate) struct Topology {
    host_processors: u64,
    online_schedulers: u64,
    numa_nodes: u64,
}
impl Topology {
    pub fn application(&self) -> (Option<f64>, Option<f64>) {
        (
            (self.online_schedulers > 0).then_some(self.online_schedulers as f64),
            (self.numa_nodes > 0).then_some(self.numa_nodes as f64),
        )
    }
    pub fn observations(&self) -> Vec<OperationalObservation> {
        [
            ("host-logical-processors", "sys.dm_os_sys_info", "cpu_count", self.host_processors),
            ("visible-online-user-schedulers", "sys.dm_os_schedulers", "online_schedulers", self.online_schedulers),
            ("visible-online-numa-nodes", "sys.dm_os_schedulers", "numa_nodes", self.numa_nodes),
        ]
        .into_iter()
        .map(|(key, object, name, value)| OperationalObservation {
            source: crate::sources::PROCESSOR_TOPOLOGY.name.into(),
            key: key.into(),
            fact: OperationalFact::Counter(NativeCounterSample::gauge(
                key,
                value,
                CounterUnit::Count,
                MeasurementScope::Server,
                object,
                name,
            )),
        })
        .collect()
    }
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Topology> {
    let rows = reader.query(TOPOLOGY, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let topology = Topology {
        host_processors: integer(row, "host_processors")? as u64,
        online_schedulers: integer(row, "online_schedulers")? as u64,
        numa_nodes: integer(row, "numa_nodes")? as u64,
    };
    if topology.host_processors == 0 || topology.numa_nodes > topology.online_schedulers {
        return Err(invalid_data());
    }
    Ok(topology)
}
pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
) -> Option<Topology> {
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let result = result.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    });
    coverage.read(&crate::sources::PROCESSOR_TOPOLOGY, result)
}

#[cfg(test)]
pub(crate) mod tests;
