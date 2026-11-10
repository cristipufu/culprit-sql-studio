use super::*;
use crate::{Engine, Environment};

fn spec(engine: Engine) -> ConnectionSpec {
    ConnectionSpec {
        engine: Some(engine),
        host: "db.example.com".into(),
        environment: Environment::Production,
        ..Default::default()
    }
}

#[test]
fn every_provider_describes_its_security_and_defaults() {
    for engine in Engine::ALL {
        let mut remote = spec(engine);
        let Some(facts) = engine.spec().connection_security() else {
            assert!(!engine.has_server());
            assert!(connection_security(&remote).is_none());
            continue;
        };
        let info = connection_security(&remote).unwrap();
        let default = facts.mode(&info.effective_mode).unwrap();
        assert!(default.verifies_identity && !default.allows_plaintext, "{engine:?}");
        remote.host = "localhost".into();
        assert_eq!(connection_security(&remote).unwrap().default_mode, facts.local_default_mode);
        for mode in &facts.modes {
            assert_eq!(facts.mode(&mode.value.to_ascii_lowercase()).unwrap().value, mode.value);
        }
    }
}

#[test]
fn explicit_settings_replace_all_aliases_and_preserve_unrelated_options() {
    let mut connection = spec(Engine::Mysql);
    connection.options.extend([
        ("sslmode".into(), "VERIFY_IDENTITY".into()),
        ("SSL-Mode".into(), "REQUIRED".into()),
        ("sslrootcert".into(), "old.pem".into()),
        ("region".into(), "eu-west-1".into()),
    ]);
    assert_eq!(connection_security(&connection).unwrap().settings.mode.as_deref(), Some("VERIFY_IDENTITY"));
    apply_connection_security(
        &mut connection,
        &ConnectionSecurity { mode: Some("disable".into()), ..Default::default() },
    )
    .unwrap();
    assert_eq!(connection.option("ssl-mode"), Some("DISABLED"));
    assert_eq!(connection.options.len(), 2);
    assert_eq!(connection.option("region"), Some("eu-west-1"));
    apply_connection_security(&mut connection, &ConnectionSecurity::default()).unwrap();
    assert_eq!(connection.options.len(), 1);
    assert_eq!(connection_security(&connection).unwrap().effective_mode, "VERIFY_IDENTITY");
}

#[test]
fn token_authentication_refuses_a_downgrade_before_mutating_options() {
    for engine in [Engine::Mssql, Engine::Postgres, Engine::Mysql] {
        let mut connection = spec(engine);
        connection.auth = AuthKind::Entra;
        let original = connection.options.clone();
        let facts = engine.spec().connection_security().unwrap();
        let insecure = facts.modes.iter().find(|mode| mode.allows_plaintext).unwrap();
        let error = apply_connection_security(
            &mut connection,
            &ConnectionSecurity { mode: Some(insecure.value.clone()), ..Default::default() },
        )
        .unwrap_err();
        assert!(error.message.contains("requires encryption"));
        assert_eq!(connection.options, original);
        connection.host = "localhost".into();
        assert_eq!(connection_security(&connection).unwrap().default_mode, facts.default_mode);
    }
}

#[test]
fn strict_sql_server_cannot_skip_certificate_verification() {
    let mut connection = spec(Engine::Mssql);
    assert!(
        apply_connection_security(
            &mut connection,
            &ConnectionSecurity { mode: Some("strict".into()), trust_server_certificate: true, ..Default::default() }
        )
        .is_err()
    );
    apply_connection_security(
        &mut connection,
        &ConnectionSecurity {
            mode: Some("true".into()),
            trust_server_certificate: true,
            ca_certificate: Some("company.pem".into()),
            certificate_hostname: Some("db.internal".into()),
        },
    )
    .unwrap();
    assert_eq!(connection.option("trustServerCertificate"), Some("true"));
    assert_eq!(connection_security(&connection).unwrap().settings.certificate_hostname.as_deref(), Some("db.internal"));
}

#[test]
fn unrecognized_saved_modes_remain_visible_and_can_be_repaired() {
    let mut connection = spec(Engine::Postgres);
    connection.options.insert("sslmode".into(), "unsupported".into());
    assert_eq!(connection_security(&connection).unwrap().settings.mode.as_deref(), Some("unsupported"));
    assert!(
        apply_connection_security(
            &mut connection,
            &ConnectionSecurity { mode: Some("unsupported".into()), ..Default::default() }
        )
        .is_err()
    );
    apply_connection_security(&mut connection, &ConnectionSecurity::default()).unwrap();
    assert_eq!(connection_security(&connection).unwrap().effective_mode, "verify-full");
}

#[test]
fn file_connections_refuse_network_security_settings() {
    let mut connection = spec(Engine::Sqlite);
    assert!(apply_connection_security(&mut connection, &ConnectionSecurity::default()).is_ok());
    assert!(
        apply_connection_security(
            &mut connection,
            &ConnectionSecurity { mode: Some("disabled".into()), ..Default::default() }
        )
        .is_err()
    );
}

#[test]
fn imported_trust_flags_use_the_same_boolean_semantics_as_the_driver() {
    let mut connection = spec(Engine::Mssql);
    for value in ["true", "YES", "1", "on", "mandatory", "strict"] {
        connection.options.insert("trustServerCertificate".into(), value.into());
        assert!(connection_security(&connection).unwrap().settings.trust_server_certificate, "{value}");
    }
    for value in ["false", "no", "0", "unsupported"] {
        connection.options.insert("trustServerCertificate".into(), value.into());
        assert!(!connection_security(&connection).unwrap().settings.trust_server_certificate, "{value}");
    }
}
