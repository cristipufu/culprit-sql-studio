//! Conditional object operations. Authentication and HTTP belong to the host application.
use crate::delivery::recovery::RecoveryReader;
use async_trait::async_trait;
use culprit_core::Result;
use std::path::Path;

pub struct BlobRead {
    pub etag: String,
    pub bytes: u64,
    pub body: RecoveryReader,
}

pub enum WriteCondition<'a> {
    Absent,
    Matches(&'a str),
}

pub enum BlobWrite {
    Written,
    ConditionNotMet,
}

/// One transport is bound to one account/container/prefix. Keys are relative to that namespace.
/// Implementations disable redirects and automatic write retries, bound response bodies, and stream files.
/// A write whose acknowledgement is lost returns an error; callers reconcile durable state before retrying.
#[async_trait]
pub trait BlobTransport: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<BlobRead>>;
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite>;
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<BlobWrite>;
}
