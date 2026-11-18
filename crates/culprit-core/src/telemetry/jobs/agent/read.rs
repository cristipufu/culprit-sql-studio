use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlAgentJobsSnapshot {
    pub context: NativeReadContext,
    pub metadata: MssqlAgentJobsMetadata,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<OperationalObservation>,
}
impl MssqlAgentJobsSnapshot {
    pub fn from_source(source: &CollectionDescription, observations: Vec<OperationalObservation>) -> Result<Self> {
        let SourceObservation::MssqlAgentJobs { snapshot, metadata } = &source.observation else {
            return Err(invalid());
        };
        if !metadata.matches_source(source, snapshot) {
            return Err(invalid());
        }
        let value = Self {
            context: NativeReadContext::from_snapshot(source)?,
            metadata: metadata.as_ref().clone(),
            sources: snapshot.sources.clone(),
            observations,
        };
        value.is_valid_read(false).then_some(value).ok_or_else(invalid)
    }
    fn context_coverage_matches_known_points(&self) -> bool {
        let complete =
            self.sources.iter().any(|source| source.source == MSSQL_AGENT_CONTEXT_SOURCE && source.complete_scope);
        if !complete {
            return true;
        }
        let points = self
            .observations
            .iter()
            .filter_map(|row| match &row.fact {
                OperationalFact::MssqlAgentContext(point) => Some(point.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        points.iter().all(|point| point.session.is_some() && !point.services.is_empty() && !point.services_limited)
            && points.windows(2).all(|pair| pair[0].session == pair[1].session)
    }
    fn activity_session_matches_original_context(&self) -> bool {
        let before = self.observations.iter().find_map(|row| match &row.fact {
            OperationalFact::MssqlAgentContext(point) if point.bound == MssqlAgentContextBound::Before => {
                point.session.as_ref()
            }
            _ => None,
        });
        let Some(before) = before else {
            return true;
        };
        self.observations.iter().all(|row| match &row.fact {
            OperationalFact::MssqlAgentJob(point) => {
                point.activity.as_ref().is_none_or(|activity| activity.agent_session_id == before.session_id)
            }
            _ => true,
        })
    }
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let count = self.sources.iter().try_fold(0u32, |sum, source| sum.checked_add(source.records));
        let Some(count) = count else {
            return false;
        };
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::Jobs,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: count,
            sources: self.sources.clone(),
        };
        let source = CollectionDescription {
            target: self.context.target.clone(),
            partition: self.context.partition.clone(),
            source: MSSQL_AGENT_JOBS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::MssqlAgentJobs {
                snapshot: snapshot.clone(),
                metadata: Box::new(self.metadata.clone()),
            },
        };
        if self.observations.len() > MAX_MSSQL_AGENT_JOB_OBSERVATIONS
            || !self.context.is_valid()
            || self.context.database_writable.is_some()
            || !self.metadata.matches_source(&source, &snapshot)
        {
            return false;
        }
        let mut keys = BTreeSet::new();
        let mut counts = BTreeMap::<&str, u32>::new();
        for row in &self.observations {
            if !self.metadata.accepts(&snapshot, row) || !keys.insert((&row.source, &row.key)) {
                return false;
            }
            *counts.entry(&row.source).or_default() += 1;
        }
        if !self.activity_session_matches_original_context() || !self.context_coverage_matches_known_points() {
            return false;
        }
        self.sources.iter().all(|source| {
            let observed = counts.get(source.source.as_str()).copied().unwrap_or(0);
            observed <= source.records
                && (!read_complete || observed == source.records)
                && (observed == 0
                    || matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial))
        })
    }
}
