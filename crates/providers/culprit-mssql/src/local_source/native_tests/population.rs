//! Idle unrelated connections own no tasks or outstanding response. Dropping this owner closes every actor.
use super::*;
use crate::{
    conn::MsClient,
    read::{integer, text},
    value::Rec,
};
use std::collections::BTreeSet;

pub(super) const IDLE_SESSIONS: usize = 32;
const SETUP_TIMEOUT: Duration = Duration::from_secs(3);
const RELEASE_TIMEOUT: Duration = Duration::from_secs(3);
const IDENTITY_SQL: &str = "SELECT /* culprit */ /* owned context idle identity */ CONVERT(bigint,@@SPID) AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS login_time FROM sys.dm_exec_sessions WHERE session_id=@@SPID";
const POPULATION_SQL: &str = "SELECT TOP(33) /* culprit */ /* owned context idle population */ CONVERT(bigint,session_id) AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS login_time,CONVERT(int,database_id) AS database_id,status FROM sys.dm_exec_sessions WHERE program_name=@P1 OPTION(MAXDOP 1)";

pub(super) struct IdleSessions {
    clients: Vec<MsClient>,
    identities: BTreeSet<(String, String)>,
    application: String,
    setup_queries: usize,
    observer_queries: usize,
    setup_elapsed_ms: f64,
    release_elapsed_ms: f64,
}
impl IdleSessions {
    pub(super) fn new() -> Self {
        Self {
            clients: Vec::new(),
            identities: BTreeSet::new(),
            application: format!("culprit-owned-context-idle-{}", std::process::id()),
            setup_queries: 0,
            observer_queries: 0,
            setup_elapsed_ms: 0.0,
            release_elapsed_ms: 0.0,
        }
    }
    pub(super) fn len(&self) -> usize {
        self.clients.len()
    }
    pub(super) fn contains(&self, session: &NativeActivityIdentity) -> bool {
        self.identities.contains(&(session.id.clone(), session.generation.clone()))
    }
    pub(super) async fn populate(&mut self, fixture: &OwnedSourceFixture, observer: &mut ReadSession) -> Result<()> {
        assert!(self.clients.is_empty() && self.identities.is_empty());
        let started = Instant::now();
        let result = self.retain_actors(fixture).await;
        self.setup_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        result?;
        self.confirm_idle(observer).await
    }
    async fn retain_actors(&mut self, fixture: &OwnedSourceFixture) -> Result<()> {
        for _ in 0..IDLE_SESSIONS {
            let config = config(fixture.port, "master", fixture.password(), &self.application);
            let client = tokio::time::timeout(SETUP_TIMEOUT, connect(&config, SessionKind::User))
                .await
                .map_err(|_| CollectionStop::TimedOut.error())??;
            // Retain ownership before reading identity: errors/timeouts still leave a client for release().
            self.clients.push(client);
            self.setup_queries += 1;
            let client = self.clients.last_mut().expect("retained actor");
            let rows = tokio::time::timeout(SETUP_TIMEOUT, async {
                let query = tiberius::Query::new(IDENTITY_SQL);
                let rows = query
                    .query(client)
                    .await
                    .map_err(crate::conn::map_error)?
                    .into_results()
                    .await
                    .map_err(crate::conn::map_error)?;
                let [rows] = rows.as_slice() else { return Err(crate::read::invalid_data()) };
                let [row] = rows.as_slice() else { return Err(crate::read::invalid_data()) };
                Ok(Rec::from_row(row))
            })
            .await
            .map_err(|_| CollectionStop::TimedOut.error())??;
            if !self.identities.insert(identity(&rows)?) {
                return Err(crate::read::invalid_data());
            }
        }
        Ok(())
    }
    pub(super) fn release(&mut self) {
        self.clients.clear();
    }
    async fn rows(&mut self, observer: &mut ReadSession) -> Result<Vec<Rec>> {
        self.observer_queries += 1;
        observer
            .query(POPULATION_SQL, vec![Parameter::Text(self.application.clone())], IDLE_SESSIONS + 1)
            .await
            .map_err(ReadError::into_error)
    }
    pub(super) async fn confirm_idle(&mut self, observer: &mut ReadSession) -> Result<()> {
        let rows = self.rows(observer).await?;
        let identities = rows.iter().map(identity).collect::<Result<BTreeSet<_>>>()?;
        if rows.len() != self.clients.len()
            || identities != self.identities
            || rows.iter().any(|row| {
                integer(row, "database_id").ok() != Some(1)
                    || !text(row, "status").is_ok_and(|state| state.eq_ignore_ascii_case("sleeping"))
            })
        {
            return Err(crate::read::invalid_data());
        }
        Ok(())
    }
    pub(super) async fn confirm_released(&mut self, observer: &mut ReadSession) -> Result<()> {
        assert!(self.clients.is_empty(), "release actors before awaiting native cleanup");
        let started = Instant::now();
        let deadline = started + RELEASE_TIMEOUT;
        loop {
            if self.rows(observer).await?.is_empty() {
                self.release_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
                self.identities.clear();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CollectionStop::TimedOut.error());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub(super) fn accounting(&self) -> Value {
        json!({"identity_queries_attempted":self.setup_queries,"native_membership_queries":self.observer_queries,
            "actor_login_and_session_setup":"included in setup client elapsed; native CPU/IO not isolated",
            "setup_client_elapsed_ms":self.setup_elapsed_ms,"release_client_elapsed_ms":self.release_elapsed_ms,
            "outside_source_measurement":true,"actor_work":"idle held sockets only; no polling/application SQL on actors"})
    }
}
fn identity(row: &Rec) -> Result<(String, String)> {
    let id = integer(row, "id")?;
    if !(1..=i16::MAX as i64).contains(&id) {
        return Err(crate::read::invalid_data());
    }
    Ok((id.to_string(), crate::sql_time::native_time(&text(row, "login_time")?)?))
}
pub(super) fn observer(fixture: &OwnedSourceFixture) -> ReadSession {
    let pool = std::sync::Arc::new(crate::conn::Pool::new(
        config(fixture.port, &fixture.database, fixture.password(), "culprit-context-population-observer"),
        None,
    ));
    ReadSession::new(
        pool,
        CollectionBudget {
            max_queries: 128,
            statement_timeout_ms: 1500,
            total_timeout_ms: 100_000,
            ..Default::default()
        },
    )
}
