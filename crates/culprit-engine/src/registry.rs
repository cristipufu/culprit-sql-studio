//! Maps an engine to its provider. New engines plug in here behind a cargo feature.

use crate::ProviderRegistration;
use culprit_core::{ConnectionSpec, Engine, Error, ErrorCode, Provider, Result, Source, t};
#[cfg(any(feature = "mssql", feature = "postgres", feature = "yugabyte", feature = "mysql", feature = "sqlite"))]
use std::sync::Arc;

/// Every source a scan reads on `engine`, as its provider declares them (`<provider>/src/sources.rs`); empty for an
/// engine this build has no provider for.
pub fn sources(engine: Engine) -> &'static [Source] {
    match engine {
        #[cfg(feature = "mssql")]
        Engine::Mssql => culprit_mssql::sources::ALL,
        #[cfg(feature = "postgres")]
        Engine::Postgres => culprit_postgres::sources::ALL,
        #[cfg(feature = "yugabyte")]
        Engine::Yugabyte => culprit_yugabyte::sources::ALL,
        #[cfg(feature = "mysql")]
        Engine::Mysql => culprit_mysql::sources::ALL,
        #[cfg(feature = "sqlite")]
        Engine::Sqlite => culprit_sqlite::sources::ALL,
        #[allow(unreachable_patterns)]
        _ => &[],
    }
}

pub fn provider(spec: &ConnectionSpec) -> Result<Box<dyn Provider>> {
    registration(engine(spec)?)?.open(spec)
}

pub(crate) fn engine(spec: &ConnectionSpec) -> Result<Engine> {
    spec.engine.ok_or_else(|| {
        Error::invalid(t!("engine.connection.unknown_engine")).with_hint(t!("engine.registry.unknown_engine_hint"))
    })
}

/// Static source declarations and native construction are selected together; no factory runs while listing.
pub fn registration(engine: Engine) -> Result<ProviderRegistration> {
    match engine {
        #[cfg(feature = "mssql")]
        Engine::Mssql => ProviderRegistration::new(
            Arc::new(|spec| Ok(Box::new(culprit_mssql::MssqlProvider::new(spec)?))),
            culprit_mssql::telemetry_recipes(),
        ),
        #[cfg(feature = "postgres")]
        Engine::Postgres => ProviderRegistration::new(
            Arc::new(|spec| Ok(Box::new(culprit_postgres::PostgresProvider::new(spec)?))),
            culprit_postgres::telemetry_recipes(),
        ),
        #[cfg(feature = "yugabyte")]
        Engine::Yugabyte => ProviderRegistration::new(
            Arc::new(|spec| Ok(Box::new(culprit_yugabyte::YugabyteProvider::new(spec)?))),
            culprit_yugabyte::telemetry_recipes(),
        ),
        #[cfg(feature = "mysql")]
        Engine::Mysql => ProviderRegistration::new(
            Arc::new(|spec| Ok(Box::new(culprit_mysql::MysqlProvider::new(spec)?))),
            culprit_mysql::telemetry_recipes(),
        ),
        #[cfg(feature = "sqlite")]
        Engine::Sqlite => ProviderRegistration::new(
            Arc::new(|spec| Ok(Box::new(culprit_sqlite::SqliteProvider::new(spec)?))),
            culprit_sqlite::telemetry_recipes(),
        ),
        #[allow(unreachable_patterns)]
        other => Err(Error::new(ErrorCode::Unsupported, t!("engine.registry.unsupported", engine = other.label()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_declare_only_the_selected_feature_gated_provider_catalog() {
        for engine in Engine::ALL {
            match registration(engine) {
                Ok(registration) => {
                    assert!(!sources(engine).is_empty());
                    assert!(culprit_core::telemetry::recipe_catalog_is_valid(registration.telemetry_recipes()));
                    #[cfg(feature = "mssql")]
                    if engine == Engine::Mssql {
                        assert_eq!(registration.telemetry_recipes(), culprit_mssql::telemetry_recipes());
                        assert!(!registration.telemetry_recipes().is_empty());
                    }
                    assert!(
                        !registration.telemetry_recipes().is_empty(),
                        "{engine:?} hides implemented native sources"
                    );
                }
                Err(error) => {
                    assert!(sources(engine).is_empty());
                    assert_eq!(error.code, ErrorCode::Unsupported);
                }
            }
        }
    }

    #[test]
    fn every_builtin_registration_uses_its_own_native_dispatch_catalog() {
        type Catalog = fn() -> Vec<culprit_core::telemetry::RecipeDescriptor>;
        let catalogs: &[(Engine, Catalog)] = &[
            #[cfg(feature = "mssql")]
            (Engine::Mssql, culprit_mssql::telemetry_recipes),
            #[cfg(feature = "postgres")]
            (Engine::Postgres, culprit_postgres::telemetry_recipes),
            #[cfg(feature = "yugabyte")]
            (Engine::Yugabyte, culprit_yugabyte::telemetry_recipes),
            #[cfg(feature = "mysql")]
            (Engine::Mysql, culprit_mysql::telemetry_recipes),
            #[cfg(feature = "sqlite")]
            (Engine::Sqlite, culprit_sqlite::telemetry_recipes),
        ];
        for &(engine, catalog) in catalogs {
            assert_eq!(registration(engine).unwrap().telemetry_recipes(), catalog(), "{engine:?}");
        }
    }

    #[test]
    fn unresolved_forwarding_never_falls_back_to_a_direct_builtin_provider() {
        for engine in Engine::ALL {
            let spec = ConnectionSpec {
                engine: Some(engine),
                host: "db.example.com".into(),
                database: Some("shop".into()),
                user: Some("reader".into()),
                ssh: Some(culprit_core::SshConfig { destination: "bastion".into(), port: None, identity_file: None }),
                ..Default::default()
            };
            assert!(provider(&spec).is_err(), "{engine:?} ignored an unresolved SSH route");
        }
    }

    // Each engine's sources are one list anyone can read (`culprit sources`): every engine this build serves has one,
    // and a source's name, which its notes carry, says which one it was.
    #[test]
    fn every_engine_declares_its_sources_once() {
        for engine in Engine::ALL {
            let list = sources(engine);
            if list.is_empty() {
                let spec =
                    ConnectionSpec { engine: Some(engine), host: "fixture.invalid".into(), ..Default::default() };
                let error = provider(&spec).err().expect("a registered provider must declare its native sources");
                assert_eq!(error.code, ErrorCode::Unsupported, "{engine:?} lacks sources for a selectable provider");
                continue;
            }
            let mut names: Vec<&str> = list.iter().map(|s| s.name).collect();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "{engine:?} names two sources the same");
        }
    }

    // What a person reads, word for word: its messages come from locales/en.json.
    #[test]
    fn a_spec_without_an_engine_says_how_to_name_one_in_english() {
        let Err(e) = provider(&ConnectionSpec::default()) else { panic!("no engine, no provider") };
        assert_eq!(e.message, "Can't tell which database engine this is");
        assert_eq!(e.hint.as_deref(), Some("Use a postgres://, mssql://, mysql:// or sqlite:// URL, or pass --engine"));
    }
}
