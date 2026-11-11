//! Durable, bounded verification pages. A private progress record is never an externally deserializable proof.
use super::{
    contract::MAX_RANGE_RECORDS,
    recovery::CommittedRange,
    sink::{ReceiptPage, TelemetrySink, VerificationBudget},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, MAX_CURSOR_BYTES},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PAGES: u32 = 4096;
const MAX_PHYSICAL_ROWS: u32 = (MAX_RANGE_RECORDS * 4) as u32;
const MAX_CURSOR_HISTORY_BYTES: usize = 1024 * 1024;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct VerifiedRange {
    range: CommittedRange,
    receipts_digest: String,
}
impl VerifiedRange {
    pub fn committed(&self) -> &CommittedRange {
        &self.range
    }
    pub fn receipts_digest(&self) -> &str {
        &self.receipts_digest
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerificationProgress {
    manifest: String,
    budget: VerificationBudget,
    cursor: Option<String>,
    visited: BTreeSet<String>,
    seen: Vec<u8>,
    seen_rows: u32,
    pages: u32,
    physical_rows: u32,
}
pub(crate) enum PageVerification {
    Pending,
    Complete(VerifiedRange),
}
impl VerificationBudget {
    pub(crate) fn validate(self) -> Result<()> {
        if self.page_rows == 0
            || self.page_rows > 10_000
            || self.max_pages == 0
            || self.max_pages > MAX_PAGES
            || self.max_physical_rows == 0
            || self.max_physical_rows > MAX_PHYSICAL_ROWS
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(())
    }
}
impl VerificationProgress {
    pub(crate) fn new(range: &CommittedRange, budget: VerificationBudget) -> Result<Self> {
        budget.validate()?;
        let rows = range.manifest().expected_receipts().count();
        if rows > budget.max_physical_rows as usize {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(Self {
            manifest: range.manifest_digest().into(),
            budget,
            cursor: None,
            visited: BTreeSet::new(),
            seen: vec![0; rows.div_ceil(8)],
            seen_rows: 0,
            pages: 0,
            physical_rows: 0,
        })
    }
    pub(crate) fn uses(&self, budget: VerificationBudget) -> bool {
        self.budget == budget
    }
    pub(crate) async fn step(&mut self, sink: &dyn TelemetrySink, range: &CommittedRange) -> Result<PageVerification> {
        self.validate(range)?;
        if self.pages == self.budget.max_pages {
            return Err(DeliveryProblem::Capacity.error());
        }
        sink.validate_contract(&range.manifest().contract).await?;
        let page = sink.receipts(range, self.cursor.as_deref(), self.budget.page_rows).await?;
        // Apply a whole page atomically. An invalid/retired/timed-out page cannot persist partial membership.
        let mut next = self.clone();
        match next.accept(range, page)? {
            Some(verified) => Ok(PageVerification::Complete(verified)),
            None => {
                *self = next;
                Ok(PageVerification::Pending)
            }
        }
    }
    fn validate(&self, range: &CommittedRange) -> Result<()> {
        let rows = range.manifest().expected_receipts().count();
        let seen = self.seen.iter().map(|byte| byte.count_ones()).sum::<u32>();
        if self.manifest != range.manifest_digest()
            || self.seen.len() != rows.div_ceil(8)
            || seen != self.seen_rows
            || self.seen_rows as usize > rows
            || self.seen_rows > self.physical_rows
            || self.physical_rows > self.budget.max_physical_rows
            || u64::from(self.physical_rows) > u64::from(self.pages) * u64::from(self.budget.page_rows)
            || self.pages > self.budget.max_pages
            || self.visited.len() != self.pages as usize
            || self.visited.iter().map(String::len).sum::<usize>() > MAX_CURSOR_HISTORY_BYTES
            || self.visited.iter().any(|cursor| cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES)
            || self.cursor.as_ref().is_some_and(|cursor| !self.visited.contains(cursor))
            || (self.cursor.is_none() && self.pages != 0)
        {
            return Err(invalid());
        }
        if !rows.is_multiple_of(8) && self.seen.last().is_some_and(|byte| byte >> (rows % 8) != 0) {
            return Err(invalid());
        }
        Ok(())
    }
    fn accept(&mut self, range: &CommittedRange, page: ReceiptPage) -> Result<Option<VerifiedRange>> {
        if page.rows.len() > self.budget.page_rows as usize || !page.complete {
            return Err(invalid());
        }
        self.physical_rows = self
            .physical_rows
            .checked_add(page.rows.len() as u32)
            .filter(|rows| *rows <= self.budget.max_physical_rows)
            .ok_or_else(|| DeliveryProblem::Capacity.error())?;
        let expected = range
            .manifest()
            .expected_receipts()
            .enumerate()
            .map(|(ordinal, row)| (&row.key, (ordinal, row)))
            .collect::<BTreeMap<_, _>>();
        for actual in page.rows {
            let (ordinal, wanted) = expected.get(&actual.key).copied().ok_or_else(invalid)?;
            if *wanted != actual {
                return Err(invalid());
            }
            let byte = &mut self.seen[ordinal / 8];
            let mask = 1u8 << (ordinal % 8);
            if *byte & mask == 0 {
                *byte |= mask;
                self.seen_rows += 1;
            }
        }
        self.pages += 1;
        match page.next_cursor {
            Some(cursor)
                if !cursor.is_empty() && cursor.len() <= MAX_CURSOR_BYTES && self.visited.insert(cursor.clone()) =>
            {
                if self.visited.iter().map(String::len).sum::<usize>() > MAX_CURSOR_HISTORY_BYTES {
                    return Err(DeliveryProblem::Capacity.error());
                }
                self.cursor = Some(cursor);
                Ok(None)
            }
            Some(_) => Err(invalid()),
            None => {
                if self.seen_rows as usize != expected.len() {
                    return Err(DeliveryProblem::NotReady.error());
                }
                Ok(Some(VerifiedRange { range: range.clone(), receipts_digest: range.manifest().receipts_digest()? }))
            }
        }
    }
}
pub async fn verify(
    sink: &dyn TelemetrySink,
    range: &CommittedRange,
    budget: VerificationBudget,
) -> Result<VerifiedRange> {
    let mut progress = VerificationProgress::new(range, budget)?;
    loop {
        match progress.step(sink, range).await? {
            PageVerification::Pending => (),
            PageVerification::Complete(range) => return Ok(range),
        }
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
