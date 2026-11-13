use super::*;
use crate::{resources::test_support::replace, value::Val};
use culprit_core::telemetry::CollectionStop;

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("window_clock", Val::Text("2020-01-01T07:00:00Z".into())),
        ("intervals_retained", Val::Int(1)),
        ("native_intervals", Val::Text("<intervals><interval id=\"1\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/></intervals>".into())),
        ("window_start", Val::Text("2020-01-01T06:00:00Z".into())),
        ("window_end", Val::Text("2020-01-01T07:00:00Z".into())),
        ("window_hours", Val::Float(1.0)),
        ("wait_stats_capture_mode_desc", Val::Text("ON".into())),
    ])
}

pub(crate) fn layout_row() -> Rec {
    Rec::of(&[("tempdb", Val::Int(0)), ("waits", Val::Int(1))])
}

pub(crate) fn open_row(clock: &str, elapsed_seconds: f64) -> Rec {
    replace(
        &row(),
        &[
            ("window_clock", Val::Text(clock.into())),
            ("window_end", Val::Text(clock.into())),
            ("window_hours", Val::Float(elapsed_seconds / 3600.0)),
        ],
    )
}

#[test]
fn native_population_is_frozen_while_capture_changes_only_withdraw_wait_qualification() {
    let original = row();
    let mut window = FrozenWindow::decode(&original, 60, true).unwrap();
    assert!(window.matches_request(60));
    assert!(!window.matches_request(120));
    assert_eq!(window.hours, 1.0);
    assert_eq!(window.verify(&original).unwrap(), WindowVerification::Unchanged);
    let changed = replace(&original, &[("wait_stats_capture_mode_desc", Val::Text("OFF".into()))]);
    assert_eq!(window.verify(&changed).unwrap(), WindowVerification::WaitCaptureChanged);
    assert!(window.retained);
    assert_eq!(window.capture, WaitCaptureState::Disabled);
    assert_eq!(window.end, original.str("window_end"));
    assert_eq!(window.verify(&changed).unwrap(), WindowVerification::Unchanged);
    let changed = replace(&original, &[("native_intervals", Val::Text("<intervals><interval id=\"2\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/></intervals>".into()))]);
    assert_eq!(window.verify(&changed).unwrap(), WindowVerification::RuntimePopulationChanged);
    assert!(!window.retained && window.end.is_none() && window.hours == 0.0);
    let Parameter::Text(scope) = window.parameter() else { panic!("bound native scope") };
    assert!(scope.contains("id=\"1\""));
    let (_, parameters) = window.verify_query();
    assert_eq!(parameters.len(), 1, "verification binds membership, not an earlier endpoint clock");
}

#[test]
fn no_native_intervals_is_not_an_invented_observation_period_or_measured_wait_zero() {
    let original = row();
    let empty = replace(
        &original,
        &[
            ("native_intervals", Val::Text("<intervals/>".into())),
            ("window_start", Val::Null),
            ("window_end", Val::Null),
            ("window_hours", Val::Null),
        ],
    );
    let window = FrozenWindow::decode(&empty, 60, true).unwrap();
    assert!(window.start.is_none() && window.end.is_none() && window.hours == 0.0);
    assert_eq!(
        FrozenWindow::decode(&original, 60, true).unwrap().verify(&empty).unwrap(),
        WindowVerification::RuntimePopulationChanged
    );
    let unsupported = replace(&original, &[("wait_stats_capture_mode_desc", Val::Null)]);
    assert_eq!(FrozenWindow::decode(&unsupported, 60, false).unwrap().capture, WaitCaptureState::Unavailable);
    assert!(FrozenWindow::decode(&unsupported, 60, true).is_err());
    assert!(!metadata_sql(false, false).contains("o.wait_stats_capture_mode_desc"));
}

#[test]
fn partial_interval_endpoint_and_hours_retain_native_precision_without_rounding_into_the_future() {
    let partial = open_row("2020-01-01T06:37:34.9281369Z", 2254.9281369);
    let window = FrozenWindow::decode(&partial, 60, true).unwrap();
    assert_eq!(window.end.as_deref(), Some("2020-01-01T06:37:34.9281369Z"));
    assert!((window.hours - 2_254_928_136_900.0 / 3_600_000_000_000.0).abs() < f64::EPSILON);
    let truncated = replace(&partial, &[("window_hours", Val::Float(0.626388))]);
    assert!(FrozenWindow::decode(&truncated, 60, true).is_err());
    let rounded_forward = replace(
        &partial,
        &[("window_end", Val::Text("2020-01-01T06:37:35Z".into())), ("window_hours", Val::Float(2255.0 / 3600.0))],
    );
    assert!(FrozenWindow::decode(&rounded_forward, 60, true).is_err());
    assert!(metadata_sql(true, false).contains("DATEDIFF_BIG(nanosecond"));
    assert!(!metadata_sql(true, false).contains("datetime2(0)"));
}

#[test]
fn final_native_endpoint_advances_without_changing_membership_or_extending_into_an_unselected_interval() {
    let first = open_row("2020-01-01T06:00:10Z", 10.0);
    let mut window = FrozenWindow::decode(&first, 60, true).unwrap();
    let membership = window.native_xml.clone();
    assert_eq!(window.verify(&open_row("2020-01-01T06:00:20Z", 20.0)).unwrap(), WindowVerification::Unchanged);
    assert_eq!(window.end.as_deref(), Some("2020-01-01T06:00:20Z"));
    assert_eq!(window.native_xml, membership);
    let closed = replace(&row(), &[("window_clock", Val::Text("2020-01-01T07:01:00Z".into()))]);
    assert_eq!(window.verify(&closed).unwrap(), WindowVerification::Unchanged);
    assert_eq!(window.end.as_deref(), Some("2020-01-01T07:00:00Z"));
    assert_eq!(window.native_xml, membership);
    let missing = replace(&closed, &[("intervals_retained", Val::Int(0))]);
    assert_eq!(window.verify(&missing).unwrap(), WindowVerification::RuntimePopulationChanged);
    assert!(!window.retained && window.end.is_none() && window.hours == 0.0);
}

#[test]
fn gaps_and_overlapping_intervals_cannot_authorize_a_continuous_denominator() {
    for second_start in ["2020-01-01T07:30:00Z", "2020-01-01T06:30:00Z"] {
        let xml = format!(
            "<intervals><interval id=\"20\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/><interval id=\"1\" starts=\"{second_start}\" ends=\"2020-01-01T08:00:00Z\"/></intervals>"
        );
        let changed = replace(
            &row(),
            &[
                ("window_clock", Val::Text("2020-01-01T09:00:00Z".into())),
                ("native_intervals", Val::Text(xml)),
                ("window_end", Val::Text("2020-01-01T08:00:00Z".into())),
                ("window_hours", Val::Float(2.0)),
            ],
        );
        let error = FrozenWindow::decode(&changed, 120, true).err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    }
    let contiguous = replace(&row(), &[
        ("window_clock", Val::Text("2020-01-01T09:00:00Z".into())),
        ("native_intervals", Val::Text("<intervals><interval id=\"20\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/><interval id=\"1\" starts=\"2020-01-01T07:00:00Z\" ends=\"2020-01-01T08:00:00Z\"/></intervals>".into())),
        ("window_end", Val::Text("2020-01-01T08:00:00Z".into())),
        ("window_hours", Val::Float(2.0)),
    ]);
    assert_eq!(FrozenWindow::decode(&contiguous, 120, true).unwrap().hours, 2.0, "native IDs do not order time");
}

#[test]
fn retained_membership_is_required_and_a_regressed_clock_retires_cached_bounds() {
    let original = row();
    for value in [Val::Null, Val::Int(-1), Val::Int(2), Val::Float(1.0)] {
        assert!(FrozenWindow::decode(&replace(&original, &[("intervals_retained", value)]), 60, true).is_err());
    }
    let missing = Rec::of(
        &original
            .columns()
            .filter(|(name, _)| *name != "intervals_retained")
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    );
    assert!(FrozenWindow::decode(&missing, 60, true).is_err());
    let mut window = FrozenWindow::decode(&original, 60, true).unwrap();
    assert_eq!(
        window.verify(&open_row("2020-01-01T06:59:59.9000000Z", 3599.9)).unwrap(),
        WindowVerification::RuntimePopulationChanged
    );
    assert!(!window.retained && window.end.is_none() && window.hours == 0.0);
}

#[test]
fn malformed_duplicated_or_overflowing_interval_membership_cannot_authorize_native_reads() {
    let original = row();
    for xml in [
        "<wrong/>",
        "<!DOCTYPE intervals [<!ENTITY x 'invalid'>]><intervals>&x;</intervals>",
        "<intervals><interval id=\"1\" starts=\"bad\" ends=\"2020-01-01T07:00:00Z\"/></intervals>",
        "<intervals><interval id=\"1\" starts=\"2020-01-02T06:00:00Z\" ends=\"2020-01-02T07:00:00Z\"/></intervals>",
        "<intervals><interval id=\"1\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/><interval id=\"1\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/></intervals>",
    ] {
        assert!(
            FrozenWindow::decode(&replace(&original, &[("native_intervals", Val::Text(xml.into()))]), 60, true)
                .is_err()
        );
    }
    let mut xml = String::from("<intervals>");
    for id in 0..=MAX_INTERVALS {
        xml.push_str(&format!("<interval id=\"{id}\" starts=\"2020-01-01T06:00:00Z\" ends=\"2020-01-01T07:00:00Z\"/>"));
    }
    xml.push_str("</intervals>");
    let error =
        FrozenWindow::decode(&replace(&original, &[("native_intervals", Val::Text(xml))]), 60, true).err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert!(FrozenWindow::decode(&replace(&original, &[("window_hours", Val::Float(2.0))]), 60, true).is_err());
}
