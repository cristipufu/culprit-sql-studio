use crate::query_store::{decode::malformed, replicas::Population};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, TelemetryStream},
};
use serde::{Deserialize, Serialize};

const PLAN_RANGE_WIDTH: i64 = 1024;
use culprit_core::telemetry::MAX_CURSOR_BYTES;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct PlanRange {
    pub lower: i64,
    pub upper: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PopulationRange {
    Catalog,
    AllGroups,
    Unpartitioned,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct Cursor {
    stream: TelemetryStream,
    interval: String,
    population: PopulationRange,
    replica_index: usize,
    next_plan: i64,
    pending: Vec<PlanRange>,
}

impl Cursor {
    pub(super) fn new(stream: TelemetryStream, interval: String, population: PopulationRange) -> Self {
        Self { stream, interval, population, replica_index: 0, next_plan: 0, pending: vec![] }
    }
    pub(super) fn parse(text: &str, stream: TelemetryStream, interval: &str) -> Result<Self> {
        if text.len() > MAX_CURSOR_BYTES {
            return Err(malformed());
        }
        let cursor: Self = serde_json::from_str(text).map_err(|_| malformed())?;
        if cursor.stream != stream || cursor.interval != interval || cursor.pending.len() > 64 {
            return Err(malformed());
        }
        Ok(cursor)
    }
    pub(super) fn next_range(
        &mut self,
        replicas: &[String],
        upper: i64,
        population: PopulationRange,
    ) -> Result<Option<(Population, PlanRange)>> {
        if self.population != population || self.next_plan < 0 || self.next_plan > upper {
            return Err(malformed());
        }
        let populations = match population {
            PopulationRange::Catalog if !replicas.is_empty() => replicas.len(),
            PopulationRange::AllGroups | PopulationRange::Unpartitioned if replicas.is_empty() => 1,
            _ => return Err(malformed()),
        };
        if self.replica_index > populations {
            return Err(malformed());
        }
        while self.replica_index < populations {
            let selected = match population {
                PopulationRange::Catalog => {
                    Population::ReplicaGroup(replicas[self.replica_index].parse().map_err(|_| malformed())?)
                }
                PopulationRange::AllGroups => Population::AllGroups,
                PopulationRange::Unpartitioned => Population::Unpartitioned,
            };
            if let Some(range) = self.pending.pop() {
                if range.lower < 0 || range.lower >= range.upper || range.upper > upper {
                    return Err(malformed());
                }
                return Ok(Some((selected, range)));
            }
            if self.next_plan < upper {
                let range = PlanRange {
                    lower: self.next_plan,
                    upper: self.next_plan.saturating_add(PLAN_RANGE_WIDTH).min(upper),
                };
                self.next_plan = range.upper;
                return Ok(Some((selected, range)));
            }
            self.replica_index += 1;
            self.next_plan = 0;
        }
        Ok(None)
    }
    pub(super) fn split(&mut self, range: PlanRange) -> std::result::Result<(), CollectionStop> {
        if range.upper - range.lower <= 1 {
            return Err(CollectionStop::RowLimit);
        }
        let middle = range.lower + (range.upper - range.lower) / 2;
        self.pending.push(PlanRange { lower: middle, upper: range.upper });
        self.pending.push(PlanRange { lower: range.lower, upper: middle });
        Ok(())
    }
    pub(super) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|_| malformed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflow_partitions_the_whole_domain_without_advancing_past_a_plan() {
        let mut cursor = Cursor::new(TelemetryStream::QueryRuntime, "7".into(), PopulationRange::Catalog);
        let replicas = vec!["1".into(), "2".into()];
        let (_, range) = cursor.next_range(&replicas, 3, PopulationRange::Catalog).unwrap().unwrap();
        cursor.split(range).unwrap();
        let mut ranges = vec![];
        while let Some((replica, range)) = cursor.next_range(&replicas, 3, PopulationRange::Catalog).unwrap() {
            let Population::ReplicaGroup(replica) = replica else { panic!() };
            ranges.push((replica, range.lower, range.upper));
        }
        assert_eq!(ranges, vec![(1, 0, 1), (1, 1, 3), (2, 0, 3)]);
        assert_eq!(cursor.split(PlanRange { lower: 5, upper: 6 }), Err(CollectionStop::RowLimit));
    }
    #[test]
    fn cursors_cannot_cross_streams_intervals_or_allocate_the_plan_domain() {
        let cursor = Cursor::new(TelemetryStream::QueryRuntime, "7".into(), PopulationRange::Catalog).encode().unwrap();
        assert!(Cursor::parse(&cursor, TelemetryStream::QueryWaits, "7").is_err());
        assert!(Cursor::parse(&cursor, TelemetryStream::QueryRuntime, "8").is_err());
        let mut cursor = Cursor::parse(&cursor, TelemetryStream::QueryRuntime, "7").unwrap();
        assert_eq!(
            cursor.next_range(&["1".into()], i64::MAX, PopulationRange::Catalog).unwrap().unwrap().1.upper,
            PLAN_RANGE_WIDTH
        );
    }
    #[test]
    fn unknown_and_native_unpartitioned_domains_never_synthesize_a_replica_identity() {
        for (mode, expected) in [
            (PopulationRange::AllGroups, Population::AllGroups),
            (PopulationRange::Unpartitioned, Population::Unpartitioned),
        ] {
            let mut cursor = Cursor::new(TelemetryStream::QueryRuntime, "7".into(), mode);
            let (population, range) = cursor.next_range(&[], 3, mode).unwrap().unwrap();
            assert_eq!(population, expected);
            cursor.split(range).unwrap();
            let token = cursor.encode().unwrap();
            let mut restored = Cursor::parse(&token, TelemetryStream::QueryRuntime, "7").unwrap();
            let mut ranges = vec![];
            while let Some((population, range)) = restored.next_range(&[], 3, mode).unwrap() {
                assert_eq!(population, expected);
                ranges.push((range.lower, range.upper));
            }
            assert_eq!(ranges, [(0, 1), (1, 3)]);
            assert!(
                Cursor::parse(&token, TelemetryStream::QueryRuntime, "7")
                    .unwrap()
                    .next_range(&["1".into()], 3, mode)
                    .is_err()
            );
            assert!(
                Cursor::parse(&token, TelemetryStream::QueryRuntime, "7")
                    .unwrap()
                    .next_range(&[], 3, PopulationRange::Catalog)
                    .is_err()
            );
        }
    }
}
