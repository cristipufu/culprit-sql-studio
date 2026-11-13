//! Native interval boundaries have one projection and decoder for capture and scheduler discovery.
use crate::read::{NativeRead, Parameter, ReadError, invalid_data, optional_text, text};
use culprit_core::{Result, telemetry::*};

const COLUMNS: &str = "runtime_stats_interval_id AS id, CONVERT(varchar(40),start_time,127) AS starts, CONVERT(varchar(40),end_time,127) AS ends";
const CLOCK: &str = "SELECT CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at, (SELECT CONVERT(varchar(40),MIN(start_time),127) FROM sys.query_store_runtime_stats_interval) AS metadata_from OPTION(MAXDOP 1)";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Position {
    pub end: String,
    pub id: i64,
}
impl Position {
    pub fn of(interval: &TelemetryInterval) -> Result<Self> {
        Ok(Self { end: canonical_time(&interval.until)?, id: interval.native_id.parse().map_err(|_| invalid_data())? })
    }
    pub fn is_valid(&self) -> bool {
        self.id > 0 && self.end.len() <= 64 && canonical_time(&self.end).is_ok()
    }
    fn follows(&self, previous: &Self) -> bool {
        let times = chrono::DateTime::parse_from_rfc3339(&self.end)
            .ok()
            .zip(chrono::DateTime::parse_from_rfc3339(&previous.end).ok());
        times.is_some_and(|(end, before)| (end, self.id) > (before, previous.id))
    }
}
fn select(predicate: &str) -> String {
    format!(
        "SELECT TOP (@P1) {COLUMNS} FROM sys.query_store_runtime_stats_interval WHERE {predicate} ORDER BY end_time,runtime_stats_interval_id OPTION(MAXDOP 1)"
    )
}
fn decode(row: &crate::value::Rec) -> Result<TelemetryInterval> {
    let id = crate::read::integer(row, "id")?;
    let interval = TelemetryInterval {
        native_id: id.to_string(),
        from: canonical_time(&text(row, "starts")?)?,
        until: canonical_time(&text(row, "ends")?)?,
    };
    let from = chrono::DateTime::parse_from_rfc3339(&interval.from).map_err(|_| invalid_data())?;
    let until = chrono::DateTime::parse_from_rfc3339(&interval.until).map_err(|_| invalid_data())?;
    if id <= 0 || from >= until {
        return Err(invalid_data());
    }
    Ok(interval)
}
pub(crate) async fn exact<R: NativeRead>(reader: &mut R, request: &CollectionRequest) -> Result<TelemetryInterval> {
    let (from, until) = request.bounds()?;
    let rows = reader.query(
        &select("start_time=CONVERT(datetimeoffset,@P2,127) AND end_time=CONVERT(datetimeoffset,@P3,127) AND end_time<=SYSDATETIMEOFFSET()"),
        vec![Parameter::Integer(2),Parameter::Text(from.to_rfc3339()),Parameter::Text(until.to_rfc3339())],2,
    ).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(culprit_core::Error::invalid(culprit_core::t!("collection.exact_closed_interval")));
    };
    decode(row)
}
pub(crate) async fn clock<R: NativeRead>(reader: &mut R) -> Result<(String, Option<String>)> {
    let rows = reader.query(CLOCK, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    Ok((
        canonical_time(&text(row, "observed_at")?)?,
        optional_text(row, "metadata_from")?.map(|at| canonical_time(&at)).transpose()?,
    ))
}
pub(crate) async fn page<R: NativeRead>(
    reader: &mut R,
    request: &IntervalDiscoveryRequest,
    at: &str,
    after: Option<&Position>,
) -> Result<Vec<TelemetryInterval>> {
    request.validate()?;
    let (from, until) = request.bounds()?;
    let mut parameters = vec![
        Parameter::Integer(i64::from(request.limit) + 1),
        Parameter::Text(from.to_rfc3339()),
        Parameter::Text(until.to_rfc3339()),
        Parameter::Text(canonical_time(at)?),
    ];
    let predicate = "end_time>CONVERT(datetimeoffset,@P2,127) AND end_time<=CONVERT(datetimeoffset,@P3,127) AND end_time<=CONVERT(datetimeoffset,@P4,127)";
    let predicate = if let Some(after) = after {
        if !after.is_valid() {
            return Err(invalid_data());
        }
        parameters.extend([Parameter::Text(after.end.clone()), Parameter::Integer(after.id)]);
        format!(
            "{predicate} AND (end_time>CONVERT(datetimeoffset,@P5,127) OR (end_time=CONVERT(datetimeoffset,@P5,127) AND runtime_stats_interval_id>@P6))"
        )
    } else {
        predicate.into()
    };
    let rows = reader
        .query(&select(&predicate), parameters, request.limit as usize + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > request.limit as usize + 1 {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut previous = after.cloned();
    rows.iter()
        .map(|row| {
            let interval = decode(row)?;
            let position = Position::of(&interval)?;
            if previous.as_ref().is_some_and(|previous| !position.follows(previous)) {
                return Err(invalid_data());
            }
            previous = Some(position);
            Ok(interval)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };
    fn row(id: i64, from: &str, until: &str) -> Rec {
        Rec::of(&[("id", Val::Int(id)), ("starts", Val::Text(from.into())), ("ends", Val::Text(until.into()))])
    }
    #[tokio::test]
    async fn capture_and_discovery_decode_identical_native_boundaries_without_approximating_them() {
        let from = "2026-11-10T06:00:00.1234567+00:00";
        let until = "2026-11-10T07:00:00.7654321+00:00";
        let request = CollectionRequest {
            subject: culprit_core::telemetry::CollectionSubject::Database,
            selection: CollectionSelection::QueryHistory { from: from.into(), until: until.into() },
            budget: CollectionBudget::default(),
        };
        let mut exact_io = FakeRead::new(vec![Ok(vec![row(17, from, until)])]);
        let expected = exact(&mut exact_io, &request).await.unwrap();
        let discovery = IntervalDiscoveryRequest {
            from: from.into(),
            until: until.into(),
            limit: 1,
            cursor: None,
            budget: CollectionBudget::default(),
        };
        let mut page_io = FakeRead::new(vec![Ok(vec![row(17, from, until)])]);
        assert_eq!(page(&mut page_io, &discovery, "2026-11-10T08:00:00Z", None).await.unwrap(), vec![expected]);
        assert!(!page_io.calls[0].sql.contains("2026-11-10"));
        assert_eq!(page_io.calls[0].limit, 2);
    }
    #[tokio::test]
    async fn cursor_values_stay_bound_and_query_failures_are_never_retried_as_another_layout() {
        let request = IntervalDiscoveryRequest {
            from: "2026-11-10T06:00:00Z".into(),
            until: "2026-11-10T08:00:00Z".into(),
            limit: 2,
            cursor: None,
            budget: CollectionBudget::default(),
        };
        let position = Position { end: "2026-11-10T07:00:00Z".into(), id: 17 };
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
        assert!(page(&mut io, &request, "2026-11-10T08:00:00Z", Some(&position)).await.is_err());
        assert_eq!(io.calls.len(), 1);
        assert!(io.calls[0].sql.contains("runtime_stats_interval_id>@P6"));
        assert!(!io.calls[0].sql.contains("2026-11-10"));
    }
}
