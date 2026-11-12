use super::*;
use serde_json::json;
fn columns() -> Vec<Column> {
    vec![Column { name: "Exact".into(), kind: "String".into() }]
}
#[test]
fn typed_complete_rows_are_required_even_with_http_success() {
    let value =
        json!({"meta":[{"name":"Exact","type":"String"}],"data":[["18446744073709551615"]],"rows":1,"statistics":{}});
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(parse(&bytes, &columns(), 1).unwrap().rows, vec![vec![json!("18446744073709551615")]]);
    for bad in [
        json!({"meta":[{"name":"Exact","type":"Float64"}],"data":[[1]],"rows":1}),
        json!({"meta":[{"name":"Exact","type":"String"}],"data":[[]],"rows":1}),
        json!({"meta":[{"name":"Exact","type":"String"}],"data":[["1"]],"rows":2}),
        json!({"meta":[{"name":"Exact","type":"String"}],"data":[["1"]],"rows":1,"exception":"private-error"}),
    ] {
        assert!(parse(&serde_json::to_vec(&bad).unwrap(), &columns(), 1).is_err());
    }
    let mut late = bytes.clone();
    late.extend_from_slice(b"\nCode: late database exception");
    assert!(parse(&late, &columns(), 1).is_err());
    assert!(parse(&bytes[..bytes.len() - 2], &columns(), 1).is_err());
    assert!(parse(&vec![b' '; MAX_RESPONSE_BYTES + 1], &columns(), 1).is_err());
    assert!(parse(&bytes, &columns(), 0).is_err());
}
