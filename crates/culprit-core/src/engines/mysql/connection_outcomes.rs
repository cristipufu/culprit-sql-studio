//! The native global-status vocabulary is shared by MySQL and MariaDB, not by every SQL dialect.
use crate::{
    ConnectionOutcomeAvailability, ConnectionOutcomeMetricKind, ConnectionOutcomeMetricSpec, ConnectionOutcomeProfile,
    MeasurementScope, telemetry::NativeCounterSet,
};

const METRICS: &[ConnectionOutcomeMetricSpec] = &[
    ConnectionOutcomeMetricSpec {
        native_key: "Connections",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionAttempts,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Aborted_connects",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::FailedConnectionAttempts,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Aborted_clients",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::AbortedClientConnections,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_accept",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsAccept,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_internal",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsInternal,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_max_connections",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsMaxConnections,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_peer_address",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsPeerAddress,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_select",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsSelect,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Connection_errors_tcpwrap",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ConnectionErrorsTcpwrap,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Threads_created",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ThreadsCreated,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Threads_connected",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ThreadsConnected,
    },
    ConnectionOutcomeMetricSpec {
        native_key: "Threads_running",
        availability: ConnectionOutcomeAvailability::Supported,
        kind: ConnectionOutcomeMetricKind::ThreadsRunning,
    },
];

pub(super) fn profile(source: &str, counters: &NativeCounterSet) -> Option<ConnectionOutcomeProfile> {
    if source != "global-status" {
        return None;
    }
    let index = METRICS.iter().position(|metric| metric.native_key.eq_ignore_ascii_case(&counters.identity.id))?;
    Some(ConnectionOutcomeProfile { scope: MeasurementScope::Server, metrics: vec![METRICS[index]], observed_at: None })
}

#[cfg(test)]
mod tests;
