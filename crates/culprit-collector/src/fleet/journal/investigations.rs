//! One immutable intent per episode shares admission transactions and bounded current replay receipts.
use super::*;
use crate::fleet::scheduler::{
    FocusMutation, FocusState, FocusStatus, MAX_FOCUSED_JOBS, ScheduleClock, ScheduleUpdate,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INTENT_BYTES: usize = 256 * 1024;
const MAX_INTENTS_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECEIPTS: usize = MAX_DIAGNOSTIC_EPISODES;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct FocusedReceipt {
    pub key: String,
    pub episode_id: String,
    pub episode_stage_key: String,
    pub settlement: FocusedSettlement,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(in crate::fleet) enum FocusedSettlement {
    Completed { result: DiagnosticCaptureRef },
    Cancelled { reason: DiagnosticAnalysisIssue },
}
impl FocusedReceipt {
    fn is_valid(&self) -> bool {
        valid_digest(&self.key)
            && valid_digest(&self.episode_id)
            && valid_digest(&self.episode_stage_key)
            && match &self.settlement {
                FocusedSettlement::Completed { result } => result.is_valid(),
                FocusedSettlement::Cancelled { .. } => true,
            }
    }
}
pub(in crate::fleet) enum FocusedStage {
    Outstanding,
    Completed,
    Cancelled { key: String },
}

struct StoredReceipt {
    key: String,
    episode: String,
    stage: String,
    json: String,
}
impl StoredReceipt {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { key: row.get(0)?, episode: row.get(1)?, stage: row.get(2)?, json: row.get(3)? })
    }
    fn decode(self) -> Result<FocusedReceipt> {
        let receipt = decode_receipt(&self.json)?;
        if receipt.key != self.key || receipt.episode_id != self.episode || receipt.episode_stage_key != self.stage {
            return Err(state_failed());
        }
        Ok(receipt)
    }
}

impl FleetJournal {
    #[cfg(test)]
    pub(in crate::fleet) async fn focused_receipt(&self, stage: &str) -> Result<Option<FocusedReceipt>> {
        self.files
            .run(|| {
                let stage = stage.to_owned();
                move |files| receipt(&files.connection, None, &stage)
            })
            .await
    }
    pub(in crate::fleet) async fn focused_intents(&self) -> Result<Vec<DiagnosticInvestigationIntent>> {
        self.files.run(|| |files| files.focused_intents()).await
    }
    pub(in crate::fleet) async fn focused_intent(&self, key: &str) -> Result<Option<DiagnosticInvestigationIntent>> {
        self.files
            .run(|| {
                let key = key.to_owned();
                move |files| intent(&files.connection, &key)
            })
            .await
    }
    pub(in crate::fleet) async fn focused_stage(&self, stage: &str) -> Result<Option<FocusedStage>> {
        self.files
            .run(|| {
                let stage = stage.to_owned();
                move |files| files.focused_stage(&stage)
            })
            .await
    }
    pub(in crate::fleet) async fn focused_receipt_episode_ids(&self) -> Result<BTreeSet<String>> {
        self.files
            .run(|| {
                |files| {
                    let mut episodes = BTreeSet::new();
                    for receipt in receipts(&files.connection)? {
                        if !episodes.insert(receipt.episode_id) {
                            return Err(state_failed());
                        }
                    }
                    Ok(episodes)
                }
            })
            .await
    }
    pub(in crate::fleet) async fn enqueue_focused(
        &self,
        intent: DiagnosticInvestigationIntent,
        state: ScheduleUpdate,
    ) -> Result<ScheduleClock> {
        self.files.run(|| move |files| files.enqueue_focused(intent, state)).await
    }
    /// The host authenticates exact result consumption first, then releases queued parent ownership after this
    /// transaction. A lost acknowledgement leaves a safe pin for complete startup reconciliation.
    pub(in crate::fleet) async fn retire_focused(&self, state: ScheduleUpdate) -> Result<ScheduleClock> {
        self.files.run(|| move |files| files.settle_focused(state, None)).await
    }
    pub(in crate::fleet) async fn cancel_focused(
        &self,
        state: ScheduleUpdate,
        reason: DiagnosticAnalysisIssue,
    ) -> Result<ScheduleClock> {
        self.files.run(|| move |files| files.settle_focused(state, Some(reason))).await
    }
    /// This is a complete current working-set reconciliation, never a native latest-N or partial-source inventory.
    pub(in crate::fleet) async fn prune_focused_receipts(&self, retained: &BTreeSet<String>) -> Result<()> {
        if retained.len() > MAX_RECEIPTS || retained.iter().any(|id| !valid_digest(id)) {
            return Err(capacity());
        }
        self.files
            .run(|| {
                let retained = retained.clone();
                move |files| files.prune_focused_receipts(&retained)
            })
            .await
    }
}

impl Files {
    fn focused_intents(&self) -> Result<Vec<DiagnosticInvestigationIntent>> {
        Ok(intents(&self.connection)?.into_values().collect())
    }
    fn focused_stage(&self, stage: &str) -> Result<Option<FocusedStage>> {
        if !valid_digest(stage) {
            return Err(state_failed());
        }
        let active: Option<String> = self
            .connection
            .query_row("SELECT id FROM investigation_intents WHERE stage_key=?1", [stage], |row| row.get(0))
            .optional()
            .map_err(|_| state_failed())?;
        let completed = receipt(&self.connection, None, stage)?;
        match (active, completed) {
            (Some(key), None) => {
                intent(&self.connection, &key)?.ok_or_else(state_failed)?;
                Ok(Some(FocusedStage::Outstanding))
            }
            (None, Some(receipt)) => Ok(Some(match &receipt.settlement {
                FocusedSettlement::Completed { .. } => FocusedStage::Completed,
                FocusedSettlement::Cancelled { .. } => FocusedStage::Cancelled { key: receipt.key },
            })),
            (None, None) => Ok(None),
            _ => Err(state_failed()),
        }
    }
    fn enqueue_focused(
        &mut self,
        candidate: DiagnosticInvestigationIntent,
        state: ScheduleUpdate,
    ) -> Result<ScheduleClock> {
        let FocusMutation::Insert { key, state: focus } = state.focused.as_ref().ok_or_else(state_failed)? else {
            return Err(state_failed());
        };
        if !focus.is_valid() || !focus.job.matches_intent(&candidate) || *key != candidate.key()? {
            return Err(state_failed());
        }
        let json = serde_json::to_string(&candidate).map_err(|_| state_failed())?;
        if json.len() > MAX_INTENT_BYTES {
            return Err(capacity());
        }
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        if let Some(existing) = intent(&transaction, key)? {
            let current = super::scheduler::read_clock(&transaction)?.ok_or_else(state_failed)?;
            let stored_focus: String = transaction
                .query_row("SELECT state_json FROM admission_focused WHERE id=?1", [key], |row| row.get(0))
                .map_err(|_| state_failed())?;
            if existing == candidate
                && stored_focus == super::scheduler::encode(focus)?
                && serde_json::to_string(&current).map_err(|_| state_failed())?
                    == serde_json::to_string(&state.clock).map_err(|_| state_failed())?
            {
                return Ok(current);
            }
            return Err(DeliveryProblem::Conflict.error());
        }
        let (count, bytes): (u32, i64) = transaction
            .query_row(
                "SELECT count(*),coalesce(sum(length(CAST(intent_json AS BLOB))),0) FROM investigation_intents",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| state_failed())?;
        if count as usize >= MAX_FOCUSED_JOBS
            || bytes < 0
            || (bytes as usize).checked_add(json.len()).is_none_or(|bytes| bytes > MAX_INTENTS_BYTES)
        {
            return Err(capacity());
        }
        let conflict: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM investigation_intents WHERE episode_id=?1 OR stage_key=?2)",
                params![candidate.episode_id, candidate.episode_stage_key()?],
                |row| row.get(0),
            )
            .map_err(|_| state_failed())?;
        if conflict {
            return Err(DeliveryProblem::Conflict.error());
        }
        if let Some(receipt) = receipt(&transaction, Some(&candidate.episode_id), &candidate.episode_stage_key()?)? {
            if !matches!(receipt.settlement, FocusedSettlement::Cancelled { .. })
                || receipt.key == *key
                || receipt.episode_id != candidate.episode_id
                || receipt.episode_stage_key != candidate.episode_stage_key()?
            {
                return Err(DeliveryProblem::Conflict.error());
            }
            let deleted = transaction
                .execute("DELETE FROM investigation_receipts WHERE id=?1", [receipt.key])
                .map_err(|_| state_failed())?;
            if deleted != 1 {
                return Err(state_failed());
            }
        }
        transaction
            .execute(
                "INSERT INTO investigation_intents VALUES(?1,?2,?3,?4,?5)",
                params![key, candidate.episode_id, candidate.episode_stage_key()?, json, digest(json.as_bytes())],
            )
            .map_err(|_| state_failed())?;
        super::scheduler::apply_update(&transaction, &state)?;
        transaction.commit().map_err(|_| state_failed())?;
        Ok(state.clock)
    }
    fn settle_focused(
        &mut self,
        state: ScheduleUpdate,
        cancelled: Option<DiagnosticAnalysisIssue>,
    ) -> Result<ScheduleClock> {
        let FocusMutation::Remove { key } = state.focused.as_ref().ok_or_else(state_failed)? else {
            return Err(state_failed());
        };
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        let candidate = intent(&transaction, key)?.ok_or_else(state_failed)?;
        let json: String = transaction
            .query_row("SELECT state_json FROM admission_focused WHERE id=?1", [key], |row| row.get(0))
            .map_err(|_| state_failed())?;
        let focus: FocusState = super::scheduler::decode(&json)?;
        if !focus.is_valid() || !focus.job.matches_intent(&candidate) {
            return Err(state_failed());
        }
        let settlement = match (&focus.status, cancelled) {
            (FocusStatus::Completed { result }, None) => FocusedSettlement::Completed { result: result.clone() },
            (FocusStatus::Pending, Some(reason))
                if matches!(focus.source_work, SourceWorkState::NotStarted | SourceWorkState::Quiescent) =>
            {
                FocusedSettlement::Cancelled { reason }
            }
            _ => return Err(state_failed()),
        };
        let clock = super::scheduler::read_clock(&transaction)?.ok_or_else(state_failed)?;
        if clock.pending_job.is_some() {
            return Err(state_failed());
        }
        let count: u32 = transaction
            .query_row("SELECT count(*) FROM investigation_receipts", [], |row| row.get(0))
            .map_err(|_| state_failed())?;
        if count as usize >= MAX_RECEIPTS {
            return Err(capacity());
        }
        let receipt = FocusedReceipt {
            key: key.clone(),
            episode_id: candidate.episode_id.clone(),
            episode_stage_key: candidate.episode_stage_key()?,
            settlement,
        };
        if !receipt.is_valid() {
            return Err(state_failed());
        }
        transaction
            .execute(
                "INSERT INTO investigation_receipts VALUES(?1,?2,?3,?4)",
                params![key, receipt.episode_id, receipt.episode_stage_key, super::scheduler::encode(&receipt)?],
            )
            .map_err(|_| state_failed())?;
        super::scheduler::apply_update(&transaction, &state)?;
        let deleted =
            transaction.execute("DELETE FROM investigation_intents WHERE id=?1", [key]).map_err(|_| state_failed())?;
        if deleted != 1 {
            return Err(state_failed());
        }
        transaction.commit().map_err(|_| state_failed())?;
        Ok(state.clock)
    }
    fn prune_focused_receipts(&mut self, retained: &BTreeSet<String>) -> Result<()> {
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        for receipt in receipts(&transaction)? {
            if !retained.contains(&receipt.episode_id) {
                transaction
                    .execute("DELETE FROM investigation_receipts WHERE id=?1", [receipt.key])
                    .map_err(|_| state_failed())?;
            }
        }
        transaction.commit().map_err(|_| state_failed())
    }
}

fn receipts(connection: &Connection) -> Result<Vec<FocusedReceipt>> {
    let mut query = connection
        .prepare("SELECT id,episode_id,stage_key,receipt_json FROM investigation_receipts LIMIT ?1")
        .map_err(|_| state_failed())?;
    let rows = query
        .query_map([(MAX_RECEIPTS + 1) as u32], StoredReceipt::read)
        .map_err(|_| state_failed())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| state_failed())?;
    if rows.len() > MAX_RECEIPTS {
        return Err(state_failed());
    }
    rows.into_iter().map(StoredReceipt::decode).collect()
}

fn receipt(connection: &Connection, episode: Option<&str>, stage: &str) -> Result<Option<FocusedReceipt>> {
    let mut query = connection
        .prepare("SELECT id,episode_id,stage_key,receipt_json FROM investigation_receipts WHERE stage_key=?1 OR episode_id=?2 LIMIT 2")
        .map_err(|_| state_failed())?;
    let mut rows = query.query(params![stage, episode]).map_err(|_| state_failed())?;
    let result =
        rows.next().map_err(|_| state_failed())?.map(StoredReceipt::read).transpose().map_err(|_| state_failed())?;
    if rows.next().map_err(|_| state_failed())?.is_some() {
        return Err(state_failed());
    }
    result.map(StoredReceipt::decode).transpose()
}

pub(super) fn intents(connection: &Connection) -> Result<BTreeMap<String, DiagnosticInvestigationIntent>> {
    let mut query = connection
        .prepare(
            "SELECT id,episode_id,stage_key,intent_json,intent_sha FROM investigation_intents ORDER BY id LIMIT ?1",
        )
        .map_err(|_| state_failed())?;
    let mut rows = query.query([(MAX_FOCUSED_JOBS + 1) as u32]).map_err(|_| state_failed())?;
    let mut result = BTreeMap::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(|_| state_failed())? {
        let key: String = row.get(0).map_err(|_| state_failed())?;
        let episode: String = row.get(1).map_err(|_| state_failed())?;
        let stage: String = row.get(2).map_err(|_| state_failed())?;
        let json: String = row.get(3).map_err(|_| state_failed())?;
        let hash: String = row.get(4).map_err(|_| state_failed())?;
        bytes = bytes.checked_add(json.len()).ok_or_else(state_failed)?;
        if result.len() == MAX_FOCUSED_JOBS || bytes > MAX_INTENTS_BYTES {
            return Err(state_failed());
        }
        let candidate = decode_intent(&key, &episode, &stage, &json, &hash)?;
        if result.insert(key, candidate).is_some() {
            return Err(state_failed());
        }
    }
    Ok(result)
}
pub(super) fn intent(connection: &Connection, key: &str) -> Result<Option<DiagnosticInvestigationIntent>> {
    if !valid_digest(key) {
        return Err(state_failed());
    }
    let row: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT episode_id,stage_key,intent_json,intent_sha FROM investigation_intents WHERE id=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|_| state_failed())?;
    row.map(|(episode, stage, json, hash)| decode_intent(key, &episode, &stage, &json, &hash)).transpose()
}
fn decode_intent(
    key: &str,
    episode: &str,
    stage: &str,
    json: &str,
    hash: &str,
) -> Result<DiagnosticInvestigationIntent> {
    if json.len() > MAX_INTENT_BYTES || digest(json.as_bytes()) != hash {
        return Err(state_failed());
    }
    let candidate: DiagnosticInvestigationIntent = serde_json::from_str(json).map_err(|_| state_failed())?;
    if !candidate.is_valid()
        || candidate.key()? != key
        || candidate.episode_id != episode
        || candidate.episode_stage_key()? != stage
    {
        return Err(state_failed());
    }
    Ok(candidate)
}
fn decode_receipt(json: &str) -> Result<FocusedReceipt> {
    let receipt: FocusedReceipt = super::scheduler::decode(json)?;
    if !receipt.is_valid() {
        return Err(state_failed());
    }
    Ok(receipt)
}
fn capacity() -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}

#[cfg(test)]
mod tests;
