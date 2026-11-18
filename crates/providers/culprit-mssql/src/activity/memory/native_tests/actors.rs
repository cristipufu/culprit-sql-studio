//! One owned workload stream. Draining its original handle is the intervention; dropping it is not completion.
use super::{ACTOR_DRAIN_TIMEOUT, CONTROL_TIMEOUT, FIXTURE_ROWS, WORKLOAD, profile::ActorRoute};
use crate::{
    conn::{Pool, SessionKind, map_error},
    read::{invalid_data, text},
};
use culprit_core::{Result, telemetry::*};
use futures_util::TryStreamExt;
use tokio::{sync::oneshot, task::JoinHandle};

fn session_sql() -> String {
    format!(
        "/* culprit */ WAITFOR DELAY '00:00:00.010';SELECT CONVERT(varchar(16),s.session_id) AS session_id,CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,CONVERT(varchar(40),SYSDATETIME(),126) AS native_ready_without_offset,s.program_name AS actor_application,s.group_id,g.pool_id,g.name AS group_name,p.name AS pool_name,p.min_memory_percent,p.max_memory_percent,p.max_memory_kb,p.target_memory_kb,g.request_max_memory_grant_percent_numeric AS grant_percent,g.group_max_requests,c.is_enabled,c.classifier_function_id AS configured_classifier,d.classifier_function_id AS effective_classifier,d.is_reconfiguration_pending FROM sys.dm_exec_sessions s JOIN sys.dm_resource_governor_workload_groups g ON g.group_id=s.group_id JOIN sys.dm_resource_governor_resource_pools p ON p.pool_id=g.pool_id CROSS JOIN sys.resource_governor_configuration c CROSS JOIN sys.dm_resource_governor_configuration d WHERE s.session_id=@@SPID AND ({})",
        crate::activity::ESTABLISHED_SESSION_SQL
    )
}

pub(super) struct FixtureRequest {
    pub session: NativeActivityIdentity,
    pub ready_without_offset: Option<String>,
    release: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<u32>>>,
    completed_rows: Option<u32>,
}
impl FixtureRequest {
    pub async fn start(pool: &Pool, route: &ActorRoute) -> Result<Self> {
        let (mut client, session, ready) = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let mut client = pool.connect(SessionKind::User).await?;
            let rows = crate::conn::query(&mut client, &session_sql()).await.map_err(map_error)?;
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            let session = NativeActivityIdentity {
                id: text(row, "session_id")?,
                generation: crate::sql_time::native_time(&text(row, "session_generation")?)?,
            };
            let ready = crate::sql_time::native_time(&text(row, "native_ready_without_offset")?)?;
            if !session.is_valid() || !route.matches(row)? || !native_clock_follows_login(&session.generation, &ready) {
                return Err(invalid_data());
            }
            Ok((client, session, ready))
        })
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
        let (release, receive) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut stream = client.simple_query(WORKLOAD).await.map_err(map_error)?;
            let first = stream.try_next().await.map_err(map_error)?;
            let mut rows = u32::from(matches!(first, Some(tiberius::QueryItem::Row(_))));
            receive.await.map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
            while let Some(item) = stream.try_next().await.map_err(map_error)? {
                if matches!(item, tiberius::QueryItem::Row(_)) {
                    rows += 1;
                    if rows > FIXTURE_ROWS {
                        return Err(CollectionStop::RowLimit.error());
                    }
                }
            }
            drop(stream);
            if rows != FIXTURE_ROWS {
                return Err(invalid_data());
            }
            client.close().await.map_err(map_error)?;
            Ok(rows)
        });
        Ok(Self {
            session,
            ready_without_offset: Some(ready),
            release: Some(release),
            task: Some(task),
            completed_rows: None,
        })
    }

    pub fn output_is_held(&self) -> bool {
        self.release.is_some() && self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    pub fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    pub async fn join(&mut self, deadline: tokio::time::Instant) -> Result<u32> {
        if let Some(rows) = self.completed_rows {
            return Ok(rows);
        }
        let task = self.task.as_mut().ok_or_else(|| CollectionStop::CancellationUnconfirmed.error())?;
        let deadline = deadline.min(tokio::time::Instant::now() + ACTOR_DRAIN_TIMEOUT);
        match tokio::time::timeout_at(deadline, task).await {
            Ok(result) => {
                self.task.take();
                let rows = result.map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
                self.completed_rows = Some(rows);
                Ok(rows)
            }
            Err(_) => {
                let task = self.task.take().ok_or_else(invalid_data)?;
                task.abort();
                let _ = task.await;
                Err(CollectionStop::CancellationUnconfirmed.error())
            }
        }
    }

    pub fn completed_rows(&self) -> Option<u32> {
        self.completed_rows
    }
}

fn native_clock_follows_login(login: &str, ready: &str) -> bool {
    let parse = |value: &str| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").ok();
    parse(login).zip(parse(ready)).is_some_and(|(login, ready)| ready > login)
}
impl Drop for FixtureRequest {
    fn drop(&mut self) {
        // Fallback retirement never claims a completed native response. Normal cleanup explicitly joins.
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(task: JoinHandle<Result<u32>>) -> FixtureRequest {
        FixtureRequest {
            session: NativeActivityIdentity { id: "42".into(), generation: "fixture-owner".into() },
            ready_without_offset: None,
            release: None,
            task: Some(task),
            completed_rows: None,
        }
    }

    #[test]
    fn native_prework_barrier_requires_a_strictly_later_clock_and_preserves_the_login_token() {
        let login = "2026-11-18T19:56:12.7266667";
        assert!(!native_clock_follows_login(login, "2026-11-18T19:56:12.7233333"));
        assert!(!native_clock_follows_login(login, login));
        assert!(native_clock_follows_login(login, "2026-11-18T19:56:12.7366667"));
        assert!(session_sql().contains(crate::activity::ESTABLISHED_SESSION_SQL));
    }

    #[tokio::test]
    async fn only_acknowledged_original_stream_completion_survives_a_second_cleanup_join() {
        let mut completed = actor(tokio::spawn(async { Ok(FIXTURE_ROWS) }));
        let deadline = tokio::time::Instant::now() + ACTOR_DRAIN_TIMEOUT;
        assert_eq!(completed.join(deadline).await.unwrap(), FIXTURE_ROWS);
        assert_eq!(completed.completed_rows(), Some(FIXTURE_ROWS));
        assert_eq!(completed.join(deadline).await.unwrap(), FIXTURE_ROWS);

        let mut failed = actor(tokio::spawn(async { Err(CollectionStop::SourceChanged.error()) }));
        let error = failed.join(deadline).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert!(failed.task.is_none());
        assert_eq!(failed.completed_rows(), None, "a removed failed handle is not native completion");
        assert!(failed.join(deadline).await.is_err(), "cleanup cannot repair the original failure");
    }

    #[tokio::test]
    async fn timed_out_actor_is_joined_after_retirement_without_a_completion_claim() {
        let mut pending = actor(tokio::spawn(std::future::pending()));
        let error = pending.join(tokio::time::Instant::now()).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
        );
        assert!(pending.task.is_none(), "the same retired task is joined");
        assert_eq!(pending.completed_rows(), None);
    }
}
