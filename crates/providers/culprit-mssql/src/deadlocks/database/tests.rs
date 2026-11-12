use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const FROM: &str = "2020-01-01T06:00:00Z";
const UNTIL: &str = "2020-01-01T07:00:00Z";

pub(crate) fn configuration() -> Rec {
    Rec::of(&[
        ("event_session_id", Val::Int(4)),
        ("generation", Val::Text("2020-01-01T05:00:00.153".into())),
        ("memory_kib", Val::Int(1024)),
        ("maximum_events", Val::Int(256)),
    ])
}

pub(crate) fn event(sequence: u64) -> String {
    format!(
        r#"<event name="database_xml_deadlock_report" timestamp="2020-01-01T06:00:30.1533333Z"><action name="event_sequence"><value>{sequence}</value></action><data name="xml_report"><value><deadlock><victim-list><victimProcess id="p1"/></victim-list><process-list><process id="p1" spid="20" loginname="private-user" hostname="private-host"><inputbuf>SELECT N'private-deadlock-canary'</inputbuf></process><process id="p2" spid="21"><inputbuf>SELECT 1</inputbuf></process></process-list><resource-list><keylock id="r1" objectname="dbo.orders"><owner-list><owner id="p2" mode="X"/></owner-list><waiter-list><waiter id="p1" mode="S"/></waiter-list></keylock></resource-list></deadlock></value></data></event>"#
    )
}

pub(crate) fn target(events: &[String]) -> Rec {
    target_xml(&format!(
        r#"<RingBufferTarget truncated="0" eventCount="{}">{}</RingBufferTarget>"#,
        events.len(),
        events.join("")
    ))
}

fn target_xml(xml: &str) -> Rec {
    Rec::of(&[("target_bytes", Val::Int(xml.encode_utf16().count() as i64 * 2)), ("target_xml", Val::Text(xml.into()))])
}

async fn capture(io: &mut FakeRead) -> Result<Vec<Event>> {
    read(io, "db", FROM, UNTIL, 257, false).await
}

#[tokio::test]
async fn explicit_bounded_session_keeps_fractional_time_and_exact_event_identity() {
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target(&[event(9007199254740993)])])]);
    let events = capture(&mut io).await.unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.at, "2020-01-01T06:00:30.153333300Z");
    assert_eq!(event.offset, "9007199254740993");
    assert_eq!(event.position.len(), 64);
    assert_eq!(event.graphs.len(), 1);
    assert_eq!(event.graphs[0].at.as_deref(), Some(event.at.as_str()));
    assert_eq!(event.graphs[0].processes[0].session.as_deref(), Some("20"));
    assert_eq!(io.calls.len(), 2);
    assert!(matches!(&io.calls[1].parameters[2],Parameter::Text(value) if value=="2020-01-01T05:00:00.153"));
    for call in &io.calls {
        assert!(!call.sql.contains("fn_xe_telemetry"));
        assert!(!call.sql.contains("fn_xe_file_target_read_file"));
    }
}

#[tokio::test]
async fn missing_or_unbounded_session_never_reads_target_data() {
    let configurations = [
        vec![],
        vec![replace(&configuration(), &[("memory_kib", Val::Int(1025))])],
        vec![replace(&configuration(), &[("memory_kib", Val::Int(0))])],
        vec![replace(&configuration(), &[("maximum_events", Val::Int(257))])],
        vec![replace(&configuration(), &[("maximum_events", Val::Int(0))])],
        vec![configuration(), configuration()],
    ];
    for configuration in configurations {
        let mut io = FakeRead::new(vec![Ok(configuration)]);
        assert_eq!(capture(&mut io).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(io.calls.len(), 1);
        assert!(!io.calls[0].sql.contains("target_data"));
    }
}

#[tokio::test]
async fn empty_confirmed_buffer_is_distinct_from_a_disappearing_session() {
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target(&[])])]);
    assert!(capture(&mut io).await.unwrap().is_empty());
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![])]);
    assert_eq!(CollectionStop::from_error(&capture(&mut io).await.err().unwrap()), CollectionStop::SourceChanged);
}

#[tokio::test]
async fn denied_or_failed_metadata_never_falls_back_to_internal_or_instance_readers() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::Timeout, ErrorCode::QueryFailed] {
        let mut io = FakeRead::new(vec![Err(ReadError::Source(Error::new(code, "fixture")))]);
        assert_eq!(capture(&mut io).await.err().unwrap().code, code);
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn oversized_or_truncated_buffer_cannot_become_complete_empty_evidence() {
    let oversized = Rec::of(&[("target_bytes", Val::Int(MAX_XML_BYTES + 1)), ("target_xml", Val::Null)]);
    let truncated = target_xml(r#"<RingBufferTarget truncated="1" eventCount="0"/>"#);
    for target in [oversized, truncated] {
        let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target])]);
        assert_eq!(CollectionStop::from_error(&capture(&mut io).await.err().unwrap()), CollectionStop::ByteLimit);
    }
}

#[test]
fn malformed_missing_or_ambiguous_event_membership_is_rejected() {
    let duplicate =
        format!(r#"<RingBufferTarget truncated="0" eventCount="2">{}{}</RingBufferTarget>"#, event(1), event(1));
    let missing_sequence = event(1).replace(r#"<action name="event_sequence"><value>1</value></action>"#, "");
    let invalid_graph = event(1).replace("<process-list>", "<unreadable>").replace("</process-list>", "</unreadable>");
    for xml in [
        "not xml".into(),
        r#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///private">]><RingBufferTarget truncated="0" eventCount="0">&e;</RingBufferTarget>"#.into(),
        r#"<RingBufferTarget truncated="0" eventCount="1"/>"#.into(),
        r#"<RingBufferTarget eventCount="0"/>"#.into(),
        duplicate,
        format!(r#"<RingBufferTarget truncated="0" eventCount="1">{missing_sequence}</RingBufferTarget>"#),
        format!(r#"<RingBufferTarget truncated="0" eventCount="1">{invalid_graph}</RingBufferTarget>"#),
    ] {
        assert!(decode(&xml, "db", "generation", FROM, UNTIL, 256).is_err(), "{xml}");
    }
}

#[tokio::test]
async fn session_generation_changes_identity_and_same_time_sequences_sort_numerically() {
    let events = vec![event(10), event(2)];
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target(&events)])]);
    let original = capture(&mut io).await.unwrap();
    assert_eq!(original.iter().map(|event| event.offset.as_str()).collect::<Vec<_>>(), ["2", "10"]);
    let configuration = replace(&configuration(), &[("generation", Val::Text("2020-01-01T05:01:00.153".into()))]);
    let mut io = FakeRead::new(vec![Ok(vec![configuration]), Ok(vec![target(&events)])]);
    let restarted = capture(&mut io).await.unwrap();
    assert_ne!(original[0].file_id, restarted[0].file_id);
    assert_ne!(original[0].position, restarted[0].position);
}

#[tokio::test]
async fn selected_window_is_half_open_and_latest_selection_is_explicit() {
    let events = [
        event(1).replace("06:00:30.1533333Z", "06:00:00Z"),
        event(2),
        event(3).replace("06:00:30.1533333Z", "07:00:00Z"),
    ];
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target(&events)])]);
    let retained = capture(&mut io).await.unwrap();
    assert_eq!(retained.iter().map(|event| event.offset.as_str()).collect::<Vec<_>>(), ["1", "2"]);
    let mut io = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![target(&events)])]);
    let latest = read(&mut io, "db", FROM, UNTIL, 1, true).await.unwrap();
    assert_eq!(latest[0].offset, "2");
}
