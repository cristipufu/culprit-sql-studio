//! Fixture workload is nonce-owned and separate from the unchanged metadata reader's accounting.
use crate::activity::{IdentityDetail, ReadOptions};
use culprit_core::telemetry::CollectionBudget;
use std::time::Duration;

mod actors;
mod drain;
mod fixture;
mod observations;
mod profile;
mod reservations;

const MAX_FIXTURE_REQUESTS: usize = 3;
const MAX_NATIVE_OBSERVATIONS: usize = 12;
const MAX_FIXTURE_MEMORY_KIB: u64 = 2 * 1024 * 1024;
const FIXTURE_ROWS: u32 = 10_000;
const SOURCE_QUERY_LIMIT: u32 = 9;
const SOURCE_ROW_LIMIT: u32 = 64;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_TIMEOUT: Duration = Duration::from_secs(180);
const SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(60);
const CASE_TIMEOUT: Duration = Duration::from_secs(300);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const ACTOR_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
const OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);
const WORKLOAD: &str = "SELECT id,payload FROM dbo.request_memory_fixture ORDER BY payload,id OPTION(MAXDOP 1,MIN_GRANT_PERCENT=70,MAX_GRANT_PERCENT=70)";

#[derive(Clone, Copy)]
enum Case {
    Reservations,
    DrainOutput,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Reservations => "grant-reservations",
            Self::DrainOutput => "grant-output-drain",
        }
    }
}

fn options() -> ReadOptions {
    ReadOptions {
        budget: CollectionBudget {
            max_queries: SOURCE_QUERY_LIMIT,
            max_snapshot_rows: SOURCE_ROW_LIMIT,
            statement_timeout_ms: 3_000,
            total_timeout_ms: 10_000,
            ..Default::default()
        },
        identity: IdentityDetail::Endpoint,
    }
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; at most three fixture sort actors; endpoint compatibility only, not production cost approval"]
async fn native_memory_grants_identify_current_reservation_holders_and_affected_waiters_without_plan_traversal() {
    Box::pin(fixture::run(Case::Reservations)).await;
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; same bounded sort and original holder drain only; twelve source points maximum; absent waiter rows never prove recovery"]
async fn draining_the_original_output_wait_holder_admits_the_same_native_waiter_without_replacement() {
    Box::pin(fixture::run(Case::DrainOutput)).await;
}
