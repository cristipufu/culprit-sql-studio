//! The identity and clock behind a measurement. Comparisons ask this contract before subtracting values.
//! A scan that doesn't say where its database is compares with none; a known endpoint is never filled in from today's
//! connection.

use crate::{Engine, Period, Snapshot, Workload, WorkloadKind, format::epoch_seconds};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

fn counter_generation(value: &impl Serialize) -> Option<String> {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_vec(value).ok()?;
    Some(Sha256::digest(encoded).iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum DatabaseLocation {
    Server { host: String, port: u16, instance: Option<String> },
    Socket { directory: String, port: u16 },
    File { path: String },
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct DatabaseIdentity {
    pub engine: Engine,
    pub database: String,
    pub location: DatabaseLocation,
    // An engine-provided incarnation, when available. Never inferred from a database name or server start time.
    pub incarnation: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ComparisonReason {
    DifferentEngine,
    DifferentDatabase,
    DifferentEndpoint,
    UnknownIdentity,
    DifferentSource,
    DifferentScope,
    DifferentPeriod,
    Reset,
    InvalidTime,
}

impl ComparisonReason {
    pub fn explanation(&self) -> &'static str {
        match self {
            Self::DifferentEngine => "The scans describe different database engines.",
            Self::DifferentDatabase => "The scans describe different databases or database incarnations.",
            Self::DifferentEndpoint => "The scans describe different server endpoints or database files.",
            Self::UnknownIdentity => "One scan does not record enough database identity to establish a comparison.",
            Self::DifferentSource => "The measurements come from different sources.",
            Self::DifferentScope => "The measurements cover different scopes.",
            Self::DifferentPeriod => "The measurements cover different kinds or lengths of periods.",
            Self::Reset => "The measurement counters started over between these scans.",
            Self::InvalidTime => "The measurements do not establish a valid earlier and later time.",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "reason", rename_all = "kebab-case")]
#[ts(export)]
pub enum Comparison {
    Compatible,
    Incompatible(ComparisonReason),
}

impl Comparison {
    pub fn allows_comparison(&self) -> bool {
        !matches!(self, Self::Incompatible(_))
    }
    pub fn reason(&self) -> Option<&ComparisonReason> {
        if let Self::Incompatible(reason) = self { Some(reason) } else { None }
    }
}

impl DatabaseIdentity {
    pub fn server(engine: Engine, database: &str, host: &str, port: u16, instance: Option<String>) -> Self {
        Self {
            engine,
            database: database.into(),
            location: if host.trim().starts_with('/') {
                DatabaseLocation::Socket { directory: host.trim().into(), port }
            } else {
                DatabaseLocation::Server {
                    host: host.trim().trim_end_matches('.').to_ascii_lowercase(),
                    port,
                    instance,
                }
            },
            incarnation: None,
        }
    }
    pub fn file(database: &str, path: String) -> Self {
        Self {
            engine: Engine::Sqlite,
            database: database.into(),
            location: DatabaseLocation::File { path },
            incarnation: None,
        }
    }
    pub fn comparison(&self, other: &Self) -> Comparison {
        use ComparisonReason::*;
        let different = |reason| Comparison::Incompatible(reason);
        if self.engine != other.engine {
            return different(DifferentEngine);
        }
        if self.database.trim().is_empty() || other.database.trim().is_empty() {
            return different(UnknownIdentity);
        }
        if matches!((&self.incarnation, &other.incarnation), (Some(a), Some(b)) if a != b) {
            return different(DifferentDatabase);
        }
        if self.incarnation.is_some() != other.incarnation.is_some() {
            return different(UnknownIdentity);
        }
        match (&self.location, &other.location) {
            // A database's name alone says nothing about which server it is on.
            (DatabaseLocation::Unknown, _) | (_, DatabaseLocation::Unknown) => different(UnknownIdentity),
            (
                DatabaseLocation::Server { host: a, port: ap, instance: ai },
                DatabaseLocation::Server { host: b, port: bp, instance: bi },
            ) => {
                if a.trim().is_empty() || b.trim().is_empty() || *ap == 0 || *bp == 0 {
                    return different(UnknownIdentity);
                }
                if !a.trim().trim_end_matches('.').eq_ignore_ascii_case(b.trim().trim_end_matches('.'))
                    || ap != bp
                    || ai != bi
                {
                    return different(DifferentEndpoint);
                }
                if self.database != other.database {
                    return different(DifferentDatabase);
                }
                Comparison::Compatible
            }
            (DatabaseLocation::File { path: a }, DatabaseLocation::File { path: b }) => {
                if a.is_empty() || b.is_empty() {
                    different(UnknownIdentity)
                } else if a != b {
                    different(DifferentEndpoint)
                } else {
                    // A symlink can give the same canonical file a different display name.
                    Comparison::Compatible
                }
            }
            (
                DatabaseLocation::Socket { directory: a, port: ap },
                DatabaseLocation::Socket { directory: b, port: bp },
            ) => {
                if a.is_empty() || b.is_empty() || *ap == 0 || *bp == 0 {
                    different(UnknownIdentity)
                } else if a != b || ap != bp {
                    different(DifferentEndpoint)
                } else if self.database != other.database {
                    different(DifferentDatabase)
                } else {
                    Comparison::Compatible
                }
            }
            _ => different(DifferentEndpoint),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "source", rename_all = "kebab-case")]
#[ts(export)]
pub enum MeasurementSource {
    Workload(WorkloadKind),
    Named(String),
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MeasurementScope {
    Database,
    Server,
    Session,
    File,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum MeasurementPeriod {
    Window { span: Option<Period>, hours: Option<f64>, requested_minutes: Option<u32> },
    Cumulative { observed_at: String, hours: Option<f64> },
    ObservedScans { span: Period },
    Sample { span: Period },
    Instant { at: String },
    Unknown,
}

impl MeasurementPeriod {
    pub fn hours(&self) -> Option<f64> {
        let hours = match self {
            Self::Window { span: Some(span), hours, .. } => {
                let actual = span.hours();
                if actual <= 0.0 || hours.is_some_and(|h| !h.is_finite() || ((h - actual) * 3600.0).abs() > 1.0) {
                    return None;
                }
                Some(actual)
            }
            Self::ObservedScans { span } | Self::Sample { span } => Some(span.hours()),
            Self::Window { hours, .. } | Self::Cumulative { hours, .. } => *hours,
            _ => None,
        };
        hours.filter(|h| h.is_finite() && *h > 0.0)
    }
    pub fn is_cumulative(&self) -> bool {
        matches!(self, Self::Cumulative { .. })
    }
    pub fn is_windowed(&self) -> bool {
        matches!(self, Self::Window { .. } | Self::ObservedScans { .. } | Self::Sample { .. })
    }
    pub fn observed_at(&self) -> Option<&str> {
        match self {
            Self::Cumulative { observed_at, .. } => Some(observed_at),
            Self::Window { span: Some(span), .. } | Self::ObservedScans { span } | Self::Sample { span } => {
                Some(&span.end)
            }
            Self::Instant { at } => Some(at),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum ResetMarker {
    At(String),
    Generation(String),
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MeasurementContext {
    pub database: DatabaseIdentity,
    pub source: MeasurementSource,
    pub scope: MeasurementScope,
    pub period: MeasurementPeriod,
    pub reset: ResetMarker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CounterTransition {
    Continued,
    Reset,
    Unmarked,
    Incompatible(ComparisonReason),
}

impl MeasurementContext {
    pub fn workload(
        identity: DatabaseIdentity,
        workload: &Workload,
        taken_at: &str,
        requested_minutes: Option<u32>,
    ) -> Self {
        let mut source = workload
            .kind()
            .map(MeasurementSource::Workload)
            .unwrap_or_else(|| MeasurementSource::Named(workload.source.clone()));
        let hours = Some(workload.window_hours).filter(|h| h.is_finite() && *h > 0.0);
        let bounds = workload
            .window_start
            .as_ref()
            .zip(workload.window_end.as_ref())
            .map(|(start, end)| Period { start: start.clone(), end: end.clone() });
        let invalid_bounds = bounds.as_ref().is_some_and(|p| p.hours() <= 0.0);
        let hours = hours.filter(|_| !invalid_bounds);
        let span = bounds.filter(|p| hours.is_some() && p.hours() > 0.0);
        let mut period = if workload.windowed {
            MeasurementPeriod::Window {
                span,
                hours,
                requested_minutes: if matches!(workload.kind(), Some(WorkloadKind::QueryLog)) {
                    None
                } else {
                    requested_minutes
                },
            }
        } else {
            MeasurementPeriod::Cumulative {
                // A cache's window_end can be the last execution, even on a later scan with no new calls.
                // Counter differences are bounded by collection times, not by the most recent execution.
                observed_at: taken_at.into(),
                hours,
            }
        };
        let inferred = workload.history_inputs.iter().any(|r| r.purpose == crate::BaselinePurpose::WindowInference);
        let mut reset =
            if workload.windowed || !matches!(workload.kind(), Some(WorkloadKind::StatementStats)) || inferred {
                // The oldest cached plan/digest is not a source-wide reset marker, nor is the earliest start the scan before
                // allowed (`infer_window`): a bound, not when the counters started.
                ResetMarker::Unknown
            } else {
                workload.window_start.clone().map(ResetMarker::At).unwrap_or(ResetMarker::Unknown)
            };
        let mut scope = MeasurementScope::Database;
        if let Some(counters) = &workload.counter_set {
            let members: Vec<_> = counters.members.keys().collect();
            source = MeasurementSource::Named(format!(
                "{}:{:?}:{}",
                workload.source,
                counters.scope,
                counter_generation(&members).unwrap_or_default()
            ));
            let known = counters.epochs_known();
            if known {
                reset =
                    counter_generation(&counters.members).map(ResetMarker::Generation).unwrap_or(ResetMarker::Unknown);
            } else {
                reset = ResetMarker::Unknown;
                scope = MeasurementScope::Unknown;
                if let MeasurementPeriod::Cumulative { hours, .. } = &mut period {
                    *hours = None;
                }
            }
        }
        Self { database: identity, source, scope, period, reset }
    }

    pub fn series_comparison(&self, other: &Self) -> Comparison {
        use ComparisonReason::*;
        let identity = self.database.comparison(&other.database);
        if !identity.allows_comparison() {
            return identity;
        }
        if self.source != other.source {
            return Comparison::Incompatible(DifferentSource);
        }
        if self.scope != other.scope || self.scope == MeasurementScope::Unknown {
            return Comparison::Incompatible(DifferentScope);
        }
        identity
    }

    /// A counter reset is an engine fact, never inferred merely because a number shrank.
    pub fn counter_transition(&self, later: &Self) -> CounterTransition {
        if let Comparison::Incompatible(reason) = self.series_comparison(later) {
            return CounterTransition::Incompatible(reason);
        }
        if !self.period.is_cumulative() || !later.period.is_cumulative() {
            return CounterTransition::Incompatible(ComparisonReason::DifferentPeriod);
        }
        let times =
            self.period.observed_at().and_then(epoch_seconds).zip(later.period.observed_at().and_then(epoch_seconds));
        let Some((before, after)) = times.filter(|(a, b)| b > a) else {
            return CounterTransition::Incompatible(ComparisonReason::InvalidTime);
        };
        for (reset, observed) in [(&self.reset, before), (&later.reset, after)] {
            if let ResetMarker::At(value) = reset
                && epoch_seconds(value).is_none_or(|reset| reset > observed)
            {
                return CounterTransition::Incompatible(ComparisonReason::InvalidTime);
            }
        }
        match (&self.reset, &later.reset) {
            (ResetMarker::At(a), ResetMarker::At(b)) => {
                let Some((a, b)) = epoch_seconds(a).zip(epoch_seconds(b)) else {
                    return CounterTransition::Incompatible(ComparisonReason::InvalidTime);
                };
                if a > before || b > after {
                    return CounterTransition::Incompatible(ComparisonReason::InvalidTime);
                }
                if (b - a).abs() <= 1.0 {
                    CounterTransition::Continued
                } else if b > before {
                    CounterTransition::Reset
                } else {
                    CounterTransition::Incompatible(ComparisonReason::InvalidTime)
                }
            }
            (_, ResetMarker::At(b)) => match epoch_seconds(b) {
                Some(b) if b > before && b <= after => CounterTransition::Reset,
                Some(b) if b <= after => CounterTransition::Unmarked,
                _ => CounterTransition::Incompatible(ComparisonReason::InvalidTime),
            },
            (ResetMarker::Generation(a), ResetMarker::Generation(b)) => {
                if a == b {
                    CounterTransition::Continued
                } else {
                    CounterTransition::Reset
                }
            }
            _ => CounterTransition::Unmarked,
        }
    }

    /// Findings from different selected windows cannot verify each other's absence.
    pub fn report_comparison(&self, other: &Self) -> Comparison {
        let series = self.series_comparison(other);
        if !series.allows_comparison() {
            return series;
        }
        let same_period = match (&self.period, &other.period) {
            (MeasurementPeriod::Cumulative { .. }, MeasurementPeriod::Cumulative { .. }) => {
                match self.counter_transition(other) {
                    CounterTransition::Reset => return Comparison::Incompatible(ComparisonReason::Reset),
                    CounterTransition::Incompatible(reason) => return Comparison::Incompatible(reason),
                    _ => true,
                }
            }
            (
                MeasurementPeriod::Window { requested_minutes: a, .. },
                MeasurementPeriod::Window { requested_minutes: b, .. },
            ) => {
                a == b
                    && self.period.hours().zip(other.period.hours()).is_some_and(|(before, after)| {
                        // Query Store includes whole boundary intervals. Two reads of the same selected
                        // window naturally differ by seconds or an interval boundary (up to an hour) as that
                        // window rolls. When a database or Query Store is younger than the requested window
                        // (for example, 1 day old yesterday and 2 days old today, or capped at 2 days of
                        // retention under a 7-day request), the window grows or rolls across hour boundaries
                        // without losing earlier history; only a shrinking partial window cannot verify
                        // disappearance.
                        let complete = a.is_some_and(|minutes| {
                            let requested = f64::from(minutes) / 60.0;
                            before + 1.0 >= requested * 0.9 && after + 1.0 >= requested * 0.9
                        });
                        let growing = before >= crate::RATE_MIN_HOURS && after + 1.0 >= before && after >= before * 0.8;
                        complete || growing || ((before - after) * 3600.0).abs() <= 1.0
                    })
            }
            _ => false,
        };
        if same_period { series } else { Comparison::Incompatible(ComparisonReason::DifferentPeriod) }
    }
}

impl Snapshot {
    pub fn database_identity(&self) -> DatabaseIdentity {
        self.identity.clone().unwrap_or_else(|| DatabaseIdentity {
            engine: self.server.engine,
            database: self.server.database.clone(),
            location: DatabaseLocation::Unknown,
            incarnation: None,
        })
    }
    /// Whether the identity a provider bound names this scan's own engine and database. One that names another is a
    /// collector's mistake; one that can't say where the database is still names it (it only compares with nothing).
    pub fn identity_names_this_database(&self) -> bool {
        self.identity.as_ref().is_none_or(|i| i.engine == self.server.engine && i.database == self.server.database)
    }
    pub fn database_comparison(&self, other: &Self) -> Comparison {
        if !self.identity_names_this_database() || !other.identity_names_this_database() {
            return Comparison::Incompatible(ComparisonReason::UnknownIdentity);
        }
        self.database_identity().comparison(&other.database_identity())
    }
    pub fn same_database(&self, other: &Self) -> bool {
        self.database_comparison(other).allows_comparison()
    }
    /// Whether this scan's workload can be compared with another scan's: its identity says which database it read well
    /// enough to compare (a location, not only a name), and its source measures that database alone. Not whether the
    /// workload is this database's: see [`Self::identity_names_this_database`].
    pub fn measurement_consistency(&self) -> Comparison {
        let identity = self.database_comparison(self);
        if !identity.allows_comparison() {
            return Comparison::Incompatible(ComparisonReason::UnknownIdentity);
        }
        match self.workload_measurement() {
            Some(context) if context.scope != MeasurementScope::Database => {
                Comparison::Incompatible(ComparisonReason::DifferentScope)
            }
            _ => identity,
        }
    }
    /// The workload's measurement context, for a scan that knows which database it read; `None` for one that doesn't,
    /// which compares with nothing.
    pub fn identified_measurement(&self) -> Option<MeasurementContext> {
        self.identity.as_ref().and(self.workload_measurement())
    }
    pub fn report_comparison(&self, later: &Self) -> Comparison {
        for snapshot in [self, later] {
            let consistency = snapshot.measurement_consistency();
            if !consistency.allows_comparison() {
                return consistency;
            }
        }
        let identity = self.database_comparison(later);
        if !identity.allows_comparison() {
            return identity;
        }
        match (self.workload_measurement(), later.workload_measurement()) {
            (Some(a), Some(b)) => a.report_comparison(&b),
            (None, None) => identity,
            _ => Comparison::Incompatible(ComparisonReason::DifferentSource),
        }
    }
    /// The workload's measurement context: what its own period fields say, with the database it read. Worked out each
    /// time from the workload, never stored beside it, so there is one description of the period to keep right.
    pub fn workload_measurement(&self) -> Option<MeasurementContext> {
        let workload = self.workload.as_ref()?;
        Some(MeasurementContext::workload(
            self.database_identity(),
            workload,
            &self.taken_at,
            self.options.as_ref().map(|o| o.window_in_minutes()),
        ))
    }
    /// Record which database the scan read: what its comparisons with other scans are decided by.
    pub fn bind_identity(&mut self, identity: DatabaseIdentity) {
        self.identity = Some(identity);
    }
}

impl Workload {
    pub fn is_cumulative(&self) -> bool {
        !self.windowed
    }
    pub fn is_windowed(&self) -> bool {
        self.windowed
    }
    pub fn measurement_hours(&self) -> f64 {
        if self.window_hours.is_finite() && self.window_hours >= 0.0 { self.window_hours } else { 0.0 }
    }
}

impl crate::Digest {
    pub fn database_comparison(&self, later: &Snapshot) -> Comparison {
        if self.identity.as_ref().is_some_and(|i| i.database != self.database)
            || !later.database_comparison(later).allows_comparison()
        {
            return Comparison::Incompatible(ComparisonReason::UnknownIdentity);
        }
        match (&self.identity, &later.identity) {
            (Some(a), Some(b)) => a.comparison(b),
            _ => Comparison::Incompatible(ComparisonReason::UnknownIdentity),
        }
    }
    pub fn comparison(&self, later: &Snapshot) -> Comparison {
        let identity = self.database_comparison(later);
        if !identity.allows_comparison() {
            return identity;
        }
        if !later.measurement_consistency().allows_comparison() {
            return Comparison::Incompatible(ComparisonReason::UnknownIdentity);
        }
        match (&self.measurement, later.workload_measurement()) {
            (Some(a), Some(b)) => {
                if self.identity.as_ref().is_some_and(|id| !id.comparison(&a.database).allows_comparison())
                    || a.database.database != self.database
                {
                    Comparison::Incompatible(ComparisonReason::UnknownIdentity)
                } else {
                    a.report_comparison(&b)
                }
            }
            (None, None) => identity,
            _ => Comparison::Incompatible(ComparisonReason::DifferentSource),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distributed_counter_sets_survive_identity_binding_and_compare_each_native_epoch() {
        let mut before: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        before.server.engine = Engine::Yugabyte;
        before.taken_at = "2026-10-23T00:00:00Z".into();
        let w = before.workload.as_mut().unwrap();
        w.source = "YSQL cluster statements".into();
        w.counter_set = Some(crate::CounterSet {
            scope: crate::CounterScope::Cluster,
            complete: true,
            members: [
                ("node-a".into(), Some("2026-10-01T00:00:00Z".into())),
                ("node-b".into(), Some("2026-10-02T00:00:00Z".into())),
            ]
            .into(),
        });
        let identity =
            DatabaseIdentity::server(Engine::Yugabyte, &before.server.database, "cluster.example", 5433, None);
        before.bind_identity(identity.clone());
        let a = before.workload_measurement().unwrap();
        let mut later = before.clone();
        later.taken_at = "2026-10-24T00:00:00Z".into();
        later.bind_identity(identity.clone());
        assert_eq!(a.counter_transition(&later.workload_measurement().unwrap()), CounterTransition::Continued);
        later
            .workload
            .as_mut()
            .unwrap()
            .counter_set
            .as_mut()
            .unwrap()
            .members
            .insert("node-b".into(), Some("2026-10-23T12:00:00Z".into()));
        later.bind_identity(identity.clone());
        assert_eq!(a.counter_transition(&later.workload_measurement().unwrap()), CounterTransition::Reset);
        later.workload.as_mut().unwrap().counter_set.as_mut().unwrap().members.remove("node-b");
        later.bind_identity(identity.clone());
        assert_eq!(
            a.counter_transition(&later.workload_measurement().unwrap()),
            CounterTransition::Incompatible(ComparisonReason::DifferentSource)
        );
        later.workload.as_mut().unwrap().counter_set.as_mut().unwrap().complete = false;
        later.bind_identity(identity);
        let context = later.workload_measurement().unwrap();
        assert_eq!(context.scope, MeasurementScope::Unknown);
        assert_eq!(context.period.hours(), None);
    }
    fn database(host: &str) -> DatabaseIdentity {
        DatabaseIdentity::server(Engine::Postgres, "shop", host, 5432, None)
    }
    fn context(at: &str, reset: ResetMarker) -> MeasurementContext {
        MeasurementContext {
            database: database("db.example"),
            source: MeasurementSource::Workload(WorkloadKind::StatementStats),
            scope: MeasurementScope::Database,
            period: MeasurementPeriod::Cumulative { observed_at: at.into(), hours: Some(24.0) },
            reset,
        }
    }
    #[test]
    fn identity_includes_endpoint_port_file_and_incarnation() {
        let a = database("DB.EXAMPLE.");
        assert_eq!(a.comparison(&database("db.example")), Comparison::Compatible);
        assert_eq!(
            a.comparison(&database("other.example")),
            Comparison::Incompatible(ComparisonReason::DifferentEndpoint)
        );
        let mut b = database("db.example");
        if let DatabaseLocation::Server { port, .. } = &mut b.location {
            *port = 55432;
        }
        assert!(!a.comparison(&b).allows_comparison());
        let mut b = a.clone();
        b.database = "Shop".into();
        assert!(!a.comparison(&b).allows_comparison(), "observed database identifiers retain their case");
        let mut a = a;
        a.incarnation = Some("first".into());
        let mut b = a.clone();
        b.incarnation = Some("replacement".into());
        assert!(!a.comparison(&b).allows_comparison());
        let a = DatabaseIdentity::file("shop", "/apps/one/shop.sqlite".into());
        let b = DatabaseIdentity::file("shop", "/apps/two/shop.sqlite".into());
        assert!(!a.comparison(&b).allows_comparison());
    }
    #[test]
    fn socket_paths_are_not_normalized_as_dns_names() {
        let a = DatabaseIdentity::server(Engine::Postgres, "shop", "/tmp/Socket", 5432, None);
        let b = DatabaseIdentity::server(Engine::Postgres, "shop", "/tmp/socket", 5432, None);
        assert!(!a.comparison(&b).allows_comparison());
    }
    #[test]
    fn an_unknown_location_compares_with_nothing_whatever_the_names() {
        let mut a = database("db.example");
        a.location = DatabaseLocation::Unknown;
        assert_eq!(a.comparison(&a.clone()), Comparison::Incompatible(ComparisonReason::UnknownIdentity));
        assert_eq!(a.comparison(&database("db.example")), Comparison::Incompatible(ComparisonReason::UnknownIdentity));
        let mut invalid = database("");
        invalid.database.clear();
        assert!(!invalid.comparison(&invalid).allows_comparison());
    }
    #[test]
    fn counter_comparison_requires_matching_source_scope_and_clock_order() {
        let a = context("2026-10-10T00:00:00Z", ResetMarker::Unknown);
        let b = context("2026-10-11T00:00:00Z", ResetMarker::Unknown);
        assert_eq!(a.counter_transition(&b), CounterTransition::Unmarked);
        assert!(matches!(b.counter_transition(&a), CounterTransition::Incompatible(ComparisonReason::InvalidTime)));
        let mut wrong = b.clone();
        wrong.scope = MeasurementScope::Server;
        assert!(matches!(
            a.counter_transition(&wrong),
            CounterTransition::Incompatible(ComparisonReason::DifferentScope)
        ));
        wrong = b.clone();
        wrong.source = MeasurementSource::Workload(WorkloadKind::PlanCache);
        assert!(matches!(
            a.counter_transition(&wrong),
            CounterTransition::Incompatible(ComparisonReason::DifferentSource)
        ));
        wrong = b.clone();
        wrong.database = database("elsewhere.example");
        assert!(matches!(
            a.counter_transition(&wrong),
            CounterTransition::Incompatible(ComparisonReason::DifferentEndpoint)
        ));
    }
    #[test]
    fn reset_marker_equivalent_timezones_and_confirmed_resets_have_distinct_results() {
        let a = context("2026-10-10T00:00:00Z", ResetMarker::At("2026-10-01T00:00:00Z".into()));
        let mut b = context("2026-10-11T00:00:00Z", ResetMarker::At("2026-10-01T03:00:00+03:00".into()));
        assert_eq!(a.counter_transition(&b), CounterTransition::Continued);
        b.reset = ResetMarker::At("2026-10-10T12:00:00Z".into());
        assert_eq!(a.counter_transition(&b), CounterTransition::Reset);
        assert_eq!(a.report_comparison(&b), Comparison::Incompatible(ComparisonReason::Reset));
        b.reset = ResetMarker::At("2026-10-09T12:00:00Z".into());
        assert!(matches!(a.counter_transition(&b), CounterTransition::Incompatible(ComparisonReason::InvalidTime)));
        b.reset = ResetMarker::At("bad timestamp".into());
        assert!(matches!(a.counter_transition(&b), CounterTransition::Incompatible(ComparisonReason::InvalidTime)));
    }
    #[test]
    fn interval_measurements_are_never_subtracted_as_running_totals() {
        let a = context("2026-10-10T00:00:00Z", ResetMarker::Unknown);
        let mut b = context("2026-10-11T00:00:00Z", ResetMarker::Unknown);
        b.period = MeasurementPeriod::ObservedScans {
            span: Period { start: "2026-10-10T00:00:00Z".into(), end: "2026-10-11T00:00:00Z".into() },
        };
        assert_eq!(b.period.hours(), Some(24.0));
        assert!(matches!(a.counter_transition(&b), CounterTransition::Incompatible(ComparisonReason::DifferentPeriod)));
    }
    #[test]
    fn selected_window_mismatches_cannot_verify_a_fix() {
        let mut a = context("2026-10-10T00:00:00Z", ResetMarker::Unknown);
        a.period = MeasurementPeriod::Window {
            span: Some(Period { start: "2026-10-09T00:00:00Z".into(), end: "2026-10-10T00:00:00Z".into() }),
            hours: Some(24.0),
            requested_minutes: Some(1440),
        };
        let mut b = a.clone();
        assert_eq!(a.report_comparison(&b), Comparison::Compatible);
        if let MeasurementPeriod::Window { requested_minutes, .. } = &mut b.period {
            *requested_minutes = Some(60);
        }
        assert_eq!(a.report_comparison(&b), Comparison::Incompatible(ComparisonReason::DifferentPeriod));
        b = a.clone();
        if let MeasurementPeriod::Window { hours, .. } = &mut b.period {
            *hours = Some(1.0);
        }
        assert!(b.period.hours().is_none(), "contradictory clocks are not silently reconciled");
    }
    #[test]
    fn a_digest_tables_oldest_entry_is_not_a_reset() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        let mut w = s.workload.unwrap();
        w.kind = Some(WorkloadKind::PerformanceSchema);
        let c = MeasurementContext::workload(database("db.example"), &w, &s.taken_at, Some(60));
        assert_eq!(c.reset, ResetMarker::Unknown);
    }
    #[test]
    fn cumulative_collection_time_is_not_the_last_execution_time() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        let mut w = s.workload.unwrap();
        w.kind = Some(WorkloadKind::PlanCache);
        w.window_end = Some("2026-10-01T00:00:00Z".into());
        let before = MeasurementContext::workload(database("db.example"), &w, "2026-10-10T00:00:00Z", None);
        let after = MeasurementContext::workload(database("db.example"), &w, "2026-10-11T00:00:00Z", None);
        assert_eq!(before.counter_transition(&after), CounterTransition::Unmarked);
        assert_eq!(after.period.observed_at(), Some("2026-10-11T00:00:00Z"));
    }
    #[test]
    fn rolling_selected_windows_compare_when_both_cover_the_request() {
        let mut a = context("2026-10-10T00:00:00Z", ResetMarker::Unknown);
        a.period = MeasurementPeriod::Window { span: None, hours: Some(24.2), requested_minutes: Some(1440) };
        let mut b = a.clone();
        b.period = MeasurementPeriod::Window { span: None, hours: Some(24.8), requested_minutes: Some(1440) };
        assert!(a.report_comparison(&b).allows_comparison());
        b.period = MeasurementPeriod::Window { span: None, hours: Some(23.8), requested_minutes: Some(1440) };
        assert!(a.report_comparison(&b).allows_comparison(), "Query Store interval boundary jitter compares");
        a.period = MeasurementPeriod::Window { span: None, hours: Some(48.08), requested_minutes: Some(7 * 1440) };
        b.period = MeasurementPeriod::Window { span: None, hours: Some(48.01), requested_minutes: Some(7 * 1440) };
        assert!(
            a.report_comparison(&b).allows_comparison(),
            "2-day retention under 7d request with cron jitter compares"
        );
        b.period = MeasurementPeriod::Window { span: None, hours: Some(2.0), requested_minutes: Some(1440) };
        a.period = MeasurementPeriod::Window { span: None, hours: Some(24.2), requested_minutes: Some(1440) };
        assert!(!a.report_comparison(&b).allows_comparison(), "partial coverage cannot verify disappearance");
        b.period = MeasurementPeriod::Window { span: None, hours: Some(24.8), requested_minutes: Some(60) };
        assert!(!a.report_comparison(&b).allows_comparison(), "different requests still do not compare");
    }
    #[test]
    fn an_unknown_selected_window_never_claims_one_minute_or_a_daily_rate() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        let mut w = s.workload.unwrap();
        // A windowed source that didn't say how long its window is.
        (w.windowed, w.window_hours) = (true, 0.0);
        assert!(
            MeasurementContext::workload(database("db.example"), &w, &s.taken_at, Some(60)).period.hours().is_none()
        );
        assert_eq!(w.window_phrase(), "the recorded workload window");
        assert!(w.rate_per_day(1000.0).is_none());
    }
    #[test]
    fn untimed_log_records_never_acquire_a_period_from_other_records_dates() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        let mut w = s.workload.unwrap();
        w.kind = Some(WorkloadKind::QueryLog);
        w.log = Some("JSON".into());
        w.windowed = true;
        w.window_hours = 0.0;
        w.window_start = Some("2026-10-10T00:00:00Z".into());
        w.window_end = Some("2026-10-11T00:00:00Z".into());
        let c = MeasurementContext::workload(database("db.example"), &w, &s.taken_at, Some(60));
        assert!(c.period.hours().is_none());
        if let MeasurementPeriod::Window { requested_minutes, .. } = c.period {
            assert_eq!(requested_minutes, None);
        } else {
            panic!("log span");
        }
    }
}
