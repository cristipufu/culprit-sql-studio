//! One fixture backend owns its original query until completion; labels never select a native participant.
use super::super::case;
use crate::{
    connect::{Connector, map_error},
    rows::tagged,
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{
    Result,
    telemetry::{CollectionStop, NativeActivityIdentity},
};
use std::time::Duration;
use tokio::{task::JoinHandle, time::timeout};
use tokio_postgres::{CancelToken, Client};

pub(super) const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
pub(super) const SHARED_APPLICATION: &str = "fixture-shared-pooled-driver";
const ACTOR_STATEMENT_TIMEOUT: &str = "50s";
type Completed = (Client, Result<()>);

pub(super) struct Actor {
    pub identity: NativeActivityIdentity,
    client: Option<Client>,
    task: Option<JoinHandle<Completed>>,
    cancel: CancelToken,
    connector: Connector,
    cancellation: CollectionCancellation,
    pub queries: u32,
    settlement_failed: bool,
}
impl Actor {
    pub async fn open(
        connector: &Connector,
        database: &str,
        marker: &str,
        application: &str,
        cancellation: &CollectionCancellation,
    ) -> Result<Self> {
        case::require_active(cancellation)?;
        let client =
            timeout(CONTROL_TIMEOUT, connector.connect_unconfigured()).await.map_err(|_| unconfirmed())??.client;
        case::admitted(cancellation,async {
            timeout(CONTROL_TIMEOUT,client.query(&tagged("SELECT pg_catalog.set_config('statement_timeout',$1,false),pg_catalog.set_config('lock_timeout','0',false),pg_catalog.set_config('application_name',$2,false)"),&[&ACTOR_STATEMENT_TIMEOUT,&application]))
                .await.map_err(|_|unconfirmed())?.map(|_|()).map_err(map_error)
        }).await?;
        let sql = format!(
            "SELECT pg_catalog.pg_backend_pid(),{},pg_catalog.current_database(),pg_catalog.current_setting('cluster_name') FROM pg_catalog.pg_stat_activity WHERE pid=pg_catalog.pg_backend_pid()",
            crate::native::iso("backend_start")
        );
        let row = case::admitted(cancellation, async {
            timeout(CONTROL_TIMEOUT, client.query_one(&tagged(&sql), &[]))
                .await
                .map_err(|_| unconfirmed())?
                .map_err(map_error)
        })
        .await?;
        if row.get::<_, String>(2) != database || row.get::<_, String>(3) != marker {
            return Err(CollectionStop::SourceChanged.error());
        }
        let identity = NativeActivityIdentity { id: row.get::<_, i32>(0).to_string(), generation: row.get(1) };
        if !identity.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let cancel = client.cancel_token();
        Ok(Self {
            identity,
            client: Some(client),
            task: None,
            cancel,
            connector: connector.clone(),
            cancellation: cancellation.clone(),
            queries: 2,
            settlement_failed: false,
        })
    }
    pub async fn hold_gate(&mut self, key: i64) -> Result<()> {
        if self.settlement_failed {
            return Err(unconfirmed());
        }
        let client = self.client.as_ref().ok_or_else(unconfirmed)?;
        case::require_active(&self.cancellation)?;
        self.queries += 1;
        case::admitted(&self.cancellation, async {
            timeout(CONTROL_TIMEOUT, client.query(&tagged("SELECT pg_catalog.pg_advisory_lock($1)"), &[&key]))
                .await
                .map_err(|_| unconfirmed())?
                .map(|_| ())
                .map_err(map_error)
        })
        .await
    }
    pub fn wait_on_gate(&mut self, key: i64) -> Result<()> {
        case::require_active(&self.cancellation)?;
        if self.task.is_some() || self.settlement_failed {
            return Err(unconfirmed());
        }
        let client = self.client.take().ok_or_else(unconfirmed)?;
        let cancellation = self.cancellation.clone();
        self.queries += 1;
        self.task = Some(tokio::spawn(async move {
            let result = case::admitted(&cancellation, async {
                client
                    .query(&tagged("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)"), &[&key])
                    .await
                    .map(|_| ())
                    .map_err(map_error)
            })
            .await;
            (client, result)
        }));
        Ok(())
    }
    pub async fn settle(&mut self) -> Result<()> {
        let task = self.task.as_mut().ok_or_else(unconfirmed)?;
        let joined = match timeout(CONTROL_TIMEOUT, task).await {
            Ok(joined) => joined,
            Err(_) => {
                self.settlement_failed = true;
                return Err(unconfirmed());
            }
        };
        self.task.take();
        let (client, result) = match joined {
            Ok(completed) => completed,
            Err(_) => {
                self.settlement_failed = true;
                return Err(unconfirmed());
            }
        };
        self.client = Some(client);
        self.settlement_failed |= result.is_err();
        result
    }
    pub fn close_idle(&mut self) -> Result<()> {
        if self.task.is_some() {
            return Err(unconfirmed());
        }
        self.client.take();
        Ok(())
    }
    pub async fn retire(&mut self) -> bool {
        self.client.take();
        let mut settled = !self.settlement_failed;
        if self.task.is_some() {
            // Cancellation is cleanup of the original backend; acknowledgement alone never proves settlement.
            let _ = timeout(CONTROL_TIMEOUT, self.connector.cancel(&self.cancel)).await;
            settled &= self.settle().await.is_ok();
            self.client.take();
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
            settled = false;
        }
        settled
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
fn unconfirmed() -> culprit_core::Error {
    CollectionStop::CancellationUnconfirmed.error()
}
