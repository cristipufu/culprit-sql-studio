//! Configuration values survive sharing; native namespace identifiers are consistently opaque.
use super::{opaque, redact_read_context};
use crate::{Result, telemetry::*};

pub(super) fn redact(mut read: Box<ConfiguredLogRateRead>) -> Result<Box<ConfiguredLogRateRead>> {
    if !read.is_valid_read(false) {
        return Err(CollectionStop::InvalidPage.error());
    }
    redact_read_context(&mut read.context);
    if let Some(point) = &mut read.observation {
        point.database_incarnation = opaque(&point.database_incarnation);
    }
    read.is_valid_read(false).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read() -> Box<ConfiguredLogRateRead> {
        let secret = "credential-and-path-canary:/private/db/password";
        let mut database =
            crate::DatabaseIdentity::server(crate::Engine::Mssql, "app", "private-host-canary", 1433, None);
        database.incarnation = Some(secret.into());
        Box::new(ConfiguredLogRateRead {
            context: NativeReadContext {
                target: database.into(),
                partition: SourcePartition::Target,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                database_writable: None,
            },
            source: StateSourceCoverage::observed(CONFIGURED_LOG_RATES_SOURCE.into(), 1, false),
            observation: Some(Box::new(ConfiguredLogRateLimits {
                observed_at: Some(canonical_time("2020-01-01T12:00:00Z").unwrap()),
                database_id: "7".into(),
                database_incarnation: secret.into(),
                workload_group_id: None,
                workload_group_bytes_per_second: Some("9007199254740993".into()),
                resource_pool_bytes_per_second: Some("0".into()),
                instance_bytes_per_second: None,
            })),
        })
    }

    #[test]
    fn sharing_keeps_exact_values_and_source_clocks_while_opaque_native_namespace_is_idempotent() {
        let original = read();
        assert!(original.is_valid_read(true));
        let shared = redact(original).unwrap();
        let text = serde_json::to_string(&shared).unwrap();
        for secret in ["private-host-canary", "credential-and-path-canary", "/private/db/password"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("9007199254740993"));
        assert_eq!(shared.observation.as_ref().unwrap().resource_pool_bytes_per_second.as_deref(), Some("0"));
        assert_eq!(shared.context.observed_from, "2020-01-01T12:00:00Z");
        assert_eq!(
            serde_json::to_value(redact(shared.clone()).unwrap()).unwrap(),
            serde_json::to_value(shared).unwrap()
        );
    }

    #[test]
    fn sharing_does_not_repair_a_foreign_original_namespace_or_invalid_rate() {
        let mut foreign = read();
        foreign.observation.as_mut().unwrap().database_incarnation = "another-original-database".into();
        assert!(redact(foreign).is_err());
        let mut invalid = read();
        invalid.observation.as_mut().unwrap().instance_bytes_per_second = Some("-1".into());
        assert!(redact(invalid).is_err());
    }
}
