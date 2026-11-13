use super::*;

#[test]
fn a_pending_request_cannot_resume_under_a_different_subject() {
    let (pending, _) = request(&schedule(), &CollectionSubject::Database, &Checkpoint::default(), now()).unwrap();
    let checkpoint = Checkpoint { pending: Some(pending), ..Default::default() };
    let subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("logical-server"),
            native: NativeIdentity::new("pool"),
        },
    };
    assert!(request(&schedule(), &subject, &checkpoint, now()).is_err());

    let (pending, _) = request(&schedule(), &subject, &Checkpoint::default(), now()).unwrap();
    assert_eq!(pending.subject, subject);
    let checkpoint = Checkpoint { pending: Some(pending.clone()), ..Default::default() };
    let (retry, _) = request(&schedule(), &subject, &checkpoint, now() + TimeDelta::hours(1)).unwrap();
    assert_eq!(pending, retry);
    let mut another_pool = subject;
    let CollectionSubject::Shared { resource } = &mut another_pool else { unreachable!() };
    resource.namespace = NativeIdentity::new("another-logical-server");
    assert!(request(&schedule(), &another_pool, &checkpoint, now()).is_err());
}
fn schedule() -> FleetSchedule {
    FleetSchedule {
        id: "resources".into(),
        every_seconds: 60,
        jitter_seconds: 0,
        collection: ScheduledCollection::ResourceHistory { window_seconds: 300, overlap_seconds: 30 },
        budget: CollectionBudget::default(),
    }
}
fn now() -> DateTime<Utc> {
    parse_time("2026-11-10T12:00:00Z").unwrap()
}
#[test]
fn rolling_windows_retain_overlap_and_backfill_a_bounded_oldest_window() {
    let checkpoint = Checkpoint { last_until: Some("2026-11-10T11:00:00Z".into()), ..Default::default() };
    let (request, gap) = request(&schedule(), &CollectionSubject::Database, &checkpoint, now()).unwrap();
    assert_eq!(
        request.selection,
        CollectionSelection::ResourceHistory {
            from: "2026-11-10T10:59:30+00:00".into(),
            until: "2026-11-10T11:05:00+00:00".into()
        }
    );
    assert!(gap.is_none());
}
#[test]
fn expired_backfill_is_an_explicit_gap_and_a_failed_window_is_never_silently_skipped() {
    let mut checkpoint = Checkpoint { last_until: Some("2026-10-01T00:00:00Z".into()), ..Default::default() };
    let (pending, gap) = request(&schedule(), &CollectionSubject::Database, &checkpoint, now()).unwrap();
    assert!(gap.is_some());
    pending.validate().unwrap();
    checkpoint.pending = Some(pending.clone());
    checkpoint.pending_gap = gap.clone();
    let (retry, retry_gap) =
        request(&schedule(), &CollectionSubject::Database, &checkpoint, now() + TimeDelta::hours(1)).unwrap();
    assert_eq!(retry, pending);
    assert_eq!(retry_gap, gap);
}
#[test]
fn clock_rollback_and_invalid_saved_times_never_move_a_confirmed_cursor_backwards() {
    for at in ["2026-11-11T00:00:00Z", "bad"] {
        let checkpoint = Checkpoint { last_until: Some(at.into()), ..Default::default() };
        assert!(request(&schedule(), &CollectionSubject::Database, &checkpoint, now()).is_err());
    }
}

#[test]
fn saved_windows_cannot_relax_the_configured_budget_or_widen_the_source_read() {
    let (mut pending, _) = request(&schedule(), &CollectionSubject::Database, &Checkpoint::default(), now()).unwrap();
    pending.selection = CollectionSelection::ResourceHistory {
        from: "2026-11-09T12:00:00Z".into(),
        until: "2026-11-10T12:00:00Z".into(),
    };
    let checkpoint = Checkpoint { pending: Some(pending), ..Default::default() };
    assert!(request(&schedule(), &CollectionSubject::Database, &checkpoint, now()).is_err());
    let (mut pending, _) = request(&schedule(), &CollectionSubject::Database, &Checkpoint::default(), now()).unwrap();
    pending.budget.total_timeout_ms += 1;
    let checkpoint = Checkpoint { pending: Some(pending), ..Default::default() };
    assert!(request(&schedule(), &CollectionSubject::Database, &checkpoint, now()).is_err());
}
