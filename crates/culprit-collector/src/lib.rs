//! Source-independent telemetry capture and durable delivery. Credential selection belongs to the embedding runtime; capture receives authenticated sources.
mod blocking_state;
pub mod capture;
mod collection_activity;
mod collection_files;
mod collection_references;
use culprit_read::durable_fs;
pub mod outbox;

pub mod delivery;
pub mod fleet;
pub mod health;
pub mod service;

pub mod analytical;
pub mod lifecycle;
