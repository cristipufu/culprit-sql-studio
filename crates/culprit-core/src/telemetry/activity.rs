//! Provider-neutral activity facts and bounded selection of persistent issues and their blocker context.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityPolicy {
    pub blocked_after_ms: u32,
    pub running_after_ms: u32,
    pub update_every_ms: u32,
}
impl Default for ActivityPolicy {
    fn default() -> Self {
        Self { blocked_after_ms: 30_000, running_after_ms: 60_000, update_every_ms: 300_000 }
    }
}
impl ActivityPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(1_000..=604_800_000).contains(&self.blocked_after_ms)
            || !(1_000..=604_800_000).contains(&self.running_after_ms)
            || !(60_000..=86_400_000).contains(&self.update_every_ms)
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
}
impl ActivityCounters {
    fn is_valid(&self) -> bool {
        [&self.cpu_ms, &self.logical_reads, &self.physical_reads, &self.writes]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(counter))
    }
    pub fn progress(&self, before: &Self) -> ActivityProgress {
        let pairs = [
            (&self.cpu_ms, &before.cpu_ms),
            (&self.logical_reads, &before.logical_reads),
            (&self.physical_reads, &before.physical_reads),
            (&self.writes, &before.writes),
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

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityWait {
    pub native_type: Option<String>,
    pub elapsed_ms: Option<String>,
    pub owners: Vec<ActivityWaitOwner>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityClient {
    pub application: Option<String>,
    pub user: Option<String>,
    pub host: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityObservation {
    pub key: ActivityKey,
    pub idle: bool,
    pub scope: ActivityScopeBasis,
    pub state: String,
    pub database: Option<String>,
    pub command: Option<String>,
    /// Age of the previous request start, separate from current request execution time.
    pub last_request_age_ms: Option<String>,
    pub open_transactions: u32,
    pub elapsed_ms: Option<String>,
    pub transaction_age_ms: Option<String>,
    /// These are running totals for different native lifetimes. Never add them or infer a rate from one poll.
    pub session_counters: ActivityCounters,
    pub request_counters: Option<ActivityCounters>,
    pub wait: Option<ActivityWait>,
    pub query_hash: Option<String>,
    pub client: Option<ActivityClient>,
    pub statement: DefinitionPayload,
}
impl ActivityObservation {
    pub fn is_valid(&self) -> bool {
        self.key.is_valid()
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
                    && wait.owners.len() <= 64
                    && wait.owners.iter().all(ActivityWaitOwner::is_valid)
            })
            && self.query_hash.as_deref().is_none_or(identifier)
            && self.client.as_ref().is_none_or(|client| {
                [&client.application, &client.user, &client.host]
                    .into_iter()
                    .all(|value| value.as_deref().is_none_or(|value| value.len() <= 512))
            })
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
    pub fn conditions(&self, policy: &ActivityPolicy) -> Vec<ActivityCondition> {
        let mut conditions = Vec::new();
        if matches!(self.scope, ActivityScopeBasis::BlockingReference | ActivityScopeBasis::OtherDatabase) {
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
                wait.owners.iter().any(ActivityWaitOwner::is_blocker)
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
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivitySnapshotDescription {
    pub epoch: String,
    pub at: String,
    pub database_wide: bool,
    pub policy: ActivityPolicy,
    pub native_rows: u32,
    pub qualifying: u32,
    pub observations: u32,
    pub blocking_edges: u32,
    pub details: ActivityDetailStatus,
}
impl ActivitySnapshotDescription {
    pub fn is_valid(&self) -> bool {
        identifier(&self.epoch)
            && valid_time(&self.at)
            && self.policy.validate().is_ok()
            && self.qualifying <= self.observations
            && self.observations <= self.native_rows
            && self.blocking_edges <= self.observations.saturating_mul(64)
            && (self.qualifying > 0 || (self.observations == 0 && self.blocking_edges == 0))
    }
}

pub struct ActivityEvidence {
    pub observations: Vec<ActivityObservation>,
    pub blocking: Vec<BlockingObservation>,
    pub conditions: BTreeMap<ActivityKey, Vec<ActivityCondition>>,
}
impl ActivityEvidence {
    pub fn validate(rows: &[ActivityObservation]) -> Result<()> {
        ActivityIndex::new(rows).map(|_| ())
    }
    pub fn select(rows: &[ActivityObservation], policy: &ActivityPolicy) -> Result<Self> {
        policy.validate()?;
        let ActivityIndex { by_key, by_session } = ActivityIndex::new(rows)?;
        let mut conditions: BTreeMap<_, _> = rows
            .iter()
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
        let blocked_context = context(&blocked_roots, &by_key, &by_session);
        for key in &blocked_context {
            let row = by_key[key];
            if !matches!(row.scope, ActivityScopeBasis::BlockingReference | ActivityScopeBasis::OtherDatabase)
                && row.idle
                && row.key.request.is_none()
                && row.key.transaction.is_some()
                && row.open_transactions > 0
            {
                conditions.entry(key.clone()).or_default().push(ActivityCondition::IdleBlocker);
            }
        }
        let selected = context(&conditions.keys().cloned().collect::<Vec<_>>(), &by_key, &by_session);
        let observations: Vec<_> = selected.iter().map(|key| (*by_key[key]).clone()).collect();
        let observed_sessions: BTreeSet<_> = observations.iter().map(|row| row.key.session.clone()).collect();
        let mut blocking = BTreeSet::new();
        for row in &observations {
            if row.key.request.is_none() {
                continue;
            }
            for owner in row.wait.iter().flat_map(|wait| &wait.owners).filter(|owner| owner.is_blocker()) {
                let owner_observed = owner.session().is_some_and(|session| observed_sessions.contains(&session));
                blocking.insert(BlockingObservation { waiter: row.key.clone(), owner: owner.clone(), owner_observed });
            }
        }
        Ok(Self { observations, blocking: blocking.into_iter().collect(), conditions })
    }
}
struct ActivityIndex<'a> {
    by_key: BTreeMap<ActivityKey, &'a ActivityObservation>,
    by_session: BTreeMap<NativeActivityIdentity, Vec<&'a ActivityObservation>>,
}
impl<'a> ActivityIndex<'a> {
    fn new(rows: &'a [ActivityObservation]) -> Result<Self> {
        let mut by_key = BTreeMap::new();
        let mut by_session: BTreeMap<NativeActivityIdentity, Vec<&ActivityObservation>> = BTreeMap::new();
        let mut generations = BTreeMap::new();
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
        }
        Ok(Self { by_key, by_session })
    }
}

fn context(
    roots: &[ActivityKey],
    by_key: &BTreeMap<ActivityKey, &ActivityObservation>,
    by_session: &BTreeMap<NativeActivityIdentity, Vec<&ActivityObservation>>,
) -> BTreeSet<ActivityKey> {
    let mut pending: VecDeque<_> = roots.iter().cloned().collect();
    let mut selected = BTreeSet::new();
    while let Some(key) = pending.pop_front() {
        if !selected.insert(key.clone()) {
            continue;
        }
        for owner in by_key[&key].wait.iter().flat_map(|wait| &wait.owners) {
            if let Some(session) = owner.session() {
                for row in by_session.get(&session).into_iter().flatten() {
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
pub(super) fn valid_time(value: &str) -> bool {
    value.len() <= 64 && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityReferenceCoverage {
    pub observations: u32,
    pub blocking_edges: u32,
    pub unresolved_blockers: u32,
    pub complete: bool,
}

#[cfg(test)]
pub(crate) mod tests;
