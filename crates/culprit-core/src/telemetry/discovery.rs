//! Discover native closed intervals before scheduling history captures. Metadata availability is not data continuity.
use super::*;

pub const MAX_INTERVAL_DISCOVERY_ROWS: u32 = 128;
pub const MAX_INTERVAL_DISCOVERY_SECONDS: u32 = 7 * 86_400;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct IntervalDiscoveryRequest {
    pub from: String,
    pub until: String,
    pub cursor: Option<String>,
    pub limit: u32,
    pub budget: CollectionBudget,
}
impl IntervalDiscoveryRequest {
    pub fn bounds(&self) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        bounded_history_bounds(&self.from, &self.until, MAX_INTERVAL_DISCOVERY_SECONDS, || {
            Error::invalid(t!("collection.invalid_discovery_interval"))
        })
    }
    pub fn validate(&self) -> Result<()> {
        self.bounds()?;
        self.budget.validate()?;
        if self.limit == 0
            || self.limit > MAX_INTERVAL_DISCOVERY_ROWS
            || self.limit > self.budget.page_rows
            || self.cursor.as_ref().is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES)
        {
            return Err(Error::invalid(t!("collection.invalid_budget")));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct IntervalDiscoveryPage {
    pub database: DatabaseIdentity,
    pub source: String,
    /// Fixed at the first page; intervals closing during pagination belong to a subsequent discovery.
    pub observed_at: String,
    /// Earliest native interval metadata still present. It does not establish that runtime or waits were retained.
    pub metadata_from: Option<String>,
    pub intervals: Vec<TelemetryInterval>,
    pub next_cursor: Option<String>,
}
impl IntervalDiscoveryPage {
    pub fn matches_request(&self, request: &IntervalDiscoveryRequest) -> bool {
        if request.validate().is_err() || self.database.database.is_empty() {
            return false;
        }
        let Ok((from, until)) = request.bounds() else { return false };
        let Ok(observed) = DateTime::parse_from_rfc3339(&self.observed_at) else { return false };
        let mut ids = std::collections::BTreeSet::new();
        let mut preceding = None;
        valid_key(&self.source)
            && self.observed_at.len() <= 64
            && self.intervals.len() <= request.limit as usize
            && self.metadata_from.as_deref().is_none_or(|at| canonical_time(at).is_ok())
            && self.next_cursor.as_ref().is_none_or(|cursor| {
                !cursor.is_empty() && cursor.len() <= MAX_CURSOR_BYTES && request.cursor.as_ref() != Some(cursor)
            })
            && (self.next_cursor.is_none() || self.intervals.len() == request.limit as usize)
            && self.intervals.iter().all(|interval| {
                let times = DateTime::parse_from_rfc3339(&interval.from)
                    .ok()
                    .zip(DateTime::parse_from_rfc3339(&interval.until).ok());
                let Some((start, end)) = times else { return false };
                let ordered = preceding.is_none_or(|previous| previous <= (end, start));
                preceding = Some((end, start));
                valid_key(&interval.native_id)
                    && ids.insert(&interval.native_id)
                    && interval.from.len() <= 64
                    && interval.until.len() <= 64
                    && start < end
                    && end > from
                    && start < until
                    && end <= until
                    && end <= observed
                    && ordered
            })
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= request.budget.page_bytes as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_discovery_can_span_a_week_without_weakening_the_capture_window_limit() {
        let mut discovery = request();
        discovery.from = "2026-11-03T08:00:00Z".into();
        discovery.validate().unwrap();
        let capture = CollectionRequest {
            selection: CollectionSelection::QueryHistory {
                from: discovery.from.clone(),
                until: discovery.until.clone(),
            },
            budget: discovery.budget.clone(),
        };
        assert!(capture.validate().is_err());
        discovery.from = "2026-11-03T07:59:59Z".into();
        assert!(discovery.validate().is_err());
    }
    fn request() -> IntervalDiscoveryRequest {
        IntervalDiscoveryRequest {
            from: "2026-11-10T06:30:00Z".into(),
            until: "2026-11-10T08:00:00Z".into(),
            cursor: None,
            limit: 1,
            budget: CollectionBudget::default(),
        }
    }
    fn page() -> IntervalDiscoveryPage {
        IntervalDiscoveryPage {
            database: DatabaseIdentity::server(crate::Engine::Mssql, "db", "fixture.invalid", 1433, None),
            source: crate::source_id::QUERY_STORE.into(),
            observed_at: "2026-11-10T08:01:00Z".into(),
            metadata_from: Some("2026-11-01T00:00:00Z".into()),
            intervals: vec![TelemetryInterval {
                native_id: "17".into(),
                from: "2026-11-10T06:00:00Z".into(),
                until: "2026-11-10T07:00:00Z".into(),
            }],
            next_cursor: None,
        }
    }
    #[test]
    fn an_overlapping_closed_native_interval_keeps_its_real_boundaries() {
        let request = request();
        assert!(request.validate().is_ok());
        assert!(page().matches_request(&request));
    }
    #[test]
    fn invalid_bounds_limits_and_cursors_are_rejected_before_reading() {
        let mut request = request();
        request.limit = 0;
        assert!(request.validate().is_err());
        request.limit = MAX_INTERVAL_DISCOVERY_ROWS + 1;
        assert!(request.validate().is_err());
        request.limit = 1;
        request.cursor = Some(String::new());
        assert!(request.validate().is_err());
        request.cursor = None;
        request.until = "2026-11-18T08:00:00Z".into();
        assert!(request.validate().is_err());
    }
    #[test]
    fn future_open_duplicate_out_of_window_and_unordered_intervals_never_schedule_work() {
        let mut request = request();
        request.limit = 2;
        for invalid in ["future", "empty", "outside", "duplicate", "order", "cursor"] {
            let mut page = page();
            match invalid {
                "future" => page.observed_at = "2026-11-10T06:59:00Z".into(),
                "empty" => page.intervals[0].until = page.intervals[0].from.clone(),
                "outside" => page.intervals[0].until = "2026-11-10T06:30:00Z".into(),
                "duplicate" => page.intervals.push(page.intervals[0].clone()),
                "order" => {
                    let mut later = page.intervals[0].clone();
                    later.native_id = "18".into();
                    later.from = "2026-11-10T07:00:00Z".into();
                    later.until = "2026-11-10T08:00:00Z".into();
                    page.intervals.insert(0, later);
                }
                "cursor" => page.next_cursor = Some("more".into()),
                _ => unreachable!(),
            }
            assert!(!page.matches_request(&request), "{invalid}");
        }
    }
}
