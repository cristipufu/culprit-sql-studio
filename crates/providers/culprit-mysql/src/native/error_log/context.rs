use super::{Availability, availability};
use crate::{native, native::instance, read::QueryRead};
use culprit_core::{Error, ErrorCode, Result};

/// The route's verified native hosting instance. Process and session qualifiers remain private to this read;
/// retained diagnostics can come from a previous run and cannot acquire the current process generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub instance: instance::Identity,
    log_error_services: String,
}

pub(crate) async fn read<R: QueryRead>(reader: &mut R) -> Result<(Context, String)> {
    let platform = instance::platform(reader).await?;
    let support = availability(platform.flavor, &platform.version, platform.performance_schema);
    if support != Availability::Available {
        return Err(unavailable(support));
    }
    let configuration = native::one(
        reader,
        "SELECT @@GLOBAL.server_uuid AS server_uuid, @@GLOBAL.log_error_services AS log_error_services",
        vec![],
    )
    .await?;
    let context = Context {
        instance: platform.identify(reader, &configuration).await?,
        // An empty current pipeline must not erase diagnostics retained from an earlier configuration.
        log_error_services: configuration.native_text("log_error_services")?.ok_or_else(native::invalid)?,
    };
    Ok((context, platform.observed_at))
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

#[cfg(test)]
pub(crate) mod tests;
