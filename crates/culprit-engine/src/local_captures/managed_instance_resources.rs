//! One original native history remains independent of acquisition clocks and the later scan.
use culprit_core::{ManagedInstanceResourceReport, telemetry::CapturedManagedInstanceResourceRead};
#[derive(Debug)]
pub struct LocalManagedInstanceResources {
    pub(super) connection: String,
    pub(super) read: Box<CapturedManagedInstanceResourceRead>,
    pub(super) report: Box<ManagedInstanceResourceReport>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalManagedInstanceResources {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &CapturedManagedInstanceResourceRead {
        &self.read
    }
    pub fn into_read(self) -> Box<CapturedManagedInstanceResourceRead> {
        self.read
    }
    pub fn report(&self) -> &ManagedInstanceResourceReport {
        &self.report
    }
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
