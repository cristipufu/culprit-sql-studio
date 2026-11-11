//! Bounded scheduler discovery uses the same native interval owner as history capture.
use super::{admit_resources, description};
use crate::{
    conn::Pool,
    query_store::intervals::{self, Position},
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{DatabaseIdentity, Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    database: DatabaseIdentity,
    physical: String,
    from: String,
    until: String,
    observed_at: String,
    after: Position,
}
impl Cursor {
    fn read(
        request: &IntervalDiscoveryRequest,
        database: &DatabaseIdentity,
        physical: &str,
        now: &str,
    ) -> Result<Option<Self>> {
        let Some(token) = request.cursor.as_deref() else { return Ok(None) };
        let cursor: Self = serde_json::from_str(token).map_err(|_| invalid_data())?;
        let (from, until) = request.bounds()?;
        let observed = chrono::DateTime::parse_from_rfc3339(&cursor.observed_at).map_err(|_| invalid_data())?;
        let now = chrono::DateTime::parse_from_rfc3339(now).map_err(|_| invalid_data())?;
        let end = chrono::DateTime::parse_from_rfc3339(&cursor.after.end).map_err(|_| invalid_data())?;
        if &cursor.database != database || cursor.physical != physical {
            return Err(CollectionStop::SourceChanged.error());
        }
        if cursor.from != from.to_rfc3339()
            || cursor.until != until.to_rfc3339()
            || !cursor.after.is_valid()
            || observed > now
            || end <= from
            || end > until
            || end > observed
        {
            return Err(invalid_data());
        }
        Ok(Some(cursor))
    }
    fn encode(&self) -> Result<String> {
        let token = serde_json::to_string(self).map_err(|_| invalid_data())?;
        if token.len() > MAX_CURSOR_BYTES {
            return Err(invalid_data());
        }
        Ok(token)
    }
}

pub(crate) async fn discover(
    pool: Arc<Pool>,
    host: &str,
    port: u16,
    request: &IntervalDiscoveryRequest,
) -> Result<IntervalDiscoveryPage> {
    request.validate()?;
    let mut reader = ReadSession::new(pool, request.budget.clone());
    let result = read(&mut reader, host, port, request).await;
    reader.finish(result).await
}
async fn read<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &IntervalDiscoveryRequest,
) -> Result<IntervalDiscoveryPage> {
    description::ensure_supported(reader).await?;
    admit_resources(reader).await.map_err(ReadError::into_error)?;
    description::settings(reader).await?;
    let (database, physical) = description::identity(reader, host, port).await?;
    let (now, metadata_from) = intervals::clock(reader).await?;
    let cursor = Cursor::read(request, &database, &physical, &now)?;
    let observed_at = cursor.as_ref().map_or_else(|| now.clone(), |cursor| cursor.observed_at.clone());
    let mut intervals =
        intervals::page(reader, request, &observed_at, cursor.as_ref().map(|cursor| &cursor.after)).await?;
    let next_cursor = if intervals.len() > request.limit as usize {
        intervals.truncate(request.limit as usize);
        let (from, until) = request.bounds()?;
        Some(
            Cursor {
                database: database.clone(),
                physical: physical.clone(),
                from: from.to_rfc3339(),
                until: until.to_rfc3339(),
                observed_at: observed_at.clone(),
                after: Position::of(intervals.last().ok_or_else(invalid_data)?)?,
            }
            .encode()?,
        )
    } else {
        None
    };
    let page = IntervalDiscoveryPage {
        database,
        source: culprit_core::source_id::QUERY_STORE.into(),
        observed_at,
        metadata_from,
        intervals,
        next_cursor,
    };
    if !page.matches_request(request) {
        return Err(invalid_data());
    }
    let (after_database, after_physical) = description::identity(reader, host, port).await?;
    if page.database != after_database || physical != after_physical {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::{FakeRead, latest_row, replace},
        value::{Rec, Val},
    };
    fn request() -> IntervalDiscoveryRequest {
        IntervalDiscoveryRequest {
            from: "2026-11-10T06:30:00Z".into(),
            until: "2026-11-10T08:00:00Z".into(),
            cursor: None,
            limit: 1,
            budget: CollectionBudget::default(),
        }
    }
    fn cursor() -> Cursor {
        let request = request();
        let (from, until) = request.bounds().unwrap();
        Cursor {
            database: DatabaseIdentity::server(culprit_core::Engine::Mssql, "db", "fixture.invalid", 1433, None),
            physical: "physical-1".into(),
            from: from.to_rfc3339(),
            until: until.to_rfc3339(),
            observed_at: "2026-11-10T08:01:00Z".into(),
            after: Position { end: "2026-11-10T07:00:00Z".into(), id: 17 },
        }
    }
    #[test]
    fn cursors_bind_native_identity_window_and_fixed_discovery_time() {
        let cursor = cursor();
        let mut request = request();
        request.cursor = Some(cursor.encode().unwrap());
        assert!(Cursor::read(&request, &cursor.database, "physical-1", "2026-11-10T08:02:00Z").unwrap().is_some());
        assert_eq!(
            Cursor::read(&request, &cursor.database, "physical-2", "2026-11-10T08:02:00Z").err().unwrap().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged))
        );
        request.until = "2026-11-10T09:00:00Z".into();
        assert!(Cursor::read(&request, &cursor.database, "physical-1", "2026-11-10T08:02:00Z").is_err());
        request.until = "2026-11-10T08:00:00Z".into();
        assert!(Cursor::read(&request, &cursor.database, "physical-1", "2026-11-10T08:00:00Z").is_err());
        request.cursor = Some("{}".into());
        assert!(Cursor::read(&request, &cursor.database, "physical-1", "2026-11-10T08:02:00Z").is_err());
    }

    fn identity() -> Rec {
        Rec::of(&[
            ("database_name", Val::Text("db".into())),
            ("incarnation", Val::Text("logical-1".into())),
            ("physical_incarnation", Val::Text("physical-1".into())),
            ("updateability", Val::Text("READ_WRITE".into())),
        ])
    }
    fn pressure() -> Rec {
        replace(
            &latest_row("2026-11-10T07:59:30Z"),
            &[
                ("observed_at", Val::Text("2026-11-10T08:00:00Z".into())),
                ("avg_instance_cpu_percent", Val::Float(10.0)),
                ("max_worker_percent", Val::Float(10.0)),
                ("avg_log_write_percent", Val::Float(10.0)),
            ],
        )
    }
    fn interval(id: i64, from: &str, until: &str) -> Rec {
        Rec::of(&[("id", Val::Int(id)), ("starts", Val::Text(from.into())), ("ends", Val::Text(until.into()))])
    }
    fn replies(rows: Vec<Rec>, observed_at: &str, final_identity: Rec) -> FakeRead {
        FakeRead::new(vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![pressure()]),
            Ok(vec![Rec::of(&[
                ("actual_state_desc", Val::Text("READ_WRITE".into())),
                ("query_capture_mode_desc", Val::Text("AUTO".into())),
                ("wait_stats_capture_mode_desc", Val::Text("ON".into())),
            ])]),
            Ok(vec![identity()]),
            Ok(vec![Rec::of(&[("observed_at", Val::Text(observed_at.into())), ("metadata_from", Val::Null)])]),
            Ok(rows),
            Ok(vec![final_identity]),
        ])
    }
    #[tokio::test]
    async fn discovery_pages_preserve_native_boundaries_and_one_fixed_closed_horizon() {
        let mut request = request();
        let first = interval(17, "2026-11-10T06:00:00.1234567Z", "2026-11-10T07:00:00.1234567Z");
        let second = interval(18, "2026-11-10T07:00:00.1234567Z", "2026-11-10T08:00:00Z");
        let mut reader = replies(vec![first, second.clone()], "2026-11-10T08:01:00Z", identity());
        let page = read(&mut reader, "fixture.invalid", 1433, &request).await.unwrap();
        assert_eq!(page.intervals.len(), 1);
        assert_eq!(page.intervals[0].from, "2026-11-10T06:00:00.123456700Z");
        assert_eq!(page.metadata_from, None, "interval metadata never invents a retained-data horizon");
        request.cursor = page.next_cursor.clone();
        assert!(request.cursor.is_some());
        let mut reader = replies(vec![second], "2026-11-10T08:10:00Z", identity());
        let next = read(&mut reader, "fixture.invalid", 1433, &request).await.unwrap();
        assert_eq!(next.observed_at, page.observed_at);
        assert_eq!(next.intervals[0].native_id, "18");
        assert!(next.next_cursor.is_none());
        assert!(reader.calls[5].sql.contains("runtime_stats_interval_id>@P6"));
    }
    #[tokio::test]
    async fn pressure_and_unknown_pressure_stop_discovery_before_history_reads() {
        for (cpu, reason) in
            [(Val::Float(80.0), CollectionStop::Pressure), (Val::Null, CollectionStop::PressureUnavailable)]
        {
            let mut reader = FakeRead::new(vec![
                Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
                Ok(vec![replace(&pressure(), &[("avg_cpu_percent", cpu)])]),
            ]);
            assert_eq!(
                CollectionStop::from_error(
                    &read(&mut reader, "fixture.invalid", 1433, &request()).await.err().unwrap()
                ),
                reason
            );
            assert_eq!(reader.calls.len(), 2);
        }
    }
    #[tokio::test]
    async fn an_identity_change_or_repeated_native_position_cannot_complete_discovery() {
        let mut reader = replies(
            vec![],
            "2026-11-10T08:01:00Z",
            replace(&identity(), &[("physical_incarnation", Val::Text("physical-2".into()))]),
        );
        assert_eq!(
            CollectionStop::from_error(&read(&mut reader, "fixture.invalid", 1433, &request()).await.err().unwrap()),
            CollectionStop::SourceChanged
        );
        let row = interval(17, "2026-11-10T06:00:00Z", "2026-11-10T07:00:00Z");
        let mut reader = replies(vec![row.clone(), row], "2026-11-10T08:01:00Z", identity());
        assert!(read(&mut reader, "fixture.invalid", 1433, &request()).await.is_err());
    }
}
