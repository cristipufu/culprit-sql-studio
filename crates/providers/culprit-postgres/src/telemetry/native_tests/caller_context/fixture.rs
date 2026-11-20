//! A nonce-admitted catalog observer and two fixture backends; no business rows or persistent objects.
use super::{
    super::{case, owned_fixture},
    actors::{Actor, SHARED_APPLICATION},
};
use crate::{
    connect::Connector,
    read::{NativeRead, QueryRead, ReadSession},
    rows::Row,
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{ConnectionSpec, Result, telemetry::*};
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{panic::AssertUnwindSafe, time::Duration};

const CASE_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_OBSERVER_QUERIES: u32 = 80;
const MAX_ACTORS: usize = 2;

pub(super) struct Fixture {
    pub specification: ConnectionSpec,
    pub cancellation: CollectionCancellation,
    connector: Connector,
    observer: ReadSession,
    gate: Option<Actor>,
    actors: Vec<Actor>,
    identities: Vec<NativeActivityIdentity>,
    actor_start_unconfirmed: bool,
    retired_gate_queries: u32,
    marker: String,
    key: i64,
}
impl Fixture {
    pub async fn run(body: for<'a> fn(&'a mut Self) -> LocalBoxFuture<'a, ()>) {
        let mut fixture = Self::open().await;
        let cancellation = fixture.cancellation.clone();
        let completion =
            case::finish(CASE_TIMEOUT, &cancellation, AssertUnwindSafe(body(&mut fixture)).catch_unwind()).await;
        let cleanup = fixture.cleanup().await;
        eprintln!(
            "owned PG caller fixture cleanup_confirmed={cleanup}; observer_queries={}; fixture_commands={}; native cost remains unqualified",
            fixture.observer.queries(),
            fixture.fixture_queries()
        );
        if let Err(panic) = completion.value {
            std::panic::resume_unwind(panic);
        }
        assert!(!completion.deadline_reached, "owned caller case deadline reached; cleanup_confirmed={cleanup}");
        assert!(cleanup, "the same native actor generations must have exited before success");
    }
    async fn open() -> Self {
        let cancellation = CollectionCancellation::default();
        let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap();
        let specification = case::admitted(&cancellation, async {
            Ok(owned_fixture::specification(&database, CollectionBudget::default()).await)
        })
        .await
        .unwrap();
        let connector = Connector::new(&specification).unwrap();
        let observer = ReadSession::new(
            connector.clone(),
            CollectionBudget {
                max_queries: MAX_OBSERVER_QUERIES,
                statement_timeout_ms: 1_000,
                total_timeout_ms: 70_000,
                max_snapshot_rows: 4,
                page_rows: 4,
                total_bytes: 128 * 1024,
                page_bytes: 16 * 1024,
                ..Default::default()
            },
        );
        let owner = owned_fixture::owner();
        let key = i64::from_str_radix(&owner[..15], 16).unwrap();
        Self {
            specification,
            cancellation,
            connector,
            observer,
            gate: None,
            actors: Vec::new(),
            identities: Vec::new(),
            actor_start_unconfirmed: false,
            retired_gate_queries: 0,
            marker: format!("culprit_owned_{owner}"),
            key,
        }
    }
    pub async fn prepare(&mut self) {
        self.open_gate().await.unwrap();
        for _ in 0..MAX_ACTORS {
            self.actor_start_unconfirmed = true;
            let actor = Actor::open(
                &self.connector,
                self.specification.database.as_deref().unwrap(),
                &self.marker,
                SHARED_APPLICATION,
                &self.cancellation,
            )
            .await
            .unwrap();
            self.identities.push(actor.identity.clone());
            self.actors.push(actor);
            self.actor_start_unconfirmed = false;
            self.actors.last_mut().unwrap().wait_on_gate(self.key).unwrap();
        }
        self.wait_for_callers(&[0, 1]).await.unwrap();
    }
    async fn open_gate(&mut self) -> Result<()> {
        case::require_active(&self.cancellation)?;
        assert!(self.gate.is_none());
        self.actor_start_unconfirmed = true;
        let gate = Actor::open(
            &self.connector,
            self.specification.database.as_deref().unwrap(),
            &self.marker,
            "fixture-caller-gate",
            &self.cancellation,
        )
        .await?;
        self.identities.push(gate.identity.clone());
        self.gate = Some(gate);
        self.actor_start_unconfirmed = false;
        self.gate.as_mut().unwrap().hold_gate(self.key).await
    }
    pub fn identity(&self, index: usize) -> &NativeActivityIdentity {
        &self.actors[index].identity
    }
    pub fn pid(&self, index: usize) -> i32 {
        self.identity(index).id.parse().unwrap()
    }
    pub async fn release_gate(&mut self) -> Result<()> {
        let mut gate = self.gate.take().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        let identity = gate.identity.clone();
        gate.close_idle()?;
        self.retired_gate_queries += gate.queries;
        self.wait_absent(&[identity]).await
    }
    pub async fn settle_callers(&mut self) -> Result<()> {
        for actor in &mut self.actors {
            actor.settle().await?;
        }
        Ok(())
    }
    pub async fn reuse_first(&mut self) -> Result<()> {
        self.open_gate().await?;
        self.actors[0].wait_on_gate(self.key)?;
        self.wait_for_callers(&[0]).await
    }
    pub async fn settle_first(&mut self) -> Result<()> {
        self.actors[0].settle().await
    }
    pub async fn close_callers(&mut self) -> Result<()> {
        let identities = self.actors.iter().map(|actor| actor.identity.clone()).collect::<Vec<_>>();
        for actor in &mut self.actors {
            actor.close_idle()?;
        }
        self.wait_absent(&identities).await
    }
    async fn observe(&mut self, identities: &[NativeActivityIdentity]) -> Result<Vec<Row>> {
        case::require_active(&self.cancellation)?;
        let pids = identities
            .iter()
            .map(|identity| identity.id.parse::<u32>().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT pid::text AS pid,{} AS backend_start,state,wait_event_type,wait_event FROM pg_catalog.pg_stat_activity WHERE pid IN ({pids}) AND datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database())",
            crate::native::iso("backend_start")
        );
        self.observer.query(&sql, 4).await
    }
    async fn wait_for_callers(&mut self, indexes: &[usize]) -> Result<()> {
        let identities = indexes.iter().map(|index| self.identity(*index).clone()).collect::<Vec<_>>();
        let deadline = tokio::time::Instant::now() + POLL_TIMEOUT;
        loop {
            let rows = self.observe(&identities).await?;
            if identities.iter().all(|identity| {
                rows.iter().any(|row| {
                    matches_identity(row, identity)
                        && row.text("state").as_deref() == Some("active")
                        && row.text("wait_event_type").as_deref() == Some("Lock")
                        && row.text("wait_event").as_deref() == Some("advisory")
                })
            }) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CollectionStop::TimedOut.error());
            }
            case::require_active(&self.cancellation)?;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    async fn wait_absent(&mut self, identities: &[NativeActivityIdentity]) -> Result<()> {
        let deadline = tokio::time::Instant::now() + POLL_TIMEOUT;
        loop {
            let rows = self.observe(identities).await?;
            if identities.iter().all(|identity| !rows.iter().any(|row| matches_identity(row, identity))) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CollectionStop::TimedOut.error());
            }
            case::require_active(&self.cancellation)?;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    pub fn fixture_queries(&self) -> u32 {
        self.retired_gate_queries
            + self.gate.as_ref().map_or(0, |actor| actor.queries)
            + self.actors.iter().map(|actor| actor.queries).sum::<u32>()
    }
    async fn cleanup(&mut self) -> bool {
        let mut settled = !self.actor_start_unconfirmed;
        if let Some(mut gate) = self.gate.take() {
            settled &= gate.retire().await;
            self.retired_gate_queries += gate.queries;
        }
        for actor in &mut self.actors {
            settled &= actor.retire().await;
        }
        if self.cancellation.is_cancelled() {
            settled = false;
        } else {
            let identities = self.identities.clone();
            settled &= self.wait_absent(&identities).await.is_ok();
        }
        settled &= self.observer.close().await == SourceShutdown::Quiescent;
        if let Some(metrics) = self.observer.metrics() {
            settled &= owned_fixture::metrics_are_settled(&metrics, self.observer.queries());
        } else {
            settled = false;
        }
        settled
    }
}
fn matches_identity(row: &Row, identity: &NativeActivityIdentity) -> bool {
    row.text("pid").as_deref() == Some(&identity.id)
        && row.text("backend_start").as_deref() == Some(&identity.generation)
}
