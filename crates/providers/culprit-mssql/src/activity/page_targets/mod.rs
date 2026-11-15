//! Explicit, bounded enrichment of pages the shared activity reader already observed. Request/coordinator waits
//! and native task waits identify sampled affected participants, never a transient latch holder, allocator or a
//! complete parallel-task population. The exact focused route checks its original coordinator only.
//! Header calls can perform I/O/latching, so this profile is opt-in and still requires native cost qualification.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-page-info-transact-sql
mod context;
mod fields;
mod focus;
#[cfg(test)]
pub(crate) use focus::tests::focus as test_focus;
mod index_context;
#[cfg(test)]
mod native_tests;
mod participants;
mod rows;
mod shared_tempdb;
mod sql;
mod task_waits;
#[cfg(test)]
pub(crate) mod tests;

use super::{ActivityContext, ActivityFrame, ActivityReader, ActivityScope};
use crate::read::{NativeRead, Parameter, ReadError, invalid_data, stops_remaining_reads};
use culprit_core::{ErrorCode, ObservedWaitResource, Result, WaitResourceKind, telemetry::*};
use rows::{HeaderRecord, PageAddress};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PAGES: usize = 8;
const MAX_SQL_BYTES: u32 = 16 * 1024;

pub(crate) struct PageWaitCapture {
    pub context: ActivityContext,
    pub snapshot: PageWaitSnapshot,
    pub limited: bool,
    pub guard: PageWaitGuard,
}
pub(crate) struct PageWaitGuard(context::LookupContext);
impl PageWaitCapture {
    pub(crate) fn collection_target(&self) -> CollectionTarget {
        match &self.guard.0.shared {
            Some(shared) if self.guard.0.shared_only => shared.target(),
            _ => self.context.database.clone().into(),
        }
    }
    pub(crate) fn source_partition(&self) -> Result<SourcePartition> {
        match &self.guard.0.shared {
            Some(shared) if self.guard.0.shared_only => Ok(shared.partition()),
            _ => self.guard.0.authority.source_partition(),
        }
    }
    pub(crate) fn task_visibility_unavailable(&self) -> bool {
        self.guard.0.task_visibility == Some(false)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LookupAdmission {
    ObservedPopulation,
    OriginalFocus,
}

struct Candidate {
    record: usize,
    resource: ObservedWaitResource,
    address: Option<PageAddress>,
    state: LookupState,
    before_check: Option<PageWaitRecheck>,
    after_check: Option<PageWaitRecheck>,
    statement_identity: Option<NativeIdentity>,
    plan_identity: Option<NativeIdentity>,
    index_operations: Option<PageIndexOperations>,
    task_wait: Option<MssqlPageTaskWait>,
}
enum LookupState {
    Pending,
    Unresolved(PageWaitResolution),
    Observed { target: Box<MappedPageTarget> },
}
impl Candidate {
    fn is_pending(&self) -> bool {
        matches!(self.state, LookupState::Pending)
    }
    fn withhold(&mut self, resolution: PageWaitResolution) {
        if resolution == PageWaitResolution::UnverifiedSource {
            self.statement_identity = None;
            self.plan_identity = None;
        }
        self.state = LookupState::Unresolved(resolution);
    }
}

impl<R: NativeRead> ActivityReader<R> {
    /// The caller owns confirmed close and stages the returned data until that close succeeds.
    pub(crate) async fn capture_page_wait_targets(&mut self) -> Result<PageWaitCapture> {
        self.capture_scan_page_wait_targets(false).await
    }

    pub(crate) async fn capture_scan_page_wait_targets(&mut self, include_shared: bool) -> Result<PageWaitCapture> {
        if !matches!(self.options.scope, ActivityScope::CurrentDatabase) {
            return Err(invalid_data());
        }
        let initial = context::initial_scan(self, include_shared).await?;
        let population = self.page_population().await?;
        if !initial.matches_initial(&population.frame) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.capture_selected_page_wait_targets(population, initial, LookupAdmission::ObservedPopulation).await
    }

    /// Caller admission must independently own this actual shared resource. The reader verifies that native
    /// resource on its existing connection; a configured route/name never grants shared-target authority.
    pub(crate) async fn capture_shared_tempdb_page_wait_targets(
        &mut self,
        resource: &SharedResourceIdentity,
    ) -> Result<PageWaitCapture> {
        if !matches!(self.options.scope, ActivityScope::CurrentDatabase) {
            return Err(invalid_data());
        }
        let initial = context::initial_shared(self, resource).await?;
        let population = self.page_population().await?;
        if !initial.matches_initial(&population.frame) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.capture_selected_page_wait_targets(population, initial, LookupAdmission::ObservedPopulation).await
    }

    async fn capture_selected_page_wait_targets(
        &mut self,
        population: task_waits::Population,
        mut initial: context::LookupContext,
        admission: LookupAdmission,
    ) -> Result<PageWaitCapture> {
        let task_waits::Population { mut frame, tasks, limited, task_visibility } = population;
        initial.task_visibility = task_visibility;
        let (mut candidates, selection_limited) = select(&frame, &tasks, &initial);
        let limited = limited || selection_limited;
        let keys = candidates.iter().map(|candidate| frame.records[candidate.record].facts.key.clone()).collect();
        if candidates.is_empty() {
            return self.finish_page_capture(frame, candidates, limited, initial).await;
        }
        for candidate in &mut candidates {
            if candidate.is_pending()
                && let Some(unavailable) = candidate.address.and_then(|address| initial.unavailable_for(address))
            {
                candidate.withhold(unavailable);
            }
        }
        if !candidates.iter().any(Candidate::is_pending) {
            if admission == LookupAdmission::ObservedPopulation {
                self.enrich_page_participants(&mut frame, &keys).await?;
            }
            return self.finish_page_capture(frame, candidates, limited, initial).await;
        }

        let indices = pending_indices(&candidates);
        let before = match self.page_participants(&frame, &indices, &candidates).await {
            Ok(rows) => rows,
            Err(error) if stops_remaining_reads(&error) => return Err(error),
            Err(error) => {
                mark_pending(&mut candidates, error_resolution(error.code));
                return self.finish_page_capture(frame, candidates, limited, initial).await;
            }
        };
        apply_request_checks(&frame, &mut candidates, &before);
        if admission == LookupAdmission::OriginalFocus {
            for candidate in &mut candidates {
                if candidate.is_pending()
                    && !candidate.before_check.as_ref().is_some_and(|check| {
                        check.request == PageWaitRequestState::SameStatement
                            && check.wait == PageWaitRelation::OriginalObserved
                    })
                {
                    candidate.withhold(PageWaitResolution::Unavailable);
                }
            }
        }
        let addresses = admitted_addresses(&candidates);
        if addresses.is_empty() {
            if admission == LookupAdmission::ObservedPopulation {
                self.enrich_page_participants(&mut frame, &keys).await?;
            }
            return self.finish_page_capture(frame, candidates, limited, initial).await;
        }
        self.admit_page_headers(&initial).await?;
        let headers_before = match self.page_headers(&addresses, &initial).await {
            Ok(rows) => rows,
            Err(error) if stops_remaining_reads(&error) => return Err(error),
            Err(error) => {
                mark_pending(&mut candidates, error_resolution(error.code));
                self.enrich_page_participants(&mut frame, &keys).await?;
                return self.finish_page_capture(frame, candidates, limited, initial).await;
            }
        };
        self.enrich_page_participants(&mut frame, &keys).await?;
        // The existing index function/catalog route is application-database scoped. A shared page does not
        // authorize that call against tempdb; its missing targeted index attribution remains explicit.
        let index_read = if initial.shared_only {
            index_context::ReadPlan::Empty
        } else {
            index_context::ReadPlan::prepare(&mut self.io, &initial, &headers_before).await?
        };
        self.admit_page_headers(&initial).await?;
        let index_points = index_read.acquire(&mut self.io).await?;
        for candidate in &mut candidates {
            candidate.index_operations = candidate.address.and_then(|address| index_points.get(&address).cloned());
            if candidate.address.is_some_and(|address| initial.shared_for(address).is_some()) && candidate.is_pending()
            {
                candidate.index_operations = Some(PageIndexOperations::Unavailable);
            }
        }
        let headers_after = match self.page_headers(&addresses, &initial).await {
            Ok(rows) => rows,
            Err(error) if stops_remaining_reads(&error) => return Err(error),
            Err(error) => {
                mark_pending(&mut candidates, error_resolution(error.code));
                return self.finish_page_capture(frame, candidates, limited, initial).await;
            }
        };
        let after = match self.page_participants(&frame, &indices, &candidates).await {
            Ok(rows) => rows,
            Err(error) if stops_remaining_reads(&error) => return Err(error),
            Err(error) => {
                mark_pending(&mut candidates, error_resolution(error.code));
                return self.finish_page_capture(frame, candidates, limited, initial).await;
            }
        };
        let final_context = context::read(self, &initial).await?;
        if !initial.same_source(&final_context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        initial.shared = final_context.shared.clone();
        for candidate in &mut candidates {
            if !candidate.is_pending() {
                continue;
            }
            let record = &frame.records[candidate.record];
            let checked = after.get(&candidate.record).ok_or_else(invalid_data)?;
            candidate.after_check = Some(checked.recheck(record, &candidate.resource, candidate.task_wait.as_ref()));
            let original = before.get(&candidate.record).ok_or_else(invalid_data)?;
            let address = candidate.address.ok_or_else(invalid_data)?;
            let header_before = headers_before.get(&address).ok_or_else(invalid_data)?;
            let header_after = headers_after.get(&address).ok_or_else(invalid_data)?;
            if let Some(point) = &mut candidate.index_operations {
                if let PageIndexOperations::Observed { observed_at, .. } = point
                    && !chronological(&[&header_before.observed_at, observed_at, &header_after.observed_at])
                {
                    return Err(CollectionStop::SourceChanged.error());
                }
                index_context::qualify(point, None, header_before, header_after);
            }
            if !chronological(&[
                &initial.authority.activity.at,
                &frame.context.at,
                &original.request.at,
                &header_before.observed_at,
                &header_after.observed_at,
                &checked.request.at,
                &final_context.authority.activity.at,
            ]) {
                return Err(CollectionStop::SourceChanged.error());
            }
            if [&candidate.before_check, &candidate.after_check]
                .into_iter()
                .all(|check| check.as_ref().is_some_and(|check| check.request == PageWaitRequestState::SameStatement))
                && matches!(record.current_sql, super::NativeStatement::Available(_))
                && (candidate.task_wait.is_none()
                    || [&candidate.before_check, &candidate.after_check].into_iter().all(|check| {
                        check.as_ref().is_some_and(|check| check.wait == PageWaitRelation::OriginalObserved)
                    }))
                && let Some((statement, plan)) = checked.request.identities(&original.request)
            {
                candidate.statement_identity = Some(statement);
                candidate.plan_identity = plan;
            }
            if !address.matches(&header_before.header)? || !address.matches(&header_after.header)? {
                candidate.withhold(PageWaitResolution::Unavailable);
                continue;
            }
            if header_before.catalog.file_guid.is_none() || header_after.catalog.file_guid.is_none() {
                candidate.withhold(PageWaitResolution::UnverifiedSource);
                continue;
            }
            if !header_before.same_mapping(header_after) {
                candidate.withhold(PageWaitResolution::ChangedMetadata);
                continue;
            }
            match mapped_target(&initial, header_after) {
                Ok(target) => {
                    if let Some(point) = &mut candidate.index_operations {
                        index_context::qualify(point, Some(&target), header_before, header_after);
                    }
                    candidate.state = LookupState::Observed { target: Box::new(target) };
                }
                Err(_) => candidate.withhold(PageWaitResolution::UnverifiedSource),
            }
        }
        finish(frame, candidates, limited, initial, final_context.observed_until().to_owned())
    }

    async fn admit_page_headers(&mut self, context: &context::LookupContext) -> Result<()> {
        if context.authority.activity.edition != 5 {
            // No equivalent Azure resource-stat contract exists for this boxed/MI route. Its small, explicit
            // header budget is not a production load qualification or permission to infer spare capacity.
            return Ok(());
        }
        crate::resources::admit_resources(&mut self.io).await.map_err(|error| {
            let error = error.into_error();
            let reason = match CollectionStop::from_error(&error) {
                CollectionStop::Pressure => culprit_core::message!("provider.mssql.page_wait_targets.pressure"),
                CollectionStop::PressureUnavailable => {
                    culprit_core::message!("provider.mssql.page_wait_targets.pressure_unavailable")
                }
                _ => return error,
            };
            error.with_message(reason)
        })
    }

    /// No target is admitted from an unqualified None/None identity. Unresolved native wait evidence may still
    /// be returned to the scan after close, while a collector cannot verify such a source for publication.
    pub(crate) async fn verify_page_wait_context(&mut self, guard: &PageWaitGuard) -> Result<bool> {
        let checked = context::read(self, &guard.0).await?;
        Ok(guard.0.same_source(&checked)
            && chronological(&[&guard.0.authority.activity.at, &checked.authority.activity.at]))
    }

    async fn finish_page_capture(
        &mut self,
        frame: ActivityFrame,
        candidates: Vec<Candidate>,
        limited: bool,
        initial: context::LookupContext,
    ) -> Result<PageWaitCapture> {
        let checked = context::read(self, &initial).await?;
        if !initial.same_observed_source(&checked)
            || !chronological(&[&initial.authority.activity.at, &frame.context.at, &checked.authority.activity.at])
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let until = checked.observed_until().to_owned();
        let mut initial = initial;
        initial.shared = checked.shared;
        finish(frame, candidates, limited, initial, until)
    }

    async fn page_headers(
        &mut self,
        addresses: &BTreeSet<PageAddress>,
        context: &context::LookupContext,
    ) -> Result<BTreeMap<PageAddress, HeaderRecord>> {
        if addresses.is_empty()
            || addresses.len() > MAX_PAGES
            || addresses.iter().any(|address| !context.accepts(*address))
        {
            return Err(invalid_data());
        }
        let shared = addresses.iter().any(|address| context.shared_for(*address).is_some());
        let request_database = addresses.iter().any(|address| context.shared_for(*address).is_none());
        let groups: &[(&str, Option<bool>)] = if shared && request_database {
            &[("request_database", Some(false)), ("shared_tempdb", Some(true))]
        } else {
            &[("", None)]
        };
        let mut xml = String::from("<pages>");
        for (name, shared_group) in groups {
            if !name.is_empty() {
                xml.push_str(&format!("<{name}>"));
            }
            for (ordinal, address) in addresses.iter().enumerate() {
                if shared_group.is_some_and(|shared| shared != context.shared_for(*address).is_some()) {
                    continue;
                }
                xml.push_str(&format!(
                    "<page ordinal=\"{ordinal}\" database=\"{}\" file=\"{}\" page=\"{}\"/>",
                    address.database, address.file, address.page
                ));
            }
            if !name.is_empty() {
                xml.push_str(&format!("</{name}>"));
            }
        }
        xml.push_str("</pages>");
        let sql = match (shared, request_database) {
            (true, true) => sql::headers_with_shared_tempdb(),
            (true, false) => sql::shared_tempdb_headers(),
            _ => sql::headers(),
        };
        let rows =
            self.io.query(&sql, vec![Parameter::Text(xml)], addresses.len()).await.map_err(ReadError::into_error)?;
        if rows.len() != addresses.len() {
            return Err(invalid_data());
        }
        let expected: Vec<_> = addresses.iter().copied().collect();
        let mut headers = BTreeMap::new();
        for row in &rows {
            let header = HeaderRecord::decode(row)?;
            if expected.get(header.ordinal) != Some(&header.address) || headers.insert(header.address, header).is_some()
            {
                return Err(invalid_data());
            }
        }
        Ok(headers)
    }
}

fn select(
    frame: &ActivityFrame,
    tasks: &[Option<MssqlPageTaskWait>],
    context: &context::LookupContext,
) -> (Vec<Candidate>, bool) {
    let mut candidates = Vec::new();
    let mut addresses = BTreeSet::new();
    let mut limited = false;
    for (index, record) in frame.records.iter().enumerate() {
        if record.facts.key.request.is_none() {
            continue;
        }
        let task = tasks.get(index).and_then(Option::as_ref);
        let resources = match task {
            Some(task) => std::slice::from_ref(&task.resource),
            None => record.facts.wait.as_ref().map(|wait| wait.identified_resources.as_slice()).unwrap_or_default(),
        };
        for resource in resources {
            if !matches!(resource.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo) {
                continue;
            }
            if candidates.len() == MAX_PAGE_WAIT_OBSERVATIONS {
                limited = true;
                continue;
            }
            let address = PageAddress::parse(&resource.native.id);
            let state = match address {
                None => LookupState::Unresolved(PageWaitResolution::UnrecognizedAddress),
                Some(address)
                    if !context.accepts(address) || record.facts.scope != ActivityScopeBasis::RequestDatabase =>
                {
                    LookupState::Unresolved(PageWaitResolution::OutsideDatabase)
                }
                Some(address) if !addresses.contains(&address) && addresses.len() == MAX_PAGES => {
                    limited = true;
                    LookupState::Unresolved(PageWaitResolution::Limit)
                }
                Some(address) => {
                    addresses.insert(address);
                    LookupState::Pending
                }
            };
            candidates.push(Candidate {
                record: index,
                resource: resource.clone(),
                address,
                state,
                before_check: None,
                after_check: None,
                statement_identity: None,
                plan_identity: None,
                index_operations: None,
                task_wait: task.cloned(),
            });
        }
    }
    (candidates, limited)
}
fn pending_indices(candidates: &[Candidate]) -> Vec<usize> {
    candidates
        .iter()
        .filter(|candidate| candidate.is_pending())
        .map(|candidate| candidate.record)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn admitted_addresses(candidates: &[Candidate]) -> BTreeSet<PageAddress> {
    candidates.iter().filter(|candidate| candidate.is_pending()).filter_map(|candidate| candidate.address).collect()
}
fn apply_request_checks(
    frame: &ActivityFrame,
    candidates: &mut [Candidate],
    checks: &BTreeMap<usize, participants::CheckedParticipant>,
) {
    for candidate in candidates.iter_mut().filter(|candidate| candidate.is_pending()) {
        candidate.before_check = checks.get(&candidate.record).map(|checked| {
            checked.recheck(&frame.records[candidate.record], &candidate.resource, candidate.task_wait.as_ref())
        });
    }
}
fn mark_pending(candidates: &mut [Candidate], resolution: PageWaitResolution) {
    for candidate in candidates {
        if candidate.is_pending() {
            candidate.withhold(resolution);
        }
    }
}
fn error_resolution(code: ErrorCode) -> PageWaitResolution {
    match code {
        ErrorCode::PermissionDenied => PageWaitResolution::PermissionDenied,
        ErrorCode::Unsupported => PageWaitResolution::Unsupported,
        _ => PageWaitResolution::Unavailable,
    }
}

fn finish(
    frame: ActivityFrame,
    candidates: Vec<Candidate>,
    limited: bool,
    mut guard: context::LookupContext,
    until: String,
) -> Result<PageWaitCapture> {
    let context = guard.capture_context()?;
    let records: BTreeMap<_, _> =
        frame.records.into_iter().enumerate().map(|(index, row)| (index, row.into_redacted_observation())).collect();
    if let Some(shared) = &mut guard.shared {
        for candidate in &candidates {
            if let LookupState::Observed { target } = &candidate.state
                && matches!(target.scope, PageTargetScope::SharedResource { .. })
            {
                shared.retain_file(target)?;
            }
        }
    }
    let observations = candidates
        .into_iter()
        .map(|candidate| {
            let (resolution, target) = match candidate.state {
                LookupState::Pending => (PageWaitResolution::Unavailable, None),
                LookupState::Unresolved(resolution) => (resolution, None),
                LookupState::Observed { target } => (PageWaitResolution::ObservedAtBothBounds, Some(target)),
            };
            let mut request = records.get(&candidate.record).ok_or_else(invalid_data)?.clone();
            if candidate.task_wait.is_some()
                && candidate.statement_identity.is_none()
                && request.statement.is_available()
            {
                request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
            }
            Ok(PageWaitObservation {
                source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
                observed_from: frame.context.at.clone(),
                observed_until: until.clone(),
                request,
                task_wait: candidate.task_wait,
                before_check: candidate.before_check,
                after_check: candidate.after_check,
                resource: candidate.resource,
                observed_address: candidate.address.map(|address| PageWaitAddress {
                    database_id: address.database,
                    file_id: address.file,
                    page_id: address.page,
                }),
                resolution,
                target,
                statement_identity: candidate.statement_identity,
                plan_identity: candidate.plan_identity,
                index_operations: candidate.index_operations,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = PageWaitSnapshot {
        source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        observed_from: context.at.clone(),
        observed_until: until,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(invalid_data());
    }
    Ok(PageWaitCapture { context, snapshot, limited, guard: PageWaitGuard(guard) })
}

fn chronological(instants: &[&str]) -> bool {
    // All values came through canonical_time, so lexical order preserves their exact native UTC instants.
    instants.windows(2).all(|pair| pair[0] <= pair[1])
}

fn mapped_target(context: &context::LookupContext, record: &HeaderRecord) -> Result<MappedPageTarget> {
    let (basis, database) = context.database_identity(record.address)?;
    let guid = record.catalog.file_guid.as_ref().ok_or_else(invalid_data)?;
    if record.catalog.file_state != Some(0) || record.catalog.file_type != Some(0) {
        return Err(invalid_data());
    }
    let header = &record.header;
    let facts = PageHeaderFacts {
        database_id: record.address.database,
        file_id: record.address.file,
        page_id: record.address.page,
        header_version: nonnegative(header.header_version)?,
        page_type: nonnegative(header.page_type)?,
        level: nonnegative(header.level)?,
        object_id: header.object_id.map(|id| id.to_string()),
        index_id: header.index_id.map(|id| id.to_string()),
        partition_id: header.partition_id.clone(),
        allocation_unit_id: header.allocation_unit_id.clone(),
        lsn: header.lsn.clone(),
        next_file_id: nonnegative(header.next_file_id)?,
        next_page_id: nonnegative(header.next_page_id)?,
    };
    let object = object(record);
    let mut file = NativeIdentity::new(guid).qualified("kind", "sql-server-database-file");
    if let Some(created) = &record.catalog.file_create_lsn {
        file = file.qualified("create_lsn", created);
    }
    let scope = match context.shared_for(record.address) {
        Some(shared) => PageTargetScope::SharedResource { context: Box::new(shared.read_context()?) },
        None => PageTargetScope::RequestDatabase,
    };
    let (database, file) = scope.bind_identities(database, file)?;
    let target = MappedPageTarget {
        scope,
        identity_basis: basis,
        database,
        file,
        kind: target_kind(&facts, record.catalog.index_type),
        header: facts,
        object,
    };
    if target.is_valid() { Ok(target) } else { Err(invalid_data()) }
}
fn nonnegative(value: Option<i32>) -> Result<Option<u32>> {
    value.map(|value| u32::try_from(value).map_err(|_| invalid_data())).transpose()
}
fn object(record: &HeaderRecord) -> Option<PageWaitObject> {
    let catalog = &record.catalog;
    let header = &record.header;
    let id = catalog.object_id?;
    if header.object_id != Some(id)
        || header.index_id != catalog.index_id
        || header.partition_id != catalog.partition_id
    {
        return None;
    }
    let native = crate::index_operations::targeted::object_identity(
        id.try_into().ok()?,
        catalog.index_id.map(u32::try_from).transpose().ok()?,
        catalog.index_type.map(i64::from),
        catalog.object_created.as_deref(),
        catalog.object_modified.as_deref(),
        catalog.hobt_id.as_deref(),
    )?;
    Some(PageWaitObject {
        native,
        schema: catalog.schema_name.clone()?,
        name: catalog.object_name.clone()?,
        index_name: catalog.index_name.clone(),
    })
}
fn target_kind(header: &PageHeaderFacts, index_type: Option<i32>) -> PageWaitTargetKind {
    // Native format meanings, not an elapsed/cause verdict. Allocation maps retain their separate mechanisms;
    // neither an observed allocation page nor a rowstore leaf identifies an allocating statement or latch holder.
    // Rowstore leaf/branch labels also require sys.indexes' native clustered/nonclustered rowstore type, rather
    // than interpreting a columnstore or invisible index's page layout from its numeric index ID alone.
    // https://ptgmedia.pearsoncmg.com/images/9780735658561/samplepages/9780735658561.pdf
    // https://techcommunity.microsoft.com/blog/sqlserver/how-to-use-dbcc-page/383094
    // https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-indexes-transact-sql
    match classify_sql_server_page_header(header) {
        PageWaitTargetKind::IndexLeaf | PageWaitTargetKind::IndexBranch if !matches!(index_type, Some(1 | 2)) => {
            PageWaitTargetKind::Unclassified
        }
        kind => kind,
    }
}
