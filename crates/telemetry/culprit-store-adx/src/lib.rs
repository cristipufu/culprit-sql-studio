//! ADX delivery contracts and protocol validation. Provisioning belongs to deployment tooling.
pub mod authentication;
pub mod catalog;
pub mod client;
pub mod control;
pub mod deployment;
pub mod evidence;
mod http;
pub mod ingestion;
pub mod projection;
pub mod queued;
pub mod ready;
pub mod receipts;
pub mod response;
pub mod schema;
pub mod sink;

#[cfg(test)]
mod emulator;
#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "../../../../tests/support/http.rs"]
mod http_fixture;

#[cfg(test)]
mod encoding_tests;
