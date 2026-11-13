//! What is built on a report, adding no findings of its own: the dashboard, the index advisor, the digest for scheduled
//! jobs, the agent brief, drift between two databases, and the waits explained. Each follows the same evidence rules
//! as the checks.

pub(crate) mod advisor;
pub(crate) mod brief;
pub mod dashboard;
pub(crate) mod digest;
pub mod drift;
pub mod memory_failures;
mod memory_participants;
pub mod native_failures;
pub mod page_waits;
pub(crate) mod plan_advice;
pub mod plan_view;
pub mod request_memory;
pub mod session_memory;
pub mod sql_lint;
pub mod statement_work;
pub mod temporary_work;
pub mod waits;
