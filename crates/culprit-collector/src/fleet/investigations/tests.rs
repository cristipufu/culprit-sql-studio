//! Runtime tests use sealed fixture evidence; no production source or native SQL is executed.
mod compiler_admission;
pub(in crate::fleet::investigations) mod page_writers;
mod request_memory;
mod runtime;
mod shared;
pub(in crate::fleet::investigations) mod support;
