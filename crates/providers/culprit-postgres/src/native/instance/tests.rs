use super::*;

struct Reader {
    reply: Result<Vec<Row>>,
    calls: Vec<(String, usize)>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls.push((sql.into(), limit));
        std::mem::replace(&mut self.reply, Err(invalid()))
    }
}
fn row(address: Option<&str>, port: &str, system: &str) -> Row {
    Row::from_fields([
        ("address".into(), address.map(str::to_owned)),
        ("port".into(), Some(port.into())),
        ("system_identifier".into(), Some(system.into())),
        ("observed_at".into(), Some("2020-01-01T06:02:00Z".into())),
    ])
}
async fn identity(address: Option<&str>, port: &str, system: &str) -> Result<SharedResourceIdentity> {
    let mut reader = Reader { reply: Ok(vec![row(address, port, system)]), calls: vec![] };
    let result = read(&mut reader, 180_000).await;
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].1, 2);
    assert!(reader.calls[0].0.contains("pg_catalog.pg_control_system()"));
    assert!(reader.calls[0].0.contains("pg_catalog.inet_server_addr()"));
    result.map(|instance| instance.identity)
}

#[tokio::test]
async fn the_native_system_and_node_are_required_without_a_database_or_connection_alias() {
    let first = identity(Some("2001:0db8:0:0:0:0:0:1"), "5432", "9223372036854775807").await.unwrap();
    let canonical = identity(Some("2001:db8::1"), "5432", "9223372036854775807").await.unwrap();
    assert_eq!(first, canonical);
    assert_ne!(first, identity(Some("2001:db8::2"), "5432", "9223372036854775807").await.unwrap());
    assert_ne!(first, identity(Some("2001:db8::1"), "5432", "42").await.unwrap());
    assert_eq!(first.kind, SharedResourceKind::Instance);
    for (address, port, system) in [
        (None, "5432", "42"),
        (Some("fixture.invalid"), "5432", "42"),
        (Some("127.0.0.1"), "0", "42"),
        (Some("127.0.0.1"), "65536", "42"),
        (Some("127.0.0.1"), "5432", "0"),
        (Some("127.0.0.1"), "5432", "18446744073709551615"),
    ] {
        assert!(identity(address, port, system).await.is_err());
    }
    assert_eq!(
        identity(Some("127.0.0.1"), "5432", "-9223372036854775808").await.unwrap().namespace.id,
        "-9223372036854775808"
    );
    let mut reader = Reader {
        reply: Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied")),
        calls: vec![],
    };
    assert_eq!(read(&mut reader, 180_000).await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn an_unsupported_version_never_sends_control_function_sql() {
    let mut reader = Reader { reply: Ok(vec![]), calls: vec![] };
    assert_eq!(read(&mut reader, 90_600).await.err().unwrap().code, culprit_core::ErrorCode::Unsupported);
    assert!(reader.calls.is_empty());
    for rows in [vec![], vec![row(Some("127.0.0.1"), "5432", "42"), row(Some("127.0.0.1"), "5432", "42")]] {
        let mut reader = Reader { reply: Ok(rows), calls: vec![] };
        assert!(read(&mut reader, 180_000).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
