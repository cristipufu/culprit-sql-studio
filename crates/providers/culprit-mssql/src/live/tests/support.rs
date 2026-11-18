use crate::{
    activity::tests::metadata,
    resources::test_support::replace,
    value::{Rec, Val},
};
use culprit_core::{SessionActionTarget, telemetry::*};

pub(crate) fn platform() -> Rec {
    replace(
        &crate::instance::test_support::metadata(),
        &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)],
    )
}

pub(crate) fn authority() -> Rec {
    crate::activity::tests::authority(&metadata(0))
}

pub(crate) fn target() -> SessionActionTarget {
    let authority = crate::local_source::test_azure_authority(&metadata(0)).unwrap();
    let row = metadata(42);
    let time = |name| crate::sql_time::native_time(&crate::read::text(&row, name).unwrap()).unwrap();
    SessionActionTarget {
        context: authority.completed(&authority).unwrap(),
        activity: ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: time("session_generation") },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: time("request_generation") }),
            transaction: None,
        },
        statement: None,
    }
}

pub(crate) fn session() -> Rec {
    Rec::of(&[
        ("session_id", Val::Int(42)),
        ("session_generation", Val::Text(target().activity.session.generation)),
        ("user_process", Val::Bool(true)),
        ("program_name", Val::Text("app".into())),
        ("own", Val::Bool(false)),
        ("session_established", Val::Bool(true)),
    ])
}

pub(crate) fn signalled(value: bool) -> Rec {
    Rec::of(&[("signalled", Val::Bool(value))])
}
