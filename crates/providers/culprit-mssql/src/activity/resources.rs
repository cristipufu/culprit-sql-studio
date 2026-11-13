//! Preserve the resource reported with the request; page/object lookup is a separate bounded operation.
use culprit_core::{ObservedWaitResource, WaitResourceKind, telemetry::NativeIdentity};

pub(super) fn identify(wait_type: Option<&str>, resource: Option<&str>) -> Vec<ObservedWaitResource> {
    let kind = match wait_type {
        Some(wait) if wait.starts_with("PAGELATCH_") => WaitResourceKind::MemoryPage,
        Some(wait) if wait.starts_with("PAGEIOLATCH_") => WaitResourceKind::PageIo,
        _ => return vec![],
    };
    let Some(resource) = resource.map(str::trim).filter(|value| !value.is_empty()) else {
        return vec![];
    };
    vec![ObservedWaitResource {
        kind,
        native: NativeIdentity::new(resource).qualified("source", "sys.dm_exec_requests.wait_resource"),
    }]
}
