//! One disposable database and three original workload actors at most. Source failures never start replacements.
use super::{
    BODY_TIMEOUT, CASE_TIMEOUT, CONTROL_TIMEOUT, Case, MAX_FIXTURE_MEMORY_KIB, MAX_FIXTURE_REQUESTS,
    MAX_NATIVE_OBSERVATIONS, OBSERVATION_INTERVAL, SETTLEMENT_TIMEOUT, SETUP_TIMEOUT,
    actors::FixtureRequest,
    observations::{self, NativePoint},
    profile::ResourceProfile,
};
use crate::{
    conn::{MsClient, Pool, SessionKind, map_error},
    read::invalid_data,
    test_support::{
        OwnedSourceFixture, config, owned_case::finish_case, owned_case::until_cancelled, receipt_directory,
        save_cost_receipt, verify_owned_source,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Error, Result, telemetry::*};
use futures_util::FutureExt;
use std::{collections::BTreeMap, panic::AssertUnwindSafe, path::PathBuf, sync::Arc};
use tokio::time::Instant;

// The table, finite population, ordering and grant hints are unchanged from the existing reservation fixture.
const SEED: &str = "CREATE TABLE dbo.request_memory_fixture(id int NOT NULL PRIMARY KEY,payload varchar(2000) NOT NULL);INSERT dbo.request_memory_fixture SELECT TOP(10000) ROW_NUMBER() OVER(ORDER BY a.object_id,b.object_id),REPLICATE('x',2000) FROM sys.all_objects a CROSS JOIN sys.all_objects b";

enum AdministrativePurpose {
    Setup,
    Teardown,
}

pub(super) struct Fixture {
    pub cancellation: CollectionCancellation,
    pub directory: PathBuf,
    admitted: Option<OwnedSourceFixture>,
    admin: Option<MsClient>,
    database: Option<String>,
    observer: Option<Arc<Pool>>,
    actor_pools: Vec<Pool>,
    profile: Option<ResourceProfile>,
    requests: Vec<FixtureRequest>,
    demands: BTreeMap<NativeActivityIdentity, u64>,
    control_unconfirmed: bool,
    actor_start_unconfirmed: bool,
    observations: usize,
    completed_observations: usize,
    completed_source_queries: u32,
    original_holder_drained: bool,
    unexercised_reason: Option<&'static str>,
}

impl Fixture {
    fn new(case: Case) -> Self {
        let directory = receipt_directory().join(case.name());
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&directory).expect("a fresh private child for this exact fixture case");
        Self::unstarted(directory)
    }

    fn unstarted(directory: PathBuf) -> Self {
        Self {
            cancellation: CollectionCancellation::default(),
            directory,
            admitted: None,
            admin: None,
            database: None,
            observer: None,
            actor_pools: vec![],
            profile: None,
            requests: vec![],
            demands: BTreeMap::new(),
            control_unconfirmed: false,
            actor_start_unconfirmed: false,
            observations: 0,
            completed_observations: 0,
            completed_source_queries: 0,
            original_holder_drained: false,
            unexercised_reason: None,
        }
    }

    async fn setup(&mut self, case: Case) -> Result<()> {
        self.control_unconfirmed = true;
        let admitted = until_cancelled(&self.cancellation, OwnedSourceFixture::open("culprit")).await?;
        self.admitted = Some(admitted);
        let admitted = self.admitted.as_ref().ok_or_else(invalid_data)?;
        let admin = tokio::time::timeout(
            CONTROL_TIMEOUT,
            until_cancelled(&self.cancellation, async {
                let mut admin = crate::test_support::connect(
                    &config(admitted.port, "master", admitted.password(), "grant-fixture-admin"),
                    SessionKind::User,
                )
                .await?;
                // The FULL native baseline nonce is verified on this same held actor before any derived DDL.
                verify_owned_source(&mut admin).await?;
                Ok(admin)
            }),
        )
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
        // No administrative actor is reused after a dropped/failed command below.
        self.admin = Some(admin);
        self.control_unconfirmed = false;
        let database = format!(
            "{}_{}_{}_{}",
            admitted.database,
            case.name(),
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        );
        if database.len() > 128 {
            return Err(invalid_data());
        }
        self.database = Some(database.clone());
        self.admin_batch(&format!("CREATE DATABASE [{database}]"), AdministrativePurpose::Setup).await?;
        self.admin_batch(&format!("USE [{database}];{SEED};USE [master]"), AdministrativePurpose::Setup).await?;
        self.profile = Some(ResourceProfile::new(&database)?);
        self.install_profile().await?;
        save_cost_receipt(
            &self.directory,
            "resource-profile-installed",
            &self.profile.as_ref().ok_or_else(invalid_data)?.snapshot(),
        )?;
        let admitted = self.admitted.as_ref().ok_or_else(invalid_data)?;
        for index in 0..MAX_FIXTURE_REQUESTS {
            let route = self.profile.as_ref().ok_or_else(invalid_data)?.route(index)?;
            self.actor_pools
                .push(Pool::new(config(admitted.port, &database, admitted.password(), &route.application), None));
        }
        self.observer =
            Some(Arc::new(Pool::new(config(admitted.port, &database, admitted.password(), "culprit"), None)));
        Ok(())
    }

    async fn install_profile(&mut self) -> Result<()> {
        let mut admin = self.admin.take().ok_or_else(|| CollectionStop::CancellationUnconfirmed.error())?;
        self.control_unconfirmed = true;
        let profile = self.profile.as_mut().ok_or_else(invalid_data)?;
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            until_cancelled(&self.cancellation, Box::pin(profile.install(&mut admin))),
        )
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())
        .and_then(|result| result);
        self.finish_admin(admin, result)
    }

    async fn remove_profile(&mut self) -> Result<()> {
        let mut admin = self.admin.take().ok_or_else(|| CollectionStop::CancellationUnconfirmed.error())?;
        self.control_unconfirmed = true;
        let profile = self.profile.as_mut().ok_or_else(invalid_data)?;
        let result = tokio::time::timeout(CONTROL_TIMEOUT, Box::pin(profile.remove(&mut admin)))
            .await
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())
            .and_then(|result| result);
        self.finish_admin(admin, result)
    }

    fn finish_admin(&mut self, admin: MsClient, result: Result<()>) -> Result<()> {
        match result {
            Ok(()) => {
                self.admin = Some(admin);
                self.control_unconfirmed = false;
                Ok(())
            }
            Err(error) => {
                self.control_unconfirmed = true;
                drop(admin);
                Err(error)
            }
        }
    }

    async fn admin_batch(&mut self, sql: &str, purpose: AdministrativePurpose) -> Result<()> {
        let mut admin = self.admin.take().ok_or_else(|| CollectionStop::CancellationUnconfirmed.error())?;
        self.control_unconfirmed = true;
        let sql = format!("/* culprit */ {sql}");
        let command = async {
            admin.simple_query(&sql).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
            Ok(())
        };
        let result = match purpose {
            AdministrativePurpose::Setup => {
                tokio::time::timeout(CONTROL_TIMEOUT, until_cancelled(&self.cancellation, command)).await
            }
            AdministrativePurpose::Teardown => tokio::time::timeout(CONTROL_TIMEOUT, command).await,
        };
        self.finish_admin(
            admin,
            result.map_err(|_| CollectionStop::CancellationUnconfirmed.error()).and_then(|result| result),
        )
    }

    pub fn session(&self, index: usize) -> Result<&NativeActivityIdentity> {
        self.requests.get(index).map(|actor| &actor.session).ok_or_else(invalid_data)
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    pub fn outputs_are_held(&self) -> bool {
        self.requests.iter().all(FixtureRequest::output_is_held)
    }

    pub fn source_pool(&self) -> Result<Arc<Pool>> {
        self.observer.clone().ok_or_else(invalid_data)
    }

    pub fn control_started(&mut self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        if self.control_unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        self.control_unconfirmed = true;
        Ok(())
    }

    pub fn control_completed(&mut self) {
        self.control_unconfirmed = false;
    }

    pub fn can_observe(&self) -> bool {
        self.observations < MAX_NATIVE_OBSERVATIONS && !self.cancellation.is_cancelled()
    }

    pub fn observation_stop(&mut self) -> Error {
        if self.cancellation.is_cancelled() {
            CollectionStop::Cancelled.error()
        } else {
            self.unexercised("required native state was not established within twelve source observations")
        }
    }

    pub async fn start_actor(&mut self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        if self.requests.len() >= MAX_FIXTURE_REQUESTS {
            return Err(self.unexercised("no native waiting peer was observed with the three admitted fixture actors"));
        }
        if let Some(previous) = self.requests.last() {
            let demand = self.demands.get(&previous.session).ok_or_else(invalid_data)?;
            let current = self
                .demands
                .values()
                .try_fold(0u64, |total, value| total.checked_add(*value))
                .ok_or_else(invalid_data)?;
            if !current.checked_add(*demand).is_some_and(|next| next <= MAX_FIXTURE_MEMORY_KIB) {
                return Err(self.unexercised(
                    "next or observed native reservation exceeds the unchanged two-GiB fixture allowance",
                ));
            }
        }
        let index = self.requests.len();
        let pool = self.actor_pools.get(index).ok_or_else(invalid_data)?;
        let route = self.profile.as_ref().ok_or_else(invalid_data)?.route(index)?;
        self.actor_start_unconfirmed = true;
        let actor = until_cancelled(&self.cancellation, FixtureRequest::start(pool, &route)).await?;
        self.requests.push(actor);
        self.actor_start_unconfirmed = false;
        Ok(())
    }

    pub fn unexercised(&mut self, reason: &'static str) -> Error {
        self.unexercised_reason = Some(reason);
        Error::invalid(format!("owned grant fixture control unexercised: {reason}"))
    }

    pub async fn observe(&mut self) -> Result<NativePoint> {
        if !self.can_observe() {
            return Err(self.observation_stop());
        }
        self.observations += 1;
        let point = observations::capture(
            self.source_pool()?,
            self.admitted.as_ref().ok_or_else(invalid_data)?.port,
            self.admitted.as_ref().ok_or_else(invalid_data)?.password(),
            self.database.as_deref().ok_or_else(invalid_data)?,
            &self.cancellation,
            &self.directory,
            self.observations,
        )
        .await?;
        self.completed_observations += 1;
        self.completed_source_queries = self
            .completed_source_queries
            .checked_add(point.queries)
            .ok_or_else(|| CollectionStop::QueryLimit.error())?;
        self.record_demands(&point)?;
        Ok(point)
    }

    fn record_demands(&mut self, point: &NativePoint) -> Result<()> {
        let mut granted = 0u64;
        for actor in &self.requests {
            let Some(row) = point.actor(&actor.session)? else { continue };
            let requested = row
                .requested_kibibytes
                .as_deref()
                .ok_or_else(invalid_data)?
                .parse::<u64>()
                .map_err(|_| invalid_data())?;
            self.demands
                .entry(actor.session.clone())
                .and_modify(|value| *value = (*value).max(requested))
                .or_insert(requested);
            if let Some(value) = row.granted_kibibytes.as_deref() {
                granted =
                    granted.checked_add(value.parse::<u64>().map_err(|_| invalid_data())?).ok_or_else(invalid_data)?;
            }
        }
        let requested =
            self.demands.values().try_fold(0u64, |total, value| total.checked_add(*value)).ok_or_else(invalid_data)?;
        if requested > MAX_FIXTURE_MEMORY_KIB || granted > MAX_FIXTURE_MEMORY_KIB {
            return Err(
                self.unexercised("next or observed native reservation exceeds the unchanged two-GiB fixture allowance")
            );
        }
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        until_cancelled(&self.cancellation, async {
            tokio::time::sleep(OBSERVATION_INTERVAL).await;
            Ok(())
        })
        .await
    }

    pub async fn drain_original_holder(&mut self) -> Result<u32> {
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        if !self.outputs_are_held() || self.original_holder_drained {
            return Err(invalid_data());
        }
        let holder = self.requests.first_mut().ok_or_else(invalid_data)?;
        holder.release();
        let rows = holder.join(Instant::now() + super::ACTOR_DRAIN_TIMEOUT).await?;
        self.original_holder_drained = true;
        if !self.requests.iter().skip(1).all(FixtureRequest::output_is_held) {
            return Err(invalid_data());
        }
        Ok(rows)
    }

    async fn settle(&mut self, deadline: Instant) -> serde_json::Value {
        // Releasing a waiting actor cannot wait for that actor to obtain its first result packet.
        for request in &mut self.requests {
            request.release();
        }
        let mut actors = vec![];
        let mut actors_confirmed = !self.actor_start_unconfirmed;
        for request in &mut self.requests {
            let result = request.join(deadline).await;
            actors_confirmed &= result.is_ok();
            actors.push(serde_json::json!({
                "session": request.session,
                "prework_native_clock_without_offset":request.ready_without_offset,
                "completed_rows": request.completed_rows(),
                "error": result.err(),
            }));
        }
        let sources_confirmed =
            self.observer.as_ref().is_none_or(|pool| pool.source_shutdown() == SourceShutdown::Quiescent)
                && self.actor_pools.iter().all(|pool| pool.source_shutdown() == SourceShutdown::Quiescent);
        self.observer.take();
        self.actor_pools.clear();
        let may_drop = actors_confirmed
            && sources_confirmed
            && !self.control_unconfirmed
            && self.admin.is_some()
            && Instant::now() + CONTROL_TIMEOUT * 2 <= deadline;
        let mut database_removed = self.database.is_none();
        let mut drop_error = None;
        let mut profile_error = None;
        let mut database_cleanup_withheld = self.database.is_some() && !may_drop;
        if may_drop && let Some(database) = self.database.clone() {
            if self.profile.is_some()
                && let Err(error) = self.remove_profile().await
            {
                profile_error = Some(error);
            }
            database_cleanup_withheld = profile_error.is_some() || Instant::now() + CONTROL_TIMEOUT > deadline;
            if !database_cleanup_withheld {
                let sql = format!(
                    "USE [master];ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"
                );
                match self.admin_batch(&sql, AdministrativePurpose::Teardown).await {
                    Ok(()) => database_removed = true,
                    Err(error) => drop_error = Some(error),
                }
            }
        }
        self.admin.take();
        serde_json::json!({
            "actors": actors,
            "actor_completion_confirmed": actors_confirmed,
            "source_shutdown_confirmed": sources_confirmed,
            "administrative_shutdown_unconfirmed": self.control_unconfirmed,
            "owned_database_removed": database_removed,
            "drop_withheld": database_cleanup_withheld,
            "drop_error": drop_error,
            "resource_profile":self.profile.as_ref().map(ResourceProfile::snapshot),
            "profile_cleanup_error":profile_error,
            "cleanup_confirmed": actors_confirmed && sources_confirmed && !self.control_unconfirmed && database_removed,
        })
    }
}

pub(super) async fn run(case: Case) {
    let started = Instant::now();
    let deadline = started + CASE_TIMEOUT;
    let mut fixture = Fixture::new(case);
    let cancellation = fixture.cancellation.clone();
    let (setup_expired, setup) = finish_case(
        started + SETUP_TIMEOUT,
        &cancellation,
        Box::pin(AssertUnwindSafe(fixture.setup(case)).catch_unwind()),
    )
    .await;
    let mut body_expired = false;
    let body = match setup {
        Ok(Ok(())) if !setup_expired => {
            let body_deadline = (Instant::now() + BODY_TIMEOUT).min(deadline - SETTLEMENT_TIMEOUT);
            let result = finish_case(
                body_deadline,
                &cancellation,
                Box::pin(
                    AssertUnwindSafe(async {
                        match case {
                            Case::Reservations => super::reservations::prove(&mut fixture).await,
                            Case::DrainOutput => super::drain::prove(&mut fixture).await,
                        }
                    })
                    .catch_unwind(),
                ),
            )
            .await;
            body_expired = result.0;
            result.1
        }
        Ok(Ok(())) => Ok(Err(CollectionStop::TimedOut.error())),
        result => result,
    };
    let settlement = Box::pin(fixture.settle(deadline)).await;
    let elapsed = started.elapsed();
    save_cost_receipt(
        &fixture.directory,
        "case",
        &serde_json::json!({
            "case": case.name(),
            "native_owned_baseline": fixture.admitted.as_ref().map(|owner| &owner.database),
            "owned_derived_database": fixture.database,
            "workload_actors": fixture.requests.len(),
            "actor_limit": MAX_FIXTURE_REQUESTS,
            "source_points_started": fixture.observations,
            "source_points_completed": fixture.completed_observations,
            "source_point_limit": MAX_NATIVE_OBSERVATIONS,
            "source_queries_in_completed_points": fixture.completed_source_queries,
            "source_query_count": (fixture.completed_observations == fixture.observations)
                .then_some(fixture.completed_source_queries),
            "source_queries_upper_bound": fixture.observations * super::SOURCE_QUERY_LIMIT as usize,
            "source_query_scope":"original capture SQL only; separately charged preparation and independent observer work are retained in each source-point receipt",
            "whole_login_or_cold_start_cost_proved":false,
            "rust_case_elapsed_ms": elapsed.as_secs_f64() * 1000.0,
            "rust_case_limit_seconds": CASE_TIMEOUT.as_secs(),
            "supervisor_startup_included": false,
            "setup_deadline_reached": setup_expired,
            "body_deadline_reached": body_expired,
            "original_holder_drained": fixture.original_holder_drained,
            "settlement": settlement,
            "body_error": body.as_ref().ok().and_then(|result| result.as_ref().err()),
            "unexercised_reason":fixture.unexercised_reason,
            "panicked": body.is_err(),
            "production_cost_qualified": false,
            "managed_azure_profile_qualified": false,
            "active_source_cancellation_control": "not attempted; case-deadline cancellation is safety cleanup only",
            "competing_lock_control": "unexercised; no extra actor or changed workload was introduced",
        }),
    )
    .expect("write the private original case receipt after settlement");
    assert!(settlement["cleanup_confirmed"].as_bool() == Some(true), "{settlement}");
    assert!(!setup_expired && !body_expired && elapsed <= CASE_TIMEOUT, "owned case deadline reached");
    match body {
        Ok(result) => {
            result.expect("native owned acceptance must establish the requested state within its fixed bounds")
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Measured unboxed frames are 2.7 KiB for the case, 41–45 KiB for the boxed source phases and 4.1 KiB
    // for settlement. These bounds catch frame growth; native execution and shutdown are separate checks.
    const MAX_CASE_FUTURE_BYTES: usize = 4 * 1024;
    const MAX_SOURCE_PHASE_FUTURE_BYTES: usize = 64 * 1024;
    const MAX_SETTLEMENT_FUTURE_BYTES: usize = 8 * 1024;

    fn frame_bytes(name: &str, future: &impl std::future::Future) -> usize {
        let bytes = std::mem::size_of_val(future);
        // Constructing and dropping these unpolled futures does not read environment/configuration, open a
        // connection, create a directory or dispatch any fixture/source SQL.
        eprintln!("owned grant fixture: {name} unboxed future frame = {bytes} bytes");
        bytes
    }

    #[test]
    fn native_fixture_future_frames_stay_within_their_owned_phase_budgets() {
        let case = frame_bytes("case", &run(Case::DrainOutput));
        let mut fixture = Fixture::unstarted(PathBuf::new());
        let setup = frame_bytes("setup", &fixture.setup(Case::DrainOutput));
        let reservations = frame_bytes("reservation proof", &super::super::reservations::prove(&mut fixture));
        let drain = frame_bytes("drain proof", &super::super::drain::prove(&mut fixture));
        let settlement = frame_bytes("settlement", &fixture.settle(Instant::now()));
        // Report every actual frame before any assertion, so a red first bound cannot hide later sizes.
        for (name, bytes, limit) in [
            ("case", case, MAX_CASE_FUTURE_BYTES),
            ("setup", setup, MAX_SOURCE_PHASE_FUTURE_BYTES),
            ("reservation proof", reservations, MAX_SOURCE_PHASE_FUTURE_BYTES),
            ("drain proof", drain, MAX_SOURCE_PHASE_FUTURE_BYTES),
            ("settlement", settlement, MAX_SETTLEMENT_FUTURE_BYTES),
        ] {
            assert!(bytes <= limit, "{name} future grew to {bytes} bytes (limit {limit}); box the actual owned phase");
        }
    }
}
