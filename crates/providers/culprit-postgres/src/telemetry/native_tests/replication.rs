//! Real native conformance on the explicit nonce-owned streaming pair, never a configured user connection.
mod lifecycle;
mod permissions;
mod streaming;
mod support;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
