//! Bounded investigation of a specifically selected committed capture. Native manifests and immutable projection
//! indexes establish trust; a warehouse row never becomes an arbitrary Snapshot or a claim of current state.
mod decode;
mod reader;
mod response_bytes;
mod types;

pub use reader::EvidenceReader;
pub use types::*;

#[cfg(test)]
mod tests;
