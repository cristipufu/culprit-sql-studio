//! A bounded original roster can supply contemporary target-writing plans. Neither a writer nor a waiter is
//! thereby a latch owner, an allocator, a complete population or a measured share of contention.
use super::*;

mod binding;
mod focus;
pub use binding::{MssqlPageWriteAccess, MssqlPageWritePlanProof, PageWriteOperation};
pub use focus::{PageWaitWriterFocus, PageWaitWriterRequest};

pub const MAX_PAGE_WAIT_WRITER_CANDIDATES: usize = 8;
pub const MAX_PAGE_WAIT_WRITE_ACCESSES: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWriterTargetState {
    CurrentMapped,
    NoLongerObserved,
    ChangedTarget,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWriterRelation {
    TargetObjectWrite,
    ReadOnlyPlan,
    OtherTarget,
    Unsupported,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageWaitWriterCandidate {
    pub parent: DiagnosticEvidenceRef,
    pub request_state: PageWaitRequestState,
    /// Fresh, guarded context for the same original request. Missing/changed requests do not acquire SQL.
    pub request: Option<ActivityObservation>,
    pub plan_identity: Option<NativeIdentity>,
    /// A selected native plan, never execution of the application statement or a cached hash-only join.
    pub plan: DefinitionPayload,
    pub relation: PageWriterRelation,
    pub binding: Option<MssqlPageWritePlanProof>,
}

impl PageWaitWriterCandidate {
    pub fn is_valid(&self) -> bool {
        self.parent.is_valid()
            && self.parent.stream == TelemetryStream::ActivityObservations
            && self.plan.is_valid()
            && self.plan_identity.as_ref().is_none_or(NativeIdentity::is_valid)
            && self.request.as_ref().is_none_or(ActivityObservation::is_valid)
            && self.binding.as_ref().is_none_or(MssqlPageWritePlanProof::is_valid)
            && (self.request_state == PageWaitRequestState::SameStatement) == self.request.is_some()
            && (self.relation == PageWriterRelation::TargetObjectWrite) == self.binding.is_some()
            && (self.request_state == PageWaitRequestState::SameStatement
                || self.relation == PageWriterRelation::Unavailable
                    && !self.plan.is_available()
                    && self.plan_identity.is_none())
            && (self.plan.is_available() || self.plan_identity.is_none() && self.binding.is_none())
            && (self.binding.is_none() || self.plan.is_available() && self.plan_identity.is_some())
            && (!matches!(self.relation, PageWriterRelation::ReadOnlyPlan | PageWriterRelation::OtherTarget)
                || self.plan.is_available()
                    && self.plan_identity.is_some()
                    && self.request.as_ref().is_some_and(|request| request.statement.is_available()))
    }

    pub fn redact(&mut self, engine: crate::Engine) {
        if let Some(request) = &mut self.request {
            request.redact(engine);
        }
        if let DefinitionPayload::Available { text, .. } = &self.plan {
            self.plan = DefinitionPayload::plan(text, engine);
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageWaitWriterObservation {
    pub observed_from: String,
    pub observed_until: String,
    pub focus_key: String,
    pub target_state: PageWriterTargetState,
    /// The newly observed waiter and mapping retain their own point, separate from the original page receipt.
    pub page: Option<Box<PageWaitObservation>>,
    /// One result for every admitted original candidate. This roster is never all writers at the resource.
    pub candidates: Vec<PageWaitWriterCandidate>,
}

impl PageWaitWriterObservation {
    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else { return false };
        let mut parents = BTreeSet::new();
        valid_digest(&self.focus_key)
            && self.candidates.len() <= MAX_PAGE_WAIT_WRITER_CANDIDATES
            && self.page.as_ref().is_none_or(|page| {
                page.is_valid() && bounds.contains(&page.observed_from) && bounds.contains(&page.observed_until)
            })
            && (self.target_state != PageWriterTargetState::CurrentMapped
                || self.page.as_ref().is_some_and(|page| page.target.is_some()))
            && self.candidates.iter().all(|candidate| {
                candidate.is_valid()
                    && parents.insert(candidate.parent.record_id.clone())
                    && (self.target_state == PageWriterTargetState::CurrentMapped
                        || matches!(
                            candidate.relation,
                            PageWriterRelation::Unavailable | PageWriterRelation::Unsupported
                        ) && candidate.binding.is_none())
            })
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("page-wait-writers-v1", &self.focus_key))
    }

    pub fn redact(&mut self, engine: crate::Engine) {
        if let Some(page) = &mut self.page {
            page.redact(engine);
        }
        for candidate in &mut self.candidates {
            candidate.redact(engine);
        }
    }
}

impl MappedPageTarget {
    /// A stable catalog/file relation at checked bounds, not proof of uninterrupted physical page identity.
    /// LSNs and next-page links may change during legitimate writes and do not supply an incarnation.
    pub fn writer_target_key(&self) -> Result<String> {
        let Some(object) = &self.object else { return Err(invalid()) };
        let index_type = object.native.qualifiers.get("index_type").map(String::as_str);
        let index_id = self.header.index_id.as_deref();
        let catalog_index = object.native.qualifiers.get("index_id").map(String::as_str);
        let qualified_index = match self.kind {
            PageWaitTargetKind::HeapData => {
                index_type == Some("0") && index_id == Some("0") && object.index_name.is_none()
            }
            PageWaitTargetKind::IndexLeaf | PageWaitTargetKind::IndexBranch => {
                matches!(index_type, Some("1" | "2"))
                    && object.index_name.is_some()
                    && index_id
                        .is_some_and(|id| id.parse::<i32>().is_ok_and(|value| value > 0 && value.to_string() == id))
            }
            _ => false,
        };
        if !self.is_valid()
            || !matches!(self.scope, PageTargetScope::RequestDatabase)
            || !matches!(
                self.kind,
                PageWaitTargetKind::IndexLeaf | PageWaitTargetKind::IndexBranch | PageWaitTargetKind::HeapData
            )
            || self.header.object_id.as_deref() != Some(object.native.id.as_str())
            || !object.native.qualifiers.contains_key("created_without_offset")
            || !object.native.qualifiers.contains_key("modified_without_offset")
            || !qualified_index
            || catalog_index != index_id
        {
            return Err(invalid());
        }
        hash(&(
            "mssql-page-writer-target-v1",
            &self.database,
            &self.file,
            self.kind,
            self.header.database_id,
            self.header.file_id,
            self.header.page_id,
            &self.header.object_id,
            &self.header.index_id,
            &self.header.partition_id,
            &self.header.allocation_unit_id,
            object,
        ))
    }
}

fn hash(value: &impl Serialize) -> Result<String> {
    serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
