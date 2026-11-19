use super::*;
use crate::value::Val;

#[test]
fn native_payload_projection_delimits_reserved_and_embedded_delimiter_aliases() {
    let selected = columns("qp.query_plan", "plan_bytes", "plan", "@P3");
    assert!(selected.contains("CAST(DATALENGTH(qp.query_plan) AS bigint) AS [plan_bytes]"));
    assert!(selected.contains("WHEN DATALENGTH(qp.query_plan)<=@P3 THEN qp.query_plan END AS [plan]"));
    let escaped = columns("payload.body", "bytes]count", "select]body", "@P2");
    assert!(escaped.contains("AS [bytes]]count]"));
    assert!(escaped.contains("END AS [select]]body]"));
}

#[test]
fn delimited_projection_keeps_original_decoder_keys_and_native_utf16_length_limits() {
    let text = "A🧪";
    let native_bytes = text.encode_utf16().count() * 2;
    assert_ne!(native_bytes, text.len());
    let row = Rec::of(&[("plan_bytes", Val::Int(native_bytes as i64)), ("plan", Val::Text(text.into()))]);
    let (body, bytes) = read(&row, "plan_bytes", "plan", native_bytes).unwrap();
    assert_eq!(body.text(), Some(text));
    assert_eq!(bytes, Some(native_bytes));
    let oversized = Rec::of(&[("plan_bytes", Val::Int(native_bytes as i64)), ("plan", Val::Null)]);
    let (body, bytes) = read(&oversized, "plan_bytes", "plan", native_bytes - 1).unwrap();
    assert!(matches!(body, Payload::Unavailable(PayloadUnavailable::Oversized)));
    assert_eq!(bytes, Some(native_bytes));
}
