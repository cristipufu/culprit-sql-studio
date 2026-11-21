//! Retains every attempted idle client until awaited retirement; uncertain identity work never heals on close.
use super::*;

#[derive(Default)]
struct Work {
    connection_attempts: usize,
    connections_completed: usize,
    identity_attempts: usize,
    identity_drained: usize,
    retirement_attempts: usize,
    retirements_confirmed: usize,
}
impl Work {
    fn has_unconfirmed_identity(&self) -> bool {
        self.identity_attempts != self.identity_drained
    }
    fn retirement_complete(&self) -> bool {
        self.retirement_attempts == self.connections_completed
            && self.retirements_confirmed == self.retirement_attempts
            && !self.has_unconfirmed_identity()
    }
}

pub(super) struct IdleClients {
    clients: Vec<MsClient>,
    identities: Vec<IdleIdentity>,
    work: Work,
}
impl IdleClients {
    pub fn new() -> Self {
        Self { clients: Vec::new(), identities: Vec::new(), work: Work::default() }
    }
    pub fn identities(&self) -> &[IdleIdentity] {
        &self.identities
    }
    pub async fn extend_to(
        &mut self,
        population: usize,
        fixture: &OwnedSourceFixture,
        cancellation: &CollectionCancellation,
    ) -> Result<()> {
        if population > *IDLE_POPULATIONS.last().ok_or_else(invalid_data)? {
            return Err(invalid_data());
        }
        while self.clients.len() < population {
            self.connect_one(fixture, cancellation).await?;
        }
        Ok(())
    }
    async fn connect_one(&mut self, fixture: &OwnedSourceFixture, cancellation: &CollectionCancellation) -> Result<()> {
        owned_case::require_active(cancellation)?;
        if self.work.has_unconfirmed_identity() {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        fixture.pool.ensure_available()?;
        self.work.connection_attempts += 1;
        let client = Box::pin(tokio::time::timeout(SETUP_TIMEOUT, fixture.pool.connect(SessionKind::User)))
            .await
            .map_err(|_| CollectionStop::TimedOut.error())??;
        self.clients.push(client);
        self.work.connections_completed += 1;
        // Cancellation during login cannot authorize its next native identity query. Retain the client first.
        owned_case::require_active(cancellation)?;
        self.work.identity_attempts += 1;
        let client = self.clients.last_mut().ok_or_else(invalid_data)?;
        let native = Box::pin(tokio::time::timeout(SETUP_TIMEOUT, async {
            client
                .simple_query(IDENTIFY_IDLE_SQL)
                .await
                .map_err(crate::conn::map_error)?
                .into_results()
                .await
                .map_err(crate::conn::map_error)
        }))
        .await;
        let rows = match native {
            Ok(Ok(rows)) => {
                self.work.identity_drained += 1;
                rows
            }
            Ok(Err(error)) => {
                fixture.pool.mark_uncertain();
                return Err(error);
            }
            Err(_) => {
                fixture.pool.mark_uncertain();
                return Err(CollectionStop::CancellationUnconfirmed.error());
            }
        };
        let [result] = rows.as_slice() else { return Err(invalid_data()) };
        let [row] = result.as_slice() else { return Err(invalid_data()) };
        self.identities.push(identity(&crate::value::Rec::from_row(row), &fixture.database)?);
        Ok(())
    }
    pub fn fence_unconfirmed(&self, pool: &crate::conn::Pool) {
        if self.work.has_unconfirmed_identity() {
            pool.mark_uncertain();
        }
    }
    pub async fn retire(&mut self) -> Result<()> {
        self.work.retirement_attempts += self.clients.len();
        // Concurrently await the same original clients under the existing per-retirement 5s policy. No new
        // native query or actor is started, and 32 sockets cannot multiply the cleanup wall bound by 32.
        let results = futures_util::future::join_all(
            self.clients.drain(..).map(|client| AssertUnwindSafe(owned_case::retire(client)).catch_unwind()),
        )
        .await;
        let mut failure = None;
        let mut first_panic = None;
        for result in results {
            match result {
                Ok(Ok(())) => self.work.retirements_confirmed += 1,
                Ok(Err(error)) if failure.is_none() => failure = Some(error),
                Err(panic) if first_panic.is_none() => first_panic = Some(panic),
                _ => (),
            }
        }
        if let Some(panic) = first_panic {
            std::panic::resume_unwind(panic);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if !self.work.retirement_complete() {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        Ok(())
    }
    pub fn accounting(&self) -> serde_json::Value {
        json!({
            "connection_attempts":self.work.connection_attempts,"connections_completed":self.work.connections_completed,
            "possible_user_session_setup_queries":self.work.connection_attempts,
            "identity_queries_attempted":self.work.identity_attempts,"identity_queries_drained":self.work.identity_drained,
            "identities_validated":self.identities.len(),"unconfirmed_identity_commands":self.work.identity_attempts-self.work.identity_drained,
            "retirement_attempts":self.work.retirement_attempts,"retirements_confirmed":self.work.retirements_confirmed,
            "retirement_complete":self.work.retirement_complete(),"outside_source_measurement":true,
        })
    }
}
fn identity(row: &crate::value::Rec, database: &str) -> Result<IdleIdentity> {
    if text(row, "database_name")? != database {
        return Err(invalid_data());
    }
    Ok(IdleIdentity {
        session: NativeActivityIdentity {
            id: text(row, "session_id")?,
            generation: crate::sql_time::native_time(&text(row, "session_generation")?)?,
        },
        connection_id: text(row, "connection_id")?,
        connected_without_offset: crate::sql_time::native_time(&text(row, "connected_without_offset")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_retirement_never_discharges_an_undrained_identity_command_or_hides_an_attempt() {
        let mut work =
            Work { connection_attempts: 2, connections_completed: 1, identity_attempts: 1, ..Default::default() };
        work.retirement_attempts = 1;
        work.retirements_confirmed = 1;
        assert!(work.has_unconfirmed_identity());
        assert!(!work.retirement_complete());
        let clients = IdleClients { clients: vec![], identities: vec![], work };
        let receipt = clients.accounting();
        assert_eq!(receipt["connection_attempts"], 2);
        assert_eq!(receipt["identity_queries_attempted"], 1);
        assert_eq!(receipt["identity_queries_drained"], 0);
        assert_eq!(receipt["identities_validated"], 0);
        assert_eq!(receipt["unconfirmed_identity_commands"], 1);
        assert_eq!(receipt["retirement_complete"], false);
    }

    #[test]
    fn unconfirmed_identity_work_fences_the_existing_pool_even_after_all_sockets_are_retired() {
        let pool = crate::conn::Pool::new(crate::test_support::config(1, "owned", "unused", "idle-fixture-test"), None);
        let clients = IdleClients {
            clients: vec![],
            identities: vec![],
            work: Work {
                connection_attempts: 1,
                connections_completed: 1,
                identity_attempts: 1,
                retirement_attempts: 1,
                retirements_confirmed: 1,
                ..Default::default()
            },
        };
        clients.fence_unconfirmed(&pool);
        assert_eq!(pool.source_shutdown(), SourceShutdown::Unconfirmed);
        assert_eq!(
            CollectionStop::from_error(&pool.ensure_available().unwrap_err()),
            CollectionStop::CancellationUnconfirmed
        );
        assert_eq!(clients.accounting()["unconfirmed_identity_commands"], 1);
        assert!(!clients.work.retirement_complete());
    }

    #[test]
    fn completed_queries_still_require_every_original_client_retirement() {
        let mut work = Work {
            connection_attempts: 2,
            connections_completed: 2,
            identity_attempts: 2,
            identity_drained: 2,
            retirement_attempts: 2,
            retirements_confirmed: 1,
        };
        assert!(!work.has_unconfirmed_identity());
        assert!(!work.retirement_complete());
        work.retirements_confirmed = 2;
        assert!(work.retirement_complete());
    }
}
