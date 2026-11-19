use crate::{
    resources::test_support::replace,
    value::{Rec, Val},
};
pub(crate) fn space_row() -> Rec {
    Rec::of(&[
        ("row_kind", Val::Int(0)),
        ("observed_at", Val::Text("2020-01-01T06:02:00.5Z".into())),
        ("database_name", Val::Text("sample".into())),
        ("native_id", Val::Int(1)),
        ("name", Val::Text("PRIMARY".into())),
        ("total", Val::Int(1000)),
        ("used", Val::Int(100)),
        ("max_bytes", Val::Null),
        ("can_grow", Val::Int(1)),
        ("reuse_wait", Val::Text("NOTHING".into())),
        ("users", Val::Int(50)),
        ("internals", Val::Null),
        ("versions", Val::Int(50)),
    ])
}
pub(crate) fn temp_space_row() -> Rec {
    replace(&space_row(), &[("database_name", Val::Null)])
}
pub(crate) fn temp_log_layout_row() -> Rec {
    Rec::of(&[
        ("edition", Val::Int(3)),
        ("major", Val::Int(16)),
        ("database_id", Val::Int(2)),
        ("metadata_visible", Val::Int(1)),
        ("log_permission", Val::Int(1)),
        ("usage_columns", Val::Int(3)),
        ("file_columns", Val::Int(4)),
        ("reuse_columns", Val::Int(1)),
    ])
}
pub(crate) fn temp_log_row() -> Rec {
    let row = replace(
        &space_row(),
        &[
            ("database_name", Val::Null),
            ("name", Val::Text("tempdb".into())),
            ("total", Val::Int(9_007_199_254_740_993)),
            ("used", Val::Int(0)),
            ("reuse_wait", Val::Text("ACTIVE_TRANSACTION".into())),
        ],
    );
    let mut columns = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    columns.push(("database_id", Val::Int(2)));
    Rec::of(&columns)
}
pub(crate) fn file_row() -> Rec {
    Rec::of(&[
        ("row_kind", Val::Int(1)),
        ("observed_at", Val::Text("2020-01-01T06:02:00.5Z".into())),
        ("database_name", Val::Text("sample".into())),
        ("database_id", Val::Int(5)),
        ("native_id", Val::Int(1)),
        ("file_guid", Val::Null),
        ("file_type", Val::Int(0)),
        ("file_type_desc", Val::Text("ROWS".into())),
        ("file_state", Val::Int(0)),
        ("file_state_desc", Val::Text("ONLINE".into())),
        ("data_space_id", Val::Int(1)),
        ("name", Val::Text("sample-data".into())),
        ("size_pages", Val::Int(128)),
        ("max_size_pages", Val::Int(-1)),
        ("growth", Val::Int(64)),
        ("is_percent_growth", Val::Bool(false)),
        ("is_read_only", Val::Bool(false)),
        ("filegroup_read_only", Val::Bool(false)),
        ("is_sparse", Val::Bool(false)),
        ("database_is_snapshot", Val::Bool(false)),
        ("used_pages", Val::Int(64)),
    ])
}
