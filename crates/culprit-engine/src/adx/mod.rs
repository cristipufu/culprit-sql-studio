//! ADX delivery contracts and protocol validation. Provisioning belongs to deployment tooling.
pub mod catalog;
pub mod client;
pub mod control;
pub mod deployment;
pub mod encoding;
pub mod envelope;
pub mod indexes;
mod prepared;
pub mod projection;
pub mod ready;
pub mod receipts;
pub mod response;
pub mod schema;
pub mod sink;

#[cfg(test)]
mod emulator;
#[cfg(test)]
mod test_support;
