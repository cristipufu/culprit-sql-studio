//! Saved routing changes invalidate a prior native binding without resolving credential values.
use super::*;
use serde::Serialize;

#[derive(Serialize)]
struct RouteMetadata<'a> {
    engine: Engine,
    environment: Environment,
    host: &'a str,
    port: Option<u16>,
    database: Option<&'a str>,
    user: Option<&'a str>,
    auth: AuthKind,
    password_reference: Option<&'a str>,
    password_saved: Option<bool>,
    options: &'a std::collections::BTreeMap<String, String>,
    ssh: Option<&'a culprit_core::SshConfig>,
}

pub(super) fn revision(stored: &StoredConnection) -> Result<String> {
    let metadata = RouteMetadata {
        engine: stored.engine,
        environment: stored.environment,
        host: &stored.host,
        port: stored.port,
        database: stored.database.as_deref(),
        user: stored.user.as_deref(),
        auth: stored.auth,
        password_reference: stored.password_env.as_deref(),
        password_saved: stored.password_saved,
        options: &stored.options,
        ssh: stored.ssh.as_ref(),
    };
    serde_json::to_vec(&("saved-source-route-v1", metadata))
        .map(|bytes| culprit_core::telemetry::digest(&bytes))
        .map_err(|_| culprit_core::telemetry::CollectionStop::InvalidPage.error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> StoredConnection {
        StoredConnection {
            engine: Engine::Postgres,
            environment: Environment::Production,
            host: "route.fixture.invalid".into(),
            port: Some(5432),
            database: Some("orders".into()),
            user: Some("reader".into()),
            auth: AuthKind::Entra,
            password_env: None,
            password_saved: Some(false),
            options: [("sslmode".into(), "verify-full".into())].into(),
            ssh: Some(culprit_core::SshConfig {
                destination: "reader@bastion.invalid".into(),
                port: Some(22),
                identity_file: Some("/not-an-actual-private-key".into()),
            }),
            agent: None,
            query_log: None,
            no_query_log: false,
        }
    }
    #[test]
    fn every_saved_native_routing_and_credential_reference_field_changes_the_binding() {
        let original = saved();
        let expected = revision(&original).unwrap();
        assert!(culprit_core::telemetry::valid_digest(&expected));
        for field in 0..14 {
            let mut changed = original.clone();
            match field {
                0 => changed.engine = Engine::Mysql,
                1 => changed.environment = Environment::Development,
                2 => changed.host = "replacement.fixture.invalid".into(),
                3 => changed.port = Some(6432),
                4 => changed.database = Some("replacement".into()),
                5 => changed.user = Some("another-reader".into()),
                6 => changed.auth = AuthKind::Password,
                7 => changed.password_env = Some("UNRESOLVED_CREDENTIAL_REFERENCE".into()),
                8 => changed.password_saved = Some(true),
                9 => {
                    changed.options.insert("sslmode".into(), "require".into());
                }
                10 => changed.ssh.as_mut().unwrap().destination = "replacement@bastion.invalid".into(),
                11 => changed.ssh.as_mut().unwrap().port = Some(2222),
                12 => changed.ssh.as_mut().unwrap().identity_file = Some("/a-different-unread-private-key".into()),
                _ => changed.ssh = None,
            }
            assert_ne!(revision(&changed).unwrap(), expected, "field {field}");
        }
    }
    #[test]
    fn incidental_log_preferences_and_option_insertion_order_do_not_replace_a_native_route() {
        let mut original = saved();
        original.options.insert("application_name".into(), "culprit-route-fixture".into());
        let expected = revision(&original).unwrap();
        let mut changed = original.clone();
        changed.query_log = Some("/private/local/querylog.jsonl".into());
        changed.no_query_log = true;
        changed.options = original.options.iter().rev().map(|(key, value)| (key.clone(), value.clone())).collect();
        assert_eq!(revision(&changed).unwrap(), expected);
        assert_eq!(revision(&original).unwrap(), expected);
    }
}
