//! ClickHouse analytical delivery with shared preparation, exact receipts and publication ownership.
pub mod catalog;
pub mod client;
pub mod deployment;
#[cfg(test)]
mod functional;
#[cfg(test)]
#[path = "../../../../tests/support/http.rs"]
mod http_fixture;
pub mod response;
pub mod schema;
pub mod sink;
