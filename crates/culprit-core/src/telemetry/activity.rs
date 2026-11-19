//! Provider-neutral activity facts and bounded selection of persistent issues and their blocker context.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
mod session_context;
pub use session_context::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityPolicy {
    pub blocked_after_ms: u32,
    pub running_after_ms: u32,
}
impl Default for ActivityPolicy {
    fn default() -> Self {
        Self { blocked_after_ms: 30_000, running_after_ms: 60_000 }
    }
}
impl ActivityPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(1_000..=604_800_000).contains(&self.blocked_after_ms)
            || !(1_000..=604_800_000).contains(&self.running_after_ms)
        {
            return Err(Error::invalid(t!("collection.invalid_budget")));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[ts(export)]
pub struct NativeActivityIdentity {
    pub id: String,
    pub generation: String,
}
impl NativeActivityIdentity {
    pub fn is_valid(&self) -> bool {
        identifier(&self.id) && identifier(&self.generation)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[ts(export)]
pub struct ActivityKey {
    pub session: NativeActivityIdentity,
    pub request: Option<NativeActivityIdentity>,
    /// Idle transactions need their own generation; a session can begin another transaction between polls.
    pub transaction: Option<String>,
}
impl ActivityKey {
    pub fn is_valid(&self) -> bool {
        self.session.is_valid()
            && self.request.as_ref().is_none_or(NativeActivityIdentity::is_valid)
            && self.transaction.as_deref().is_none_or(identifier)
            && (self.request.is_none() || self.transaction.is_none())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TransactionWaitOwner {
    Active,
    Prepared,
    Orphaned,
    Recovery,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum UnknownWaitOwner {
    LatchTransition,
    UntrackedLatch,
    Other,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityWaitOwner {
    Session { id: String, generation: Option<String> },
    Transaction { native_id: String, owner: TransactionWaitOwner },
    Unknown { native_id: String, reason: UnknownWaitOwner },
}
impl ActivityWaitOwner {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Session { id, generation } => identifier(id) && generation.as_deref().is_none_or(identifier),
            Self::Transaction { native_id, .. } | Self::Unknown { native_id, .. } => identifier(native_id),
        }
    }
    pub fn is_blocker(&self) -> bool {
        !matches!(self, Self::Unknown { .. })
    }
    pub fn session(&self) -> Option<NativeActivityIdentity> {
        match self {
            Self::Session { id, generation: Some(generation) } => {
                Some(NativeActivityIdentity { id: id.clone(), generation: generation.clone() })
            }
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[ts(export)]
pub struct ActivityCounters {
    pub cpu_ms: Option<String>,
    pub logical_reads: Option<String>,
    pub physical_reads: Option<String>,
    pub writes: Option<String>,
    /// Native request-lifetime page-server reads. No statement attribution, elapsed time or cache-miss inference.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_server_reads: Option<String>,
}
impl ActivityCounters {
    fn is_valid(&self) -> bool {
        [&self.cpu_ms, &self.logical_reads, &self.physical_reads, &self.writes]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(counter))
            && self
                .page_server_reads
                .as_deref()
                .is_none_or(|value| PageServerReadStatistics::native_count(value).is_some())
    }
    pub fn progress(&self, before: &Self) -> ActivityProgress {
        let pairs = [
            (&self.cpu_ms, &before.cpu_ms),
            (&self.logical_reads, &before.logical_reads),
            (&self.physical_reads, &before.physical_reads),
            (&self.writes, &before.writes),
            (&self.page_server_reads, &before.page_server_reads),
        ];
        let comparable: Vec<_> = pairs
            .into_iter()
            .filter_map(|(now, before)| {
                Some((now.as_deref()?.parse::<u64>().ok()?, before.as_deref()?.parse::<u64>().ok()?))
            })
            .collect();
        if comparable.iter().any(|(now, before)| now < before) {
            return ActivityProgress::Unknown;
        }
        if comparable.is_empty() {
            ActivityProgress::Unknown
        } else if comparable.iter().any(|(now, before)| now != before) {
            ActivityProgress::ChangedCounters
        } else {
            ActivityProgress::NoObservedCounterChange
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityWaitClass {
    LogRateGovernance,
    /// A native execution-memory grant wait, distinct from compilation admission and allocator contention.
    ExecutionGrant,
    /// A native optimizer admission wait. It identifies an affected request, not a memory holder.
    CompilationAdmission,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityWait {
    pub class: Option<ActivityWaitClass>,
    /// Identified wait targets only. An empty list does not establish absence of resource contention.
    pub identified_resources: Vec<crate::ObservedWaitResource>,
    pub native_type: Option<String>,
    pub elapsed_ms: Option<String>,
    pub owners: Vec<ActivityWaitOwner>,
    /// None is uncollected lock detail, distinct from an observed empty set.
    pub resources: Option<Vec<crate::LockWait>>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityClient {
    pub application: Option<String>,
    pub user: Option<String>,
    pub host: Option<String>,
}
impl ActivityClient {
    pub fn is_valid(&self) -> bool {
        [&self.application, &self.user, &self.host]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(|value| value.len() <= 512))
    }
}

/// Unknown blocking evidence cannot clear an incident previously established for the same request.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityBlockingStatus {
    Known,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_context: Option<Box<ActivitySessionContext>>,
    pub blocking_status: ActivityBlockingStatus,
    /// Native current lock-wait verdict, independent of owner visibility or measured wait duration.
    /// None means the source could not establish whether this actor is waiting on a lock.
    pub waiting_on_lock: Option<bool>,
    pub key: ActivityKey,
    pub idle: bool,
    pub scope: ActivityScopeBasis,
    pub state: String,
    pub database: Option<String>,
    pub command: Option<String>,
    /// Age of the previous request start, separate from current request execution time.
    pub last_request_age_ms: Option<String>,
    /// None means the metadata source did not measure transaction state.
    pub open_transactions: Option<u32>,
    pub elapsed_ms: Option<String>,
    pub transaction_age_ms: Option<String>,
    /// These are running totals for different native lifetimes. Never add them or infer a rate from one poll.
    pub session_counters: ActivityCounters,
    pub request_counters: Option<ActivityCounters>,
    pub wait: Option<ActivityWait>,
    pub query_hash: Option<String>,
    /// Source-owned current-statement identity, independent of query hash or SQL text. Native qualification and
    /// observation bounds remain necessary before using it for a later, focused read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement_identity: Option<NativeIdentity>,
    pub client: Option<ActivityClient>,
    pub statement: DefinitionPayload,
}
impl ActivityObservation {
    /// Recorded prelogin/pool-reset context withdraws the source's native generation contract. Unacquired
    /// metadata adds no authority and preserves other sources' existing identity validation.
    pub fn has_session_generation_authority(&self) -> bool {
        self.session_context.as_deref().is_none_or(|context| {
            !matches!(context.lifecycle, SessionLifecycle::PoolResetPrelogin | SessionLifecycle::Connecting)
                && context.native.as_deref().is_none_or(|native| match native {
                    SessionNativeContext::Mssql(native) => native.login_established,
                })
        })
    }

    /// Reject a conflicting label on claimed request-database context. Other scope bases remain unowned context;
    /// accepting them here does not establish membership in the selected database.
    pub fn database_context_is_consistent(&self, database: &str) -> bool {
        self.scope != ActivityScopeBasis::RequestDatabase
            || self.database.as_deref().is_none_or(|observed| observed == database)
    }

    pub fn is_valid(&self) -> bool {
        self.key.is_valid()
            && self.session_context.as_deref().is_none_or(|context| context.is_valid_for(&self.key, self.idle))
            && (!self.idle || self.key.request.is_none())
            && identifier(&self.state)
            && self.database.as_deref().is_none_or(|value| !value.is_empty() && value.len() <= 512)
            && self.command.as_deref().is_none_or(identifier)
            && self.last_request_age_ms.as_deref().is_none_or(counter)
            && self.elapsed_ms.as_deref().is_none_or(counter)
            && self.transaction_age_ms.as_deref().is_none_or(counter)
            && self.session_counters.is_valid()
            && self.request_counters.as_ref().is_none_or(ActivityCounters::is_valid)
            && (self.request_counters.is_some() == self.key.request.is_some())
            && self.wait.as_ref().is_none_or(|wait| {
                wait.native_type.as_deref().is_none_or(identifier)
                    && wait.elapsed_ms.as_deref().is_none_or(counter)
                    && wait.identified_resources.len() <= crate::wait_resources::MAX_IDENTIFIED_WAIT_RESOURCES
                    && wait.identified_resources.iter().all(crate::ObservedWaitResource::is_valid)
                    && wait.identified_resources.iter().collect::<std::collections::HashSet<_>>().len()
                        == wait.identified_resources.len()
                    && wait.owners.len() <= 64
                    && wait.owners.iter().all(ActivityWaitOwner::is_valid)
                    && wait.resources.as_ref().is_none_or(|resources| {
                        resources.len() <= 64
                            && resources.iter().all(|resource| {
                                resource.session == self.key.session.id
                                    && [&resource.mode, &resource.resource]
                                        .into_iter()
                                        .flatten()
                                        .all(|value| identifier(value))
                                    && [&resource.object, &resource.index]
                                        .into_iter()
                                        .flatten()
                                        .all(|value| value.len() <= 2048)
                            })
                    })
            })
            && self.query_hash.as_deref().is_none_or(identifier)
            && self.statement_identity.as_ref().is_none_or(|identity| self.key.request.is_some() && identity.is_valid())
            && self.client.as_ref().is_none_or(ActivityClient::is_valid)
            && self.statement.is_valid()
    }
    pub fn redact(&mut self, engine: crate::Engine) {
        if let DefinitionPayload::Available { text, .. } = &self.statement {
            self.statement = DefinitionPayload::sql(text, engine);
        }
    }
    pub fn counters(&self) -> &ActivityCounters {
        self.request_counters.as_ref().unwrap_or(&self.session_counters)
    }
    pub fn blocking_uncertain(&self, policy: &ActivityPolicy) -> bool {
        self.blocking_status == ActivityBlockingStatus::Unknown
            && self.scope != ActivityScopeBasis::OtherDatabase
            && self.scope != ActivityScopeBasis::BlockingReference
            && self.key.request.is_some()
            && self
                .elapsed_ms
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|age| age >= policy.blocked_after_ms.into())
    }
    pub fn conditions(&self, policy: &ActivityPolicy) -> Vec<ActivityCondition> {
        let mut conditions = Vec::new();
        if !self.has_session_generation_authority()
            || matches!(self.scope, ActivityScopeBasis::BlockingReference | ActivityScopeBasis::OtherDatabase)
        {
            return conditions;
        }
        if self.key.request.is_some() {
            if self
                .elapsed_ms
                .as_ref()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|value| value >= u64::from(policy.running_after_ms))
            {
                conditions.push(ActivityCondition::LongRunning);
            }
            if self.wait.as_ref().is_some_and(|wait| {
                (self.waiting_on_lock == Some(true) || wait.owners.iter().any(ActivityWaitOwner::is_blocker))
                    && wait
                        .elapsed_ms
                        .as_ref()
                        .and_then(|value| value.parse::<u64>().ok())
                        .is_some_and(|value| value >= u64::from(policy.blocked_after_ms))
            }) {
                conditions.push(ActivityCondition::Blocked);
            }
        }
        conditions
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityScopeBasis {
    RequestDatabase,
    TransactionDatabase,
    SessionDatabase,
    BlockingReference,
    OtherDatabase,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityCondition {
    LongRunning,
    Blocked,
    IdleBlocker,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityProgress {
    FirstObservation,
    ChangedCounters,
    NoObservedCounterChange,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[ts(export)]
pub struct BlockingObservation {
    pub waiter: ActivityKey,
    pub owner: ActivityWaitOwner,
    pub owner_observed: bool,
}
impl BlockingObservation {
    /// Equality of reported session tokens cannot establish which request owns a lock, even when both session
    /// generations agree. Keep the native relationship without manufacturing a cross-request owner.
    pub fn reports_waiter_session_id(&self) -> bool {
        matches!(&self.owner, ActivityWaitOwner::Session { id, .. } if *id == self.waiter.session.id)
    }
    pub fn is_valid(&self) -> bool {
        self.waiter.is_valid()
            && self.waiter.request.is_some()
            && self.owner.is_valid()
            && self.owner.is_blocker()
            && (!self.owner_observed || self.owner.session().is_some())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityDetailStatus {
    NotNeeded,
    Collected,
    Unavailable { code: crate::ErrorCode },
}
/// The native root predicate defines what a complete point observation can establish. It is not a claim about
/// every session, parallel task or distributed component associated with the database.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityPopulation {
    ClientBackends,
    ActiveRequestsOpenTransactionsAndBlockers,
    UserSessionsCurrentRequestsAndBlockers,
    DefaultDatabaseConnections,
    NodeClientBackendsAndConnectionWorkers,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivitySnapshotDescription {
    pub epoch: Option<String>,
    /// Observed connected database updateability, not login/session permissions or requested routing intent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub database_writable: Option<bool>,
    pub observed_from: String,
    pub observed_until: String,
    pub at: String,
    pub scope: crate::MeasurementScope,
    pub population: ActivityPopulation,
    pub native_rows: u32,
    pub observations: u32,
    pub coverage: StateSourceCoverage,
}
impl ActivitySnapshotDescription {
    pub fn is_valid(&self) -> bool {
        self.epoch.as_deref().is_none_or(identifier)
            && (self.epoch.is_some() || !self.coverage.complete_scope)
            && ObservationBounds::new(&self.observed_from, &self.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.at))
            && self.coverage.is_valid()
            && self.coverage.records == self.observations
            && (!self.coverage.complete_scope || self.observations == self.native_rows)
            && (!self.coverage.complete_scope
                || matches!(
                    self.scope,
                    crate::MeasurementScope::Database
                        | crate::MeasurementScope::Node
                        | crate::MeasurementScope::Cluster
                ))
            && self.observations <= self.native_rows
            && self.native_rows <= 100_001
    }
}

pub struct ActivityEvidence {
    pub observations: Vec<ActivityObservation>,
    pub blocking: Vec<BlockingObservation>,
    pub conditions: BTreeMap<ActivityKey, Vec<ActivityCondition>>,
    pub uncertain: BTreeSet<ActivityKey>,
}
impl ActivityEvidence {
    /// All native owner relationships in one validated frame, without applying incident-age selection policy.
    pub fn blocking_observations(rows: &[ActivityObservation]) -> Result<Vec<BlockingObservation>> {
        let index = ActivityIndex::new(rows)?;
        Ok(blocking_observations(&index, rows))
    }
    pub fn blocking_for_frame(frame: &ActivityFrameEvidence<'_>) -> Vec<BlockingObservation> {
        let rows = frame.rows().map(|row| row.observation.clone()).collect::<Vec<_>>();
        blocking_observations(frame.index(), &rows)
    }
    pub fn validate(rows: &[ActivityObservation]) -> Result<()> {
        ActivityIndex::new(rows).map(|_| ())
    }
    pub fn validate_references<'a>(rows: impl IntoIterator<Item = &'a ActivityObservation>) -> Result<()> {
        ActivityIndex::new(rows).map(|_| ())
    }
    /// Validate one supplied population and return only its recorded native-generation refusals. Absence from
    /// this set supplies no source, scope, complete-population or positive generation authority.
    pub fn disqualified_session_generations<'a>(
        rows: impl IntoIterator<Item = &'a ActivityObservation>,
    ) -> Result<BTreeSet<NativeActivityIdentity>> {
        Ok(ActivityIndex::new(rows)?.disqualified_sessions)
    }
    /// Recorded refusals from borrowed points only. Repeated workers and different observed generations do not
    /// form a complete Activity frame; absence here supplies no positive generation or population authority.
    pub fn observed_session_generation_refusals<'a>(
        rows: impl IntoIterator<Item = &'a ActivityObservation>,
    ) -> Result<BTreeSet<NativeActivityIdentity>> {
        let mut refused = BTreeSet::new();
        for row in rows {
            if !row.is_valid() {
                return Err(CollectionStop::InvalidPage.error());
            }
            refused.extend(recorded_generation_refusal(row).cloned());
        }
        Ok(refused)
    }
    /// Follow previously observed blockers when current native lock details cannot establish their status.
    pub fn context_for(rows: &[ActivityObservation], roots: &BTreeSet<ActivityKey>) -> Result<BTreeSet<ActivityKey>> {
        let index = ActivityIndex::new(rows)?;
        Ok(context(&roots.iter().cloned().collect::<Vec<_>>(), &index))
    }
    pub fn context_for_frame(
        frame: &ActivityFrameEvidence<'_>,
        roots: &BTreeSet<ActivityKey>,
    ) -> BTreeSet<ActivityKey> {
        context(&roots.iter().cloned().collect::<Vec<_>>(), frame.index())
    }
    pub fn select(rows: &[ActivityObservation], policy: &ActivityPolicy) -> Result<Self> {
        Self::select_index(&ActivityIndex::new(rows)?, policy)
    }
    pub fn select_frame(frame: &ActivityFrameEvidence<'_>, policy: &ActivityPolicy) -> Result<Self> {
        Self::select_index(frame.index(), policy)
    }
    fn select_index(index: &ActivityIndex<'_>, policy: &ActivityPolicy) -> Result<Self> {
        policy.validate()?;
        let mut conditions: BTreeMap<_, _> = index
            .by_key
            .values()
            .filter(|row| index.has_session_generation_authority(&row.key.session))
            .filter_map(|row| {
                let conditions = row.conditions(policy);
                (!conditions.is_empty()).then(|| (row.key.clone(), conditions))
            })
            .collect();
        let blocked_roots: Vec<_> = conditions
            .iter()
            .filter(|(_, conditions)| conditions.contains(&ActivityCondition::Blocked))
            .map(|(key, _)| key.clone())
            .collect();
        let blocked_context = context(&blocked_roots, index);
        for key in &blocked_context {
            let row = index.by_key[key];
            if index.has_session_generation_authority(&row.key.session)
                && !matches!(row.scope, ActivityScopeBasis::BlockingReference | ActivityScopeBasis::OtherDatabase)
                && row.idle
                && row.key.request.is_none()
                && row.key.transaction.is_some()
                && row.open_transactions.is_some_and(|count| count > 0)
            {
                conditions.entry(key.clone()).or_default().push(ActivityCondition::IdleBlocker);
            }
        }
        let uncertain: BTreeSet<_> =
            index.by_key.values().filter(|row| row.blocking_uncertain(policy)).map(|row| row.key.clone()).collect();
        let roots = conditions.keys().chain(uncertain.iter()).cloned().collect::<Vec<_>>();
        let selected = context(&roots, index);
        let observations: Vec<_> = selected.iter().map(|key| (*index.by_key[key]).clone()).collect();
        let blocking = blocking_observations(index, &observations);
        Ok(Self { observations, blocking, conditions, uncertain })
    }
}

fn blocking_observations(index: &ActivityIndex<'_>, rows: &[ActivityObservation]) -> Vec<BlockingObservation> {
    rows.iter()
        .filter(|row| row.key.request.is_some() && index.has_session_generation_authority(&row.key.session))
        .flat_map(|row| {
            row.wait.iter().flat_map(|wait| &wait.owners).filter(|owner| owner.is_blocker()).map(move |owner| {
                let owner_observed =
                    owner.session().is_some_and(|session| index.has_session_generation_authority(&session));
                BlockingObservation { waiter: row.key.clone(), owner: owner.clone(), owner_observed }
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn recorded_generation_refusal(row: &ActivityObservation) -> Option<&NativeActivityIdentity> {
    (!row.has_session_generation_authority()).then_some(&row.key.session)
}

pub(super) struct ActivityIndex<'a> {
    pub(super) by_key: BTreeMap<ActivityKey, &'a ActivityObservation>,
    pub(super) by_session: BTreeMap<NativeActivityIdentity, Vec<&'a ActivityObservation>>,
    disqualified_sessions: BTreeSet<NativeActivityIdentity>,
}
impl<'a> ActivityIndex<'a> {
    pub(super) fn new(rows: impl IntoIterator<Item = &'a ActivityObservation>) -> Result<Self> {
        let mut by_key = BTreeMap::new();
        let mut by_session: BTreeMap<NativeActivityIdentity, Vec<&ActivityObservation>> = BTreeMap::new();
        let mut generations = BTreeMap::new();
        let mut disqualified_sessions = BTreeSet::new();
        for row in rows {
            if !row.is_valid() || by_key.insert(row.key.clone(), row).is_some() {
                return Err(CollectionStop::InvalidPage.error());
            }
            if generations
                .insert(row.key.session.id.clone(), row.key.session.generation.clone())
                .is_some_and(|before| before != row.key.session.generation)
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            by_session.entry(row.key.session.clone()).or_default().push(row);
            disqualified_sessions.extend(recorded_generation_refusal(row).cloned());
        }
        Ok(Self { by_key, by_session, disqualified_sessions })
    }
    pub(super) fn has_session_generation_authority(&self, identity: &NativeActivityIdentity) -> bool {
        self.by_session.contains_key(identity) && !self.session_generation_is_disqualified(identity)
    }
    pub(super) fn session_generation_is_disqualified(&self, identity: &NativeActivityIdentity) -> bool {
        self.disqualified_sessions.contains(identity)
    }
}

fn context(roots: &[ActivityKey], index: &ActivityIndex<'_>) -> BTreeSet<ActivityKey> {
    let mut pending: VecDeque<_> = roots.iter().filter(|key| index.by_key.contains_key(*key)).cloned().collect();
    let mut selected = BTreeSet::new();
    while let Some(key) = pending.pop_front() {
        if !selected.insert(key.clone()) {
            continue;
        }
        let row = index.by_key[&key];
        if !index.has_session_generation_authority(&row.key.session) {
            continue;
        }
        for owner in row.wait.iter().flat_map(|wait| &wait.owners) {
            if let Some(session) = owner.session() {
                for row in index.by_session.get(&session).into_iter().flatten() {
                    pending.push_back(row.key.clone());
                }
            }
        }
    }
    selected
}

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.chars().all(|ch| !ch.is_control())
}
pub(super) fn counter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityReferenceCoverage {
    pub observations: u32,
    pub complete: bool,
}

#[cfg(test)]
mod baseline_tests;
#[cfg(test)]
pub(crate) mod tests;
