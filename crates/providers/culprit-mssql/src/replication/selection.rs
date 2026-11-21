//! Source families retain independent native availability on the same bounded owner.
use super::*;
use culprit_core::{Error, ErrorCode, telemetry::*};

const AVAILABILITY_SOURCE: &str = "availability-group-state";
const GEO_SOURCE: &str = "geo-replication-links";

pub(crate) struct SelectedRead {
    pub parts: Vec<Part>,
}

pub(crate) enum Part {
    Observed { selection: Selection, facts: Vec<ReplicationFact>, complete: bool, limited: bool },
    Unavailable { selection: Selection, error: Error },
    NotApplicable { selection: Selection },
}

impl Part {
    pub(super) fn observed(selection: Selection, facts: Vec<ReplicationFact>) -> Self {
        Self::Observed { selection, complete: scope_complete(selection, &facts), facts, limited: false }
    }
}

impl Selection {
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::AvailabilityGroup => AVAILABILITY_SOURCE,
            Self::GeoLinks => GEO_SOURCE,
            Self::Hyperscale => MSSQL_HYPERSCALE_REPLICAS_SOURCE,
        }
    }
}

impl SelectedRead {
    pub(crate) fn remaining(&self, limit: usize) -> usize {
        limit.saturating_sub(
            self.parts
                .iter()
                .map(|part| match part {
                    Part::Observed { facts, .. } => facts.len(),
                    _ => 0,
                })
                .sum::<usize>(),
        )
    }

    pub(crate) fn into_records(
        self,
        qualified: bool,
        qualification_error: Option<ErrorCode>,
    ) -> (Vec<TelemetryRecord>, Vec<StateSourceCoverage>) {
        let (observations, sources) = self.into_observations(ReadAuthority::Native { qualified, qualification_error });
        (observations.into_iter().map(TelemetryRecord::Replication).collect(), sources)
    }

    /// A confirmed scan retains query visibility; its separate Collector context supplies no native member proof.
    pub(super) fn into_scan_observations(self) -> (Vec<ReplicationObservation>, Vec<StateSourceCoverage>) {
        self.into_observations(ReadAuthority::Collector)
    }

    fn into_observations(self, authority: ReadAuthority) -> (Vec<ReplicationObservation>, Vec<StateSourceCoverage>) {
        let mut records = Vec::new();
        let mut sources = Vec::new();
        for part in self.parts {
            match part {
                Part::Observed { selection, facts, complete, limited } => {
                    let source = selection.source();
                    let (qualified, qualification_error) = match authority {
                        ReadAuthority::Collector => (true, None),
                        ReadAuthority::Native { qualified, qualification_error } => (qualified, qualification_error),
                    };
                    let mut coverage =
                        StateSourceCoverage::observed(source.into(), facts.len() as u32, complete && qualified);
                    if limited {
                        coverage.stopped = Some(CollectionStop::RowLimit);
                    }
                    if !qualified {
                        coverage.error_code = qualification_error;
                    }
                    sources.push(coverage);
                    records
                        .extend(facts.into_iter().map(|fact| ReplicationObservation { source: source.into(), fact }));
                }
                Part::NotApplicable { selection } => {
                    sources.push(StateSourceCoverage::not_applicable(selection.source().into()));
                }
                Part::Unavailable { selection, error } => {
                    sources.push(StateSourceCoverage::unavailable(selection.source().into(), error.code, None))
                }
            }
        }
        (records, sources)
    }
}

#[derive(Clone, Copy)]
enum ReadAuthority {
    Native { qualified: bool, qualification_error: Option<ErrorCode> },
    Collector,
}

pub(crate) async fn read_selected<R: NativeRead>(reader: &mut R, edition: i64, limit: usize) -> Result<SelectedRead> {
    let mut parts = Vec::new();
    let mut remaining = limit.min(MAX_REPLICAS);
    for selection in selections(edition)? {
        if remaining == 0 {
            return Err(CollectionStop::RowLimit.error());
        }
        let part = read(reader, edition, *selection, remaining).await?;
        if let Part::Observed { facts, .. } = &part {
            remaining -= facts.len();
        }
        parts.push(part);
    }
    Ok(SelectedRead { parts })
}

fn selections(edition: i64) -> Result<&'static [Selection]> {
    match edition {
        crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE => Ok(&[Selection::GeoLinks]),
        crate::probe::ENGINE_EDITION_MANAGED_INSTANCE => Ok(&[Selection::AvailabilityGroup, Selection::GeoLinks]),
        2..=4 => Ok(&[Selection::AvailabilityGroup]),
        _ => Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    }
}

fn scope_complete(selection: Selection, facts: &[ReplicationFact]) -> bool {
    if facts.is_empty() {
        return selection != Selection::Hyperscale;
    }
    match selection {
        Selection::Hyperscale => false,
        Selection::GeoLinks => facts.iter().all(direction_known),
        Selection::AvailabilityGroup => {
            // Secondary-side visibility is local, not a complete partner inventory. Even a primary cannot
            // establish catalog completeness where replica names/modes or local role are unavailable.
            facts.iter().any(|fact| {
                matches!(fact, ReplicationFact::AvailabilityGroup(row)
                    if row.is_local == Some(true) && row.is_primary == Some(true))
            }) && facts.iter().all(|fact| {
                matches!(fact, ReplicationFact::AvailabilityGroup(row)
                    if row.is_local.is_some() && row.is_primary.is_some()
                        && row.native_name.is_some() && row.mode.is_some() && row.state.is_some()
                        && row.group.as_ref().is_some_and(|context| context.catalog_observed && context.state_observed)
                        && row.replica.as_ref().is_some_and(|context| context.catalog_observed && context.state_observed))
            })
        }
    }
}

#[cfg(test)]
mod tests;
