//! Conditional object operations. Native transports own HTTP and receive credentials from the host.
use crate::delivery::recovery::RecoveryReader;
use async_trait::async_trait;
use culprit_core::Result;
use std::path::Path;

pub struct ObjectRead {
    pub etag: String,
    pub bytes: u64,
    pub body: RecoveryReader,
}

pub enum WriteCondition<'a> {
    Absent,
    Matches(&'a str),
}

pub enum ObjectWrite {
    Written,
    ConditionNotMet,
}

/// One transport is bound to one account/container/prefix. Keys are relative to that namespace.
/// Implementations disable redirects and automatic write retries, bound response bodies, and stream files.
/// A write whose acknowledgement is lost returns an error; callers reconcile durable state before retrying.
#[async_trait]
pub trait ObjectTransport: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<ObjectRead>>;
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<ObjectWrite>;
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<ObjectWrite>;
}

/// Generated recovery keys are relative and canonical; no query, traversal, escaping or empty path component.
pub fn valid_object_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && key.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
}
