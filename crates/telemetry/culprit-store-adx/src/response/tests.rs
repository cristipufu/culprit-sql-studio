use super::*;
use serde_json::json;

fn reply() -> Value {
    json!([
        {"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false},
        {"FrameType":"DataTable","TableId":0,"TableKind":"PrimaryResult","TableName":"Result",
         "Columns":[{"ColumnName":"RecordId","ColumnType":"string"},{"ColumnName":"Count","ColumnType":"long"}],
         "Rows":[["native",9007199254740993u64]]},
        {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}
    ])
}
fn schema() -> Vec<Column> {
    vec![
        Column { column_name: "RecordId".into(), column_type: "string".into() },
        Column { column_name: "Count".into(), column_type: "long".into() },
    ]
}
fn decode(value: &Value) -> Result<QueryTable> {
    parse(&serde_json::to_vec(value).unwrap(), &schema(), 1000, MAX_RESPONSE_BYTES)
}

#[test]
fn complete_replies_preserve_exact_integer_values_and_explicit_empty_results() {
    let table = decode(&reply()).unwrap();
    assert_eq!(table.response_bytes, serde_json::to_vec(&reply()).unwrap().len());
    assert_eq!(table.rows[0][1].as_u64(), Some(9_007_199_254_740_993));
    let mut empty = reply();
    empty[1]["Rows"] = json!([]);
    assert!(decode(&empty).unwrap().rows.is_empty());
}

#[test]
fn a_per_request_wire_limit_includes_metadata_and_completion_frames() {
    let bytes = serde_json::to_vec(&reply()).unwrap();
    assert!(parse(&bytes, &schema(), 1000, bytes.len()).is_ok());
    assert!(parse(&bytes, &schema(), 1000, bytes.len() - 1).is_err());
    assert!(parse(&bytes, &schema(), 1000, 0).is_err());
}
#[test]
fn partial_rows_and_http_success_cannot_hide_cancellation_or_late_failure() {
    for field in ["HasErrors", "Cancelled"] {
        let mut value = reply();
        value[2][field] = json!(true);
        assert!(decode(&value).is_err());
    }
    let mut value = reply();
    value[2]["OneApiErrors"] = json!([{"message":"secret source query"}]);
    let error = decode(&value).err().unwrap();
    assert!(!format!("{error:?}").contains("secret source query"));
    let mut value = reply();
    value.as_array_mut().unwrap().pop();
    assert!(decode(&value).is_err());
    let mut value = reply();
    value[2].as_object_mut().unwrap().remove("Cancelled");
    assert!(decode(&value).is_err());
    assert!(parse(br#"{"error":{"message":"secret"}}"#, &schema(), 1000, MAX_RESPONSE_BYTES).is_err());
}
#[test]
fn ambiguous_tables_wrong_projections_and_progressive_responses_are_refused() {
    let mut value = reply();
    value[0]["IsProgressive"] = json!(true);
    assert!(decode(&value).is_err());
    let mut value = reply();
    value[0]["Version"] = json!("v3.0");
    assert!(decode(&value).is_err());
    let mut value = reply();
    value[1]["Columns"][1]["ColumnType"] = json!("real");
    assert!(decode(&value).is_err());
    let mut value = reply();
    value[1]["Columns"][1]["ColumnName"] = json!("RecordId");
    assert!(decode(&value).is_err());
    let mut value = reply();
    value[1]["Rows"][0] = json!(["native"]);
    assert!(decode(&value).is_err());
    let mut value = reply();
    let duplicate = value[1].clone();
    value.as_array_mut().unwrap().insert(2, duplicate);
    assert!(decode(&value).is_err());
    let mut value = reply();
    let mut duplicate = value[1].clone();
    duplicate["TableId"] = json!(1);
    value.as_array_mut().unwrap().insert(2, duplicate);
    assert!(decode(&value).is_err());
    let mut value = reply();
    value.as_array_mut().unwrap().push(json!({"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}));
    assert!(decode(&value).is_err());
}
#[test]
fn response_and_row_budgets_are_enforced_before_receipts_can_be_used() {
    assert!(parse(&vec![b' '; MAX_RESPONSE_BYTES + 1], &schema(), 1, MAX_RESPONSE_BYTES).is_err());
    let mut value = reply();
    value[1]["Rows"] = json!([["one", 1], ["two", 2]]);
    assert!(parse(&serde_json::to_vec(&value).unwrap(), &schema(), 1, MAX_RESPONSE_BYTES).is_err());
    assert!(parse(&serde_json::to_vec(&reply()).unwrap(), &schema(), 0, MAX_RESPONSE_BYTES).is_err());
}
