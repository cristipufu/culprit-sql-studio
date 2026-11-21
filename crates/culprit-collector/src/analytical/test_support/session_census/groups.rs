use super::*;

pub(super) fn apply(read: &mut ActivityCensusRead, case: SessionCensusCase) {
    let mut first = current_request(read.observations.remove(0), "0", "11111111-1111-1111-1111-111111111111");
    first.session_counters.writes = Some("2".into());
    let mut mars = current_request(first.clone(), "1", "22222222-2222-2222-2222-222222222222");
    if matches!(case, SessionCensusCase::NativeSignedElapsed) {
        native_context(&mut first).elapsed_ms = Some("-1".into());
        native_context(&mut mars).elapsed_ms = Some("-1".into());
    }
    if matches!(case, SessionCensusCase::ContradictoryMars) {
        native_context(&mut mars).elapsed_ms = Some("600001".into());
        read.observations = vec![first, mars];
        return;
    }
    let mut other = first.clone();
    other.key.session.id = "56".into();
    other.key.session.generation = "2020-01-01T11:55:00.0000000".into();
    other.session_context.as_deref_mut().unwrap().login_age_ms = Some("300000".into());
    other.session_context.as_deref_mut().unwrap().last_request_end_age_ms = Some("2000".into());
    other.client.as_mut().unwrap().user = Some("another-shared-login".into());
    other.session_counters.logical_reads = Some("7".into());
    other.session_counters.writes = Some("3".into());
    let native = native_context(&mut other);
    native.times.login_without_offset = "2020-01-01T11:55:00.0000000".into();
    native.times.last_request_end_without_offset = Some("2020-01-01T11:59:58.0000000".into());
    native.elapsed_ms = Some("300000".into());
    if matches!(case, SessionCensusCase::GroupUnknownTotals) {
        first.session_counters.writes = Some("0".into());
        other.session_counters.writes = Some("0".into());
        other.session_counters.logical_reads = None;
        other.session_context.as_deref_mut().unwrap().last_request_end_age_ms = None;
        let native = native_context(&mut other);
        native.elapsed_ms = None;
        native.times.last_request_end_without_offset = None;
        read.observations = vec![first, other];
    } else {
        read.observations = vec![first, mars, other];
    }
}

fn native_context(row: &mut ActivityObservation) -> &mut MssqlSessionContext {
    let SessionNativeContext::Mssql(native) =
        row.session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap()
    else {
        panic!("MSSQL test context")
    };
    native
}
