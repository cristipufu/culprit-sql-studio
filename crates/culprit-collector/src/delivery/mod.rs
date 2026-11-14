//! Destination-independent recovery and analytical publication protocol.
mod archive_driver;
mod archive_worker;
pub mod committed_cache;
pub mod contract;
pub mod diagnostic_recovery;
pub mod evidence;
pub mod file_store;
mod hydration;
mod object;
pub mod object_store;
mod operation;
mod pending;
pub mod recovery;
pub mod restore;
pub mod sink;
mod verification;
pub mod worker;

#[cfg(test)]
mod tests;

pub mod driver;
pub mod journal;

pub mod file_sink;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod fault_tests;
