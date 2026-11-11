//! Source-independent telemetry capture and durable delivery. Authentication belongs to the embedding engine.
pub mod admission;
mod blocking_state;
pub mod capture;
mod collection_activity;
mod collection_files;
mod collection_references;
mod durable_fs;
pub mod outbox;

pub mod delivery;
pub mod fleet;
pub mod health;
pub mod service;

pub mod lifecycle;
