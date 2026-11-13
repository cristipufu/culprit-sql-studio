use crate::value::{Rec, Val};
pub(crate) fn metadata() -> Rec {
    Rec::of(&[
        ("edition", Val::Int(3)),
        ("major", Val::Text("16".into())),
        ("server_name", Val::Text("fixture-machine\\NATIVE".into())),
        ("machine_name", Val::Text("fixture-machine".into())),
        ("process_id", Val::Int(42)),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
    ])
}
pub(crate) fn epoch(ticks: i64) -> Rec {
    Rec::of(&[("started_at_ticks", Val::Int(ticks)), ("ms_ticks", Val::Int(500))])
}
