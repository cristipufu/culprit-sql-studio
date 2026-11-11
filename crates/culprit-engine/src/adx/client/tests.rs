use super::*;
use std::io::Write;

fn request() -> QueryRequest {
    QueryRequest {
        text: "declare query_parameters(batch:string); Receipts | where BatchId == batch | project RecordId".into(),
        parameters: BTreeMap::from([("batch".into(), json!("quoted \" value\nwith newline"))]),
        columns: vec![Column { column_name: "RecordId".into(), column_type: "string".into() }],
        row_limit: 1000,
    }
}
#[test]
fn query_properties_keep_verification_strong_read_only_bounded_and_uncached() {
    let query = request();
    let bytes = query.encode("db \" name", "Culprit.Delivery;test").unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["db"], "db \" name");
    assert_eq!(body["csl"], query.text);
    let properties: Value = serde_json::from_str(body["properties"].as_str().unwrap()).unwrap();
    assert_eq!(properties["Parameters"]["batch"], query.parameters["batch"]);
    let options = &properties["Options"];
    assert_eq!(options["queryconsistency"], "strongconsistency");
    assert_eq!(options["query_results_cache_max_age"], "00:00:00");
    assert_eq!(options["request_readonly_hardline"], true);
    assert_eq!(options["request_callout_disabled"], true);
    assert_eq!(options["results_progressive_enabled"], false);
    assert_eq!(options["truncationmaxsize"], response::MAX_RESPONSE_BYTES);
    assert_eq!(options["servertimeout"], "00:00:30");
}
#[test]
fn metadata_does_not_turn_a_cluster_address_into_an_assumed_token_audience() {
    let origin = "https://cluster.westeurope.kusto.windows.net";
    let metadata = br#"{"AzureAD":{"KustoServiceResourceId":"https://kusto.kusto.windows.net"}}"#;
    assert_eq!(http::audience(origin, metadata).unwrap().resource(), "https://kusto.kusto.windows.net");
    for value in [
        json!({}),
        json!({"AzureAD":{"KustoServiceResourceId":"https://attacker.example"}}),
        json!({"AzureAD":{"KustoServiceResourceId":"https://other.kusto.windows.net"}}),
    ] {
        assert!(http::audience(origin, &serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(http::audience(origin, &vec![b' '; 65 * 1024]).is_err());
}
#[test]
fn invalid_targets_and_unbounded_or_malformed_parameters_are_rejected_before_authentication() {
    for endpoint in [
        "http://cluster.kusto.windows.net",
        "https://x.kusto.windows.net@attacker.example",
        "https://x.kusto.windows.net/path",
    ] {
        assert!(AdxConfig { endpoint: endpoint.into(), database: "db".into() }.validated().is_err());
    }
    let mut query = request();
    query.row_limit = 0;
    assert!(query.encode("db", "id").is_err());
    let mut query = request();
    query.text = "x".repeat(MAX_QUERY_BYTES + 1);
    assert!(query.encode("db", "id").is_err());
    let mut query = request();
    query.parameters.insert("bad-name".into(), json!(1));
    assert!(query.encode("db", "id").is_err());
    let mut query = request();
    query.parameters.insert("object".into(), json!({"secret":"value"}));
    assert!(query.encode("db", "id").is_err());
    let mut query = request();
    query.parameters.insert("large".into(), json!("x".repeat(MAX_REQUEST_BYTES)));
    assert!(query.encode("db", "id").is_err());
}
#[test]
fn query_http_checks_native_completion_and_never_exposes_service_error_bodies() {
    for (status, has_errors, success) in [(200, false, true), (200, true, false), (429, false, false)] {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let body = json!([
            {"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false},
            {"FrameType":"DataTable","TableId":0,"TableKind":"PrimaryResult","Columns":[{"ColumnName":"RecordId","ColumnType":"string"}],"Rows":[["native"]]},
            {"FrameType":"DataSetCompletion","HasErrors":has_errors,"Cancelled":false}
        ]).to_string();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = crate::http_fixture::request(&mut socket).unwrap();
            let body = if status == 200 { body } else { "private-service-error-canary".into() };
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        let request = request();
        let result = http::query(
            &origin,
            "synthetic-token",
            "Culprit.Delivery;test",
            request.encode("db", "id").unwrap(),
            &request.columns,
            request.row_limit,
        );
        assert_eq!(result.is_ok(), success);
        if let Err(error) = result {
            assert!(!format!("{error:?}").contains("private-service-error-canary"));
        }
        let sent = server.join().unwrap().to_ascii_lowercase();
        assert!(sent.starts_with("post /v2/rest/query "));
        assert!(sent.contains("x-ms-readonly: true"));
    }
}
