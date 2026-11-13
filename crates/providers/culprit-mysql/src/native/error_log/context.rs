use super::{Availability, availability};
use crate::{connect::Flavor, native, read::QueryRead};
use culprit_core::{Engine, Error, ErrorCode, Result, telemetry::*};

/// The route's verified native hosting instance. Process and session qualifiers remain private to this read;
/// retained diagnostics can come from a previous run and cannot acquire the current process generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub resource: SharedResourceIdentity,
    connection_id: u64,
    login: String,
    version: String,
    server_id: u64,
    server_name: String,
    started_second: u64,
    log_error_services: String,
}

pub(crate) async fn read<R: QueryRead>(reader: &mut R) -> Result<(Context, String)> {
    let platform = native::one(
        reader,
        "SELECT VERSION() AS version, @@performance_schema AS performance_schema,
            CONNECTION_ID() AS connection_id, CURRENT_USER() AS login,
            @@GLOBAL.server_id AS server_id, @@hostname AS server_name,
            DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at",
        vec![],
    )
    .await?;
    let version = native::text(&platform, "version")?;
    let support = availability(Flavor::of_version(&version), &version, native::flag(&platform, "performance_schema")?);
    if support != Availability::Available {
        return Err(unavailable(support));
    }
    let configuration = native::one(
        reader,
        "SELECT @@GLOBAL.server_uuid AS server_uuid, @@GLOBAL.log_error_services AS log_error_services",
        vec![],
    )
    .await?;
    let uuid = native::text(&configuration, "server_uuid")?.to_ascii_lowercase();
    if !valid_uuid(&uuid) {
        return Err(native::invalid());
    }
    let resource = SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", &uuid),
        native: NativeIdentity::new(uuid),
    };
    if !resource.is_valid() {
        return Err(native::invalid());
    }
    let context = Context {
        resource,
        connection_id: native::integer(&platform, "connection_id")?,
        login: native::text(&platform, "login")?,
        version,
        server_id: native::integer(&platform, "server_id")?,
        server_name: native::text(&platform, "server_name")?,
        started_second: native::metadata::process_started_second(reader, Flavor::Mysql).await?,
        // An empty current pipeline must not erase diagnostics retained from an earlier configuration.
        log_error_services: configuration.native_text("log_error_services")?.ok_or_else(native::invalid)?,
    };
    Ok((context, native::text(&platform, "observed_at")?))
}

pub(crate) fn unavailable(support: Availability) -> Error {
    let message = match support {
        Availability::UnsupportedProduct => culprit_core::message!("provider.mysql.native_failures.mariadb"),
        Availability::UnsupportedVersion => culprit_core::message!("provider.mysql.native_failures.version"),
        Availability::InstrumentationDisabled => {
            culprit_core::message!("provider.mysql.native_failures.instrumentation")
        }
        Availability::Available => return native::invalid(),
    };
    Error::new(ErrorCode::Unsupported, message)
}

fn valid_uuid(uuid: &str) -> bool {
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(offset, value)| {
            if matches!(offset, 8 | 13 | 18 | 23) { value == b'-' } else { value.is_ascii_hexdigit() }
        })
}

#[cfg(test)]
pub(crate) mod tests;
