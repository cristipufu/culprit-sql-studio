//! Bounded scheduler discovery uses the same native interval owner as history capture.
use super::description;
use crate::{
    conn::Pool,
    query_store::intervals::{self, Position},
    read::{NativeRead, ReadSession, invalid_data},
};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    acquisition_owner: NativeReadContext,
    from: String,
    until: String,
    observed_at: String,
    after: Position,
}
impl Cursor {
    fn read(request: &IntervalDiscoveryRequest, owner: &NativeReadContext, now: &str) -> Result<Option<Self>> {
        let Some(token) = request.cursor.as_deref() else { return Ok(None) };
        let cursor: Self = serde_json::from_str(token).map_err(|_| invalid_data())?;
        let (from, until) = request.bounds()?;
        let observed = chrono::DateTime::parse_from_rfc3339(&cursor.observed_at).map_err(|_| invalid_data())?;
        let now = chrono::DateTime::parse_from_rfc3339(now).map_err(|_| invalid_data())?;
        let end = chrono::DateTime::parse_from_rfc3339(&cursor.after.end).map_err(|_| invalid_data())?;
        if !owner.matches_earlier_scope(&cursor.acquisition_owner) {
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
    let acquisition = super::history_acquisition::HistoryAcquisition::read(reader, host, port).await?;
    acquisition.admit(reader).await?;
    let settings = description::settings(reader).await?;
    if settings.layout.edition != acquisition.edition() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let owner = acquisition.owner()?;
    let CollectionTarget::Database(database) = acquisition.target() else { return Err(invalid_data()) };
    let (now, metadata_from) = intervals::clock(reader).await?;
    let cursor = Cursor::read(request, &owner, &now)?;
    let observed_at = cursor.as_ref().map_or_else(|| now.clone(), |cursor| cursor.observed_at.clone());
    let mut intervals =
        intervals::page(reader, request, &observed_at, cursor.as_ref().map(|cursor| &cursor.after)).await?;
    let next_cursor = if intervals.len() > request.limit as usize {
        intervals.truncate(request.limit as usize);
        let (from, until) = request.bounds()?;
        Some(
            Cursor {
                acquisition_owner: owner.clone(),
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
        partition: SourcePartition::Target,
        source: culprit_core::source_id::QUERY_STORE.into(),
        observed_at,
        metadata_from,
        intervals,
        next_cursor,
    };
    if !page.matches_request(request) {
        return Err(invalid_data());
    }
    let after = acquisition.recheck(reader, host, port).await?;
    acquisition.completed(&after)?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::{FakeRead, instance_pressure_row, replace},
        telemetry::history_tests::{authority, native_platform, options, pressure},
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
    async fn owner() -> NativeReadContext {
        let mut reader = FakeRead::new(vec![Ok(vec![native_platform(5)]), Ok(vec![authority(5)])]);
        super::super::history_acquisition::HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433)
            .await
            .unwrap()
            .owner()
            .unwrap()
    }
    #[tokio::test]
    async fn cursors_bind_the_observed_native_owner_window_and_fixed_discovery_time() {
        let request = request();
        let (from, until) = request.bounds().unwrap();
        let owner = owner().await;
        let cursor = Cursor {
            acquisition_owner: owner.clone(),
            from: from.to_rfc3339(),
            until: until.to_rfc3339(),
            observed_at: "2026-11-10T08:01:00Z".into(),
            after: Position { end: "2026-11-10T07:00:00Z".into(), id: 17 },
        };
        let mut request = request;
        request.cursor = Some(cursor.encode().unwrap());
        assert!(Cursor::read(&request, &owner, "2026-11-10T08:02:00Z").unwrap().is_some());
        let mut changed = owner.clone();
        let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!("qualified owner") };
        *epoch = Some("another-native-epoch".into());
        assert_eq!(
            CollectionStop::from_error(&Cursor::read(&request, &changed, "2026-11-10T08:02:00Z").err().unwrap()),
            CollectionStop::SourceChanged
        );
        request.until = "2026-11-10T09:00:00Z".into();
        assert!(Cursor::read(&request, &owner, "2026-11-10T08:02:00Z").is_err());
        request.until = "2026-11-10T08:00:00Z".into();
        assert!(Cursor::read(&request, &owner, "2026-11-10T08:00:00Z").is_err());
        request.cursor = Some("{}".into());
        assert!(Cursor::read(&request, &owner, "2026-11-10T08:02:00Z").is_err());
    }
    fn interval(id: i64, from: &str, until: &str) -> Rec {
        Rec::of(&[("id", Val::Int(id)), ("starts", Val::Text(from.into())), ("ends", Val::Text(until.into()))])
    }
    fn replies(edition: i64, rows: Vec<Rec>, observed_at: &str, final_authority: Rec) -> FakeRead {
        let mut replies = vec![Ok(vec![native_platform(edition)])];
        if edition != 5 {
            replies.push(Ok(vec![crate::instance::test_support::epoch(100)]));
        }
        replies.extend([
            Ok(vec![authority(edition)]),
            Ok(vec![if edition == 5 { pressure() } else { instance_pressure_row() }]),
            Ok(vec![options(edition, true)]),
            Ok(vec![Rec::of(&[("observed_at", Val::Text(observed_at.into())), ("metadata_from", Val::Null)])]),
            Ok(rows),
        ]);
        if edition != 5 {
            replies.extend([Ok(vec![native_platform(edition)]), Ok(vec![crate::instance::test_support::epoch(100)])]);
        }
        replies.push(Ok(vec![final_authority]));
        FakeRead::new(replies)
    }
    #[tokio::test]
    async fn discovery_pages_preserve_native_boundaries_and_one_fixed_closed_horizon() {
        for edition in [5, 3] {
            let mut request = request();
            let first = interval(17, "2026-11-10T06:00:00.1234567Z", "2026-11-10T07:00:00.1234567Z");
            let second = interval(18, "2026-11-10T07:00:00.1234567Z", "2026-11-10T08:00:00Z");
            let mut reader = replies(edition, vec![first, second.clone()], "2026-11-10T08:01:00Z", authority(edition));
            let page = read(&mut reader, "fixture.invalid", 1433, &request).await.unwrap();
            assert_eq!(page.intervals.len(), 1);
            assert_eq!(page.intervals[0].from, "2026-11-10T06:00:00.123456700Z");
            assert_eq!(page.metadata_from, None, "interval metadata never invents a retained-data horizon");
            assert_eq!(page.database.incarnation.is_some(), edition == 5);
            assert_eq!(page.partition, SourcePartition::Target);
            assert_eq!(reader.calls.len(), if edition == 5 { 7 } else { 10 });
            request.cursor = page.next_cursor.clone();
            assert!(request.cursor.is_some());
            let mut reader = replies(edition, vec![second], "2026-11-10T08:10:00Z", authority(edition));
            let next = read(&mut reader, "fixture.invalid", 1433, &request).await.unwrap();
            assert_eq!(next.observed_at, page.observed_at);
            assert_eq!(next.intervals[0].native_id, "18");
            assert!(next.next_cursor.is_none());
            assert!(reader.calls[if edition == 5 { 5 } else { 6 }].sql.contains("runtime_stats_interval_id>@P6"));
        }
    }
    #[tokio::test]
    async fn pressure_and_unknown_pressure_stop_discovery_before_history_reads() {
        for (cpu, reason) in
            [(Val::Float(80.0), CollectionStop::Pressure), (Val::Null, CollectionStop::PressureUnavailable)]
        {
            let mut reader = FakeRead::new(vec![
                Ok(vec![native_platform(5)]),
                Ok(vec![authority(5)]),
                Ok(vec![replace(&pressure(), &[("avg_cpu_percent", cpu)])]),
            ]);
            assert_eq!(
                CollectionStop::from_error(
                    &read(&mut reader, "fixture.invalid", 1433, &request()).await.err().unwrap()
                ),
                reason
            );
            assert_eq!(reader.calls.len(), 3);
        }
    }
    #[tokio::test]
    async fn a_changed_native_owner_or_repeated_interval_cannot_complete_discovery() {
        for edition in [5, 3] {
            let field = if edition == 5 { "physical_incarnation" } else { "recovery_fork_guid" };
            let mut reader = replies(
                edition,
                vec![],
                "2026-11-10T08:01:00Z",
                replace(&authority(edition), &[(field, Val::Text("changed-native-key".into()))]),
            );
            assert_eq!(
                CollectionStop::from_error(
                    &read(&mut reader, "fixture.invalid", 1433, &request()).await.err().unwrap()
                ),
                CollectionStop::SourceChanged
            );
            let row = interval(17, "2026-11-10T06:00:00Z", "2026-11-10T07:00:00Z");
            let mut reader = replies(edition, vec![row.clone(), row], "2026-11-10T08:01:00Z", authority(edition));
            assert!(read(&mut reader, "fixture.invalid", 1433, &request()).await.is_err());
        }
    }
}
