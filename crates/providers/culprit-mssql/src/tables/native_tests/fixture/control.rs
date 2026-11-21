//! Counted commands on one verified original client; the shared retirement policy owns every outcome.
use super::*;
use crate::{
    test_support::owned_control::{COMMAND_TIMEOUT, retire_control},
    value::Rec,
};

const MAX_CONTROL_CONNECTIONS: u32 = 4;
const MAX_CONTROL_SQL: u32 = 24;

pub(super) struct Commands<'a> {
    client: &'a mut MsClient,
    attempted: &'a mut u32,
    temporal: &'a mut super::temporal::Objects,
}
impl Commands<'_> {
    pub(super) fn claim_temporal(&mut self) {
        self.temporal.claim();
    }
    fn charge(&mut self) {
        assert!(*self.attempted < MAX_CONTROL_SQL, "owned full-text command dispatch cap");
        *self.attempted += 1;
    }
    pub(super) async fn batch(&mut self, sql: &str) {
        self.charge();
        crate::conn::execute(self.client, &format!("/* culprit fixture */ {sql}")).await.unwrap();
    }
    pub(super) async fn row(&mut self, sql: &str) -> Rec {
        self.charge();
        let rows = self.client.simple_query(sql).await.unwrap().into_results().await.unwrap();
        let [rows] = rows.as_slice() else { panic!("one fully drained owned metadata result set") };
        let [row] = rows.as_slice() else { panic!("one selected owned metadata row") };
        Rec::from_row(row)
    }
}
impl Fixture<'_> {
    pub(super) async fn control<T>(
        &mut self,
        label: &str,
        work: impl AsyncFnOnce(&mut Commands<'_>, &mut bool) -> T,
    ) -> T {
        assert!(self.control_available, "unconfirmed original control forbids any new fixture SQL");
        assert!(self.control_connections < MAX_CONTROL_CONNECTIONS, "owned full-text control session cap");
        assert!(self.control_sql_attempts < MAX_CONTROL_SQL, "owned full-text verification dispatch cap");
        self.control_available = false;
        self.control_connections += 1; // Reserve this original connection's one SET batch, even if login fails.
        let mut actor = tokio::time::timeout(
            COMMAND_TIMEOUT,
            connect(&config(self.owned.port, &self.owned.database, self.owned.password(), label), SessionKind::User),
        )
        .await
        .unwrap()
        .unwrap();
        self.control_sql_attempts += 1; // Same-client nonce verification is paid before dispatch.
        let operation = AssertUnwindSafe(tokio::time::timeout(COMMAND_TIMEOUT, async {
            verify_owned_source(&mut actor).await.unwrap();
            let mut commands = Commands {
                client: &mut actor,
                attempted: &mut self.control_sql_attempts,
                temporal: &mut self.temporal,
            };
            work(&mut commands, &mut self.owns_objects).await
        }))
        .catch_unwind()
        .await;
        let retired = retire_control(operation, actor.close()).await;
        self.control_available = retired.permits_sql();
        retired.finish()
    }
    pub(super) fn control_accounting(&self) -> serde_json::Value {
        serde_json::json!({
            "fixture_sql_dispatch_attempts":self.control_sql_attempts,
            "fixture_sql_dispatch_cap":MAX_CONTROL_SQL,
            "fixture_connection_setup_reservations":self.control_connections,
            "fixture_connection_setup_cap":MAX_CONTROL_CONNECTIONS,
            "control_responses_and_client_retirements_confirmed":self.control_available,
            "per_connection_or_command_or_retirement_bound_seconds":COMMAND_TIMEOUT.as_secs(),
            "fixture_admission_dispatch_cap_including_its_setup":2,
        })
    }
}
