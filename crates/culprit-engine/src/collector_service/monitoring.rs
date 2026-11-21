//! Standard policy resolves the selected native declarations into the existing fleet, without source work.
use super::*;
use culprit_collector::fleet::{FleetCaptureOutcome, FleetSourceAdmission, minimum_collection_slot};
use std::collections::{BTreeMap, BTreeSet};

const STANDARD_PERIOD_SECONDS: u32 = 300;
const MAX_STANDARD_PERIOD_SECONDS: u32 = 86_400;
const HISTORY_WINDOW_SECONDS: u32 = 3_600;
const CAPACITY_UNITS: u64 = 1_000_000;
const MAX_VISIBLE_GAPS: usize = 4_096;

type ProfileKey = (String, String, String);

pub(super) struct Enrollment {
    pub inventory: Option<FleetInventory>,
    pub summary: MonitoringEnrollmentSummary,
    profiles: BTreeMap<ProfileKey, SourceProfileAdmission>,
}
struct Candidate {
    target: String,
    schedule: FleetSchedule,
    profile: SourceProfileAdmission,
}
impl Enrollment {
    pub fn resolve(workspace: &Workspace, config: &CollectorConfig) -> Result<Self> {
        config.validate()?;
        let assigned = config.inventory.targets.iter().filter(|target| target.shard == config.shard);
        let mut result = Self {
            inventory: None,
            summary: MonitoringEnrollmentSummary {
                assigned_targets: assigned.clone().count() as u32,
                explicit_schedules: assigned.map(|target| target.schedules.len() as u32).sum(),
                automatic: vec![],
                gaps: vec![],
                omitted_gaps: 0,
            },
            profiles: BTreeMap::new(),
        };
        let budget = match &config.monitoring {
            MonitoringPolicy::Explicit => {
                result.inventory = Some(config.inventory.clone());
                return Ok(result);
            }
            MonitoringPolicy::Standard { budget } => budget,
        };
        let mut targets =
            config.inventory.targets.iter().filter(|target| target.shard == config.shard).cloned().collect::<Vec<_>>();
        targets.sort_by(|a, b| a.id.cmp(&b.id));
        let mut candidates = Vec::new();
        for target in &targets {
            result.select(workspace, target, budget, &mut candidates)?;
        }
        result.schedule(&mut targets, candidates)?;
        targets.retain(|target| !target.schedules.is_empty());
        if !targets.is_empty() {
            let inventory = FleetInventory { targets };
            inventory.validate(&config.shard)?;
            result.inventory = Some(inventory);
        }
        Ok(result)
    }

    fn gap(&mut self, target: &str, recipe: Option<&str>, reason: MonitoringGapReason) {
        if self.summary.gaps.len() < MAX_VISIBLE_GAPS {
            self.summary.gaps.push(MonitoringGap {
                target: target.into(),
                recipe_id: recipe.map(str::to_owned),
                reason,
            });
        } else {
            self.summary.omitted_gaps += 1;
        }
    }

    fn select(
        &mut self,
        workspace: &Workspace,
        target: &FleetTarget,
        budget: &CollectionBudget,
        candidates: &mut Vec<Candidate>,
    ) -> Result<()> {
        let connections = workspace.connection_service();
        let recipes = workspace.telemetry_recipes(&target.connection)?;
        let eligible = recipes
            .iter()
            .filter(|recipe| {
                recipe.purpose == RecipePurpose::Baseline
                    && recipe.subjects.iter().any(|subject| subject.accepts(&target.subject))
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            self.gap(&target.id, None, MonitoringGapReason::Undeclared);
            return Ok(());
        }
        let route = connections.native_route_revision(&target.connection)?;
        let installed = workspace.qualification_bindings.receipts(&target.connection, &route)?;
        let registration = connections.selected_registration(workspace.store.get(&target.connection)?.engine)?;
        for recipe in eligible {
            if target
                .schedules
                .iter()
                .any(|schedule| recipe.selector == RecipeSelector::Collection(schedule.collection.collection()))
            {
                continue;
            }
            let RecipeCostQualification::Measured(receipt) = &recipe.cost else {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::Unqualified);
                continue;
            };
            if !installed.contains(receipt.as_ref())
                || registration.qualification_component() != Some(&receipt.component)
                || receipt.scope.subject() != target.subject
            {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::MissingApproval);
                continue;
            }
            if !registration.supports_profile_source(recipe.selector) {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::AdmissionUnsupported);
                continue;
            }
            let Some(collection) = self.collection(target, recipe) else {
                continue;
            };
            let budget = budget.intersection(&recipe.budget).intersection(&receipt.tested_budget);
            if u64::from(budget.page_rows) * u64::from(budget.max_pages)
                > culprit_collector::delivery::contract::MAX_RANGE_RECORDS as u64
            {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::Capacity);
                continue;
            }
            let Some(binding) =
                workspace.qualification_bindings.profile_binding(&target.connection, &route, receipt)?
            else {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::MissingApproval);
                continue;
            };
            let profile = SourceProfileAdmission::new(
                connections.route_revision(&target.connection)?,
                receipt.as_ref().clone(),
                budget.clone(),
                move || binding.is_active(),
            )?;
            let schedule = FleetSchedule {
                id: recipe.id.clone(),
                every_seconds: STANDARD_PERIOD_SECONDS,
                jitter_seconds: 0,
                collection,
                budget,
            };
            candidates.push(Candidate { target: target.id.clone(), schedule, profile });
        }
        Ok(())
    }

    fn collection(&mut self, target: &FleetTarget, recipe: &RecipeDescriptor) -> Option<ScheduledCollection> {
        let collection = match recipe.selector {
            RecipeSelector::Collection(TelemetryCollection::Activity) => {
                ScheduledCollection::Activity { policy: ActivityPolicy::default() }
            }
            RecipeSelector::Collection(TelemetryCollection::Snapshot(dataset)) => {
                let selection = CollectionSelection::Snapshot { dataset };
                let request =
                    CollectionRequest { selection, subject: target.subject.clone(), budget: recipe.budget.clone() };
                if request.validate().is_err() {
                    self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::RequiresSelectedTarget);
                    return None;
                }
                ScheduledCollection::Snapshot { dataset }
            }
            RecipeSelector::Collection(TelemetryCollection::ResourceHistory) => {
                ScheduledCollection::ResourceHistory { window_seconds: HISTORY_WINDOW_SECONDS, overlap_seconds: 0 }
            }
            RecipeSelector::Collection(TelemetryCollection::DeadlockHistory) => {
                ScheduledCollection::DeadlockHistory { window_seconds: HISTORY_WINDOW_SECONDS, overlap_seconds: 0 }
            }
            RecipeSelector::Collection(TelemetryCollection::QueryHistory) => {
                // This scheduler dispatches interval discovery separately. A source-only approval cannot pay it.
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::AdditionalWorkUnqualified);
                return None;
            }
            _ => {
                self.gap(&target.id, Some(&recipe.id), MonitoringGapReason::RequiresSelectedTarget);
                return None;
            }
        };
        Some(collection)
    }

    fn schedule(&mut self, targets: &mut [FleetTarget], candidates: Vec<Candidate>) -> Result<()> {
        // The workspace also serializes different resource groups. Reserve the whole profile, not only each group.
        let explicit = targets.iter().flat_map(|target| &target.schedules).collect::<Vec<_>>();
        let workspace_period = feasible_period(&explicit, &candidates)?;
        let mut groups: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
        let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for candidate in &candidates {
            let target = targets.iter().find(|target| target.id == candidate.target).ok_or_else(invalid)?;
            owners.entry(native_resource_key(&candidate.profile)?).or_default().insert(target.admission_group.clone());
        }
        for candidate in candidates {
            let target = targets.iter().find(|target| target.id == candidate.target).ok_or_else(invalid)?;
            if owners.get(&native_resource_key(&candidate.profile)?).is_some_and(|groups| groups.len() != 1) {
                self.gap(&candidate.target, Some(&candidate.schedule.id), MonitoringGapReason::SharedAdmissionConflict);
                continue;
            }
            groups.entry(target.admission_group.clone()).or_default().push(candidate);
        }
        let mut shared = BTreeSet::new();
        for (group, candidates) in groups {
            let explicit = targets
                .iter()
                .filter(|target| target.admission_group == group)
                .flat_map(|target| &target.schedules)
                .collect::<Vec<_>>();
            let period = match (workspace_period, feasible_period(&explicit, &candidates)?) {
                (Some(workspace), Some(resource)) => Some(workspace.max(resource)),
                _ => None,
            };
            for mut candidate in candidates {
                let Some(period) = period else {
                    self.gap(&candidate.target, Some(&candidate.schedule.id), MonitoringGapReason::InfeasibleCadence);
                    continue;
                };
                let target = targets.iter_mut().find(|target| target.id == candidate.target).ok_or_else(invalid)?;
                if target.schedules.len() >= MAX_TARGET_SCHEDULES {
                    self.gap(&candidate.target, Some(&candidate.schedule.id), MonitoringGapReason::ScheduleCapacity);
                    continue;
                }
                if target.schedules.iter().any(|schedule| schedule.id == candidate.schedule.id) {
                    self.gap(&candidate.target, Some(&candidate.schedule.id), MonitoringGapReason::ConflictingSchedule);
                    continue;
                }
                let receipt = candidate.profile.qualification();
                let scope_key = digest(
                    &serde_json::to_vec(&(receipt.scope.target(), receipt.scope.partition(), receipt.selector))
                        .map_err(|_| invalid())?,
                );
                if matches!(target.subject, CollectionSubject::Shared { .. }) && !shared.insert(scope_key) {
                    self.gap(
                        &candidate.target,
                        Some(&candidate.schedule.id),
                        MonitoringGapReason::SharedOwnerAlreadyScheduled,
                    );
                    continue;
                }
                candidate.schedule.every_seconds = period;
                if let ScheduledCollection::ResourceHistory { window_seconds, .. }
                | ScheduledCollection::DeadlockHistory { window_seconds, .. } = &mut candidate.schedule.collection
                {
                    *window_seconds = (*window_seconds).max(period);
                }
                candidate.schedule.validate()?;
                let key = profile_key(&target.connection, &target.subject, candidate.schedule.collection.collection())?;
                if self.profiles.insert(key, candidate.profile).is_some() {
                    return Err(invalid());
                }
                self.summary.automatic.push(MonitoringScheduleSummary {
                    target: target.id.clone(),
                    recipe_id: candidate.schedule.id.clone(),
                    every_seconds: period,
                });
                target.schedules.push(candidate.schedule);
            }
        }
        Ok(())
    }

    pub fn backend<'a>(&'a self, workspace: &'a Workspace) -> EnrolledBackend<'a> {
        EnrolledBackend { workspace, profiles: &self.profiles }
    }
}

fn native_resource_key(profile: &SourceProfileAdmission) -> Result<String> {
    let SourcePartition::InstanceLocal { member: Some(member), .. } = profile.qualification().scope.partition() else {
        return Err(invalid());
    };
    serde_json::to_vec(member).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}
fn source_slot(budget: &CollectionBudget) -> u64 {
    minimum_collection_slot(budget).as_millis().div_ceil(1_000) as u64
}
fn feasible_period(explicit: &[&FleetSchedule], candidates: &[Candidate]) -> Result<Option<u32>> {
    // Current/backfill history can repeatedly request More. Do not promise baseline cadence beside that work.
    if explicit.iter().any(|schedule| matches!(schedule.collection, ScheduledCollection::QueryHistory { .. })) {
        return Ok(None);
    }
    let mut used = 0u64;
    for schedule in explicit {
        used = used
            .checked_add((source_slot(&schedule.budget) * CAPACITY_UNITS).div_ceil(u64::from(schedule.every_seconds)))
            .ok_or_else(invalid)?;
    }
    let Some(available) = CAPACITY_UNITS.checked_sub(used).filter(|value| *value > 0) else {
        return Ok(None);
    };
    let seconds = candidates.iter().try_fold(0u64, |sum, candidate| {
        sum.checked_add(source_slot(&candidate.schedule.budget)).ok_or_else(invalid)
    })?;
    let period = (seconds * CAPACITY_UNITS).div_ceil(available).max(u64::from(STANDARD_PERIOD_SECONDS));
    Ok((period <= u64::from(MAX_STANDARD_PERIOD_SECONDS)).then_some(period as u32))
}
fn profile_key(connection: &str, subject: &CollectionSubject, collection: TelemetryCollection) -> Result<ProfileKey> {
    Ok((
        connection.into(),
        subject.assignment_key(connection)?,
        serde_json::to_string(&collection).map_err(|_| invalid())?,
    ))
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}

pub(super) struct EnrolledBackend<'a> {
    workspace: &'a Workspace,
    profiles: &'a BTreeMap<ProfileKey, SourceProfileAdmission>,
}
#[async_trait::async_trait]
impl FleetBackend for EnrolledBackend<'_> {
    fn validate_target(&self, connection: &str) -> Result<()> {
        self.workspace.validate_target(connection)
    }
    fn telemetry_recipes(&self, connection: &str) -> Result<Vec<RecipeDescriptor>> {
        self.workspace.telemetry_recipes(connection)
    }
    fn route_revision(&self, connection: &str) -> Result<Option<String>> {
        FleetBackend::route_revision(self.workspace, connection)
    }
    fn qualification_component(&self, connection: &str) -> Result<Option<culprit_core::ComponentVersion>> {
        FleetBackend::qualification_component(self.workspace, connection)
    }
    fn verify_cost(
        &self,
        connection: &str,
        receipt: &RecipeCostReceipt,
        source: &CollectionDescription,
        row: &ExportedRecord,
    ) -> Result<bool> {
        self.workspace.verify_telemetry_cost(connection, receipt, source, row)
    }
    async fn discover(
        &self,
        connection: &str,
        request: &IntervalDiscoveryRequest,
        admission: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage> {
        FleetBackend::discover(self.workspace, connection, request, admission).await
    }
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        mut admission: FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        let profile =
            profile_key(connection, &request.subject, request.collection()).map(|key| self.profiles.get(&key));
        match profile {
            Err(error) => {
                return FleetCaptureOutcome {
                    result: Err(error),
                    source_work: SourceWorkState::NotStarted,
                    attempt: None,
                };
            }
            Ok(Some(profile)) if admission.investigation.is_none() && profile.matches_request(&request) => {
                if admission.route_revision.as_deref() != Some(profile.route_revision()) {
                    return FleetCaptureOutcome {
                        result: Err(CollectionStop::SourceChanged.error()),
                        source_work: SourceWorkState::NotStarted,
                        attempt: None,
                    };
                }
                admission.profile = Some(profile.clone());
            }
            Ok(Some(_)) if admission.investigation.is_none() => {
                return FleetCaptureOutcome {
                    result: Err(CollectionStop::InvalidPage.error()),
                    source_work: SourceWorkState::NotStarted,
                    attempt: None,
                };
            }
            _ => (),
        }
        FleetBackend::capture(self.workspace, connection, request, outbox, cancellation, admission).await
    }
}

#[cfg(all(test, feature = "store-adx", feature = "recovery-azure"))]
mod tests;
