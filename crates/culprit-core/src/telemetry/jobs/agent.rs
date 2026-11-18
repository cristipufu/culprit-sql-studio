//! SQL Server Agent definitions, recorded activity and retained history are distinct instance-scoped facts.
use super::*;

pub const MSSQL_AGENT_JOBS_SOURCE: &str = "mssql-agent-jobs";
pub const MSSQL_AGENT_STEPS_SOURCE: &str = "mssql-agent-job-steps";
pub const MSSQL_AGENT_HISTORY_SOURCE: &str = "mssql-agent-job-history";
pub const MSSQL_AGENT_CONTEXT_SOURCE: &str = "mssql-agent-context";
pub const MSSQL_AGENT_SOURCES: [&str; 4] =
    [MSSQL_AGENT_JOBS_SOURCE, MSSQL_AGENT_STEPS_SOURCE, MSSQL_AGENT_HISTORY_SOURCE, MSSQL_AGENT_CONTEXT_SOURCE];
pub const MAX_MSSQL_AGENT_JOB_OBSERVATIONS: usize = 1024;
pub const MAX_MSSQL_AGENT_LABEL_BYTES: usize = 1024;
pub const MAX_MSSQL_AGENT_SERVICES: usize = 3;

mod metadata;
mod points;
mod read;
pub use metadata::*;
pub use points::*;
pub use read::*;

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
fn native_label(value: &str, bytes: usize) -> bool {
    !value.is_empty() && native_text(value, bytes)
}
fn native_text(value: &str, bytes: usize) -> bool {
    value.len() <= bytes && !value.contains('\0')
}
fn nonnegative_i32(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|number| number >= 0 && number.to_string() == value)
}
fn signed_i32(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|number| number.to_string() == value)
}
fn nonzero_i32(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|number| number != 0 && number.to_string() == value)
}
fn positive_i32(value: &str) -> bool {
    nonnegative_i32(value) && value != "0"
}
fn native_local_time(value: &str) -> bool {
    value.len() <= 32 && super::super::catalog::local_time(value)
}
fn native_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[cfg(test)]
mod tests;
