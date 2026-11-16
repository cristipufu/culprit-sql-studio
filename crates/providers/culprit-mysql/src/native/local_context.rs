use crate::{
    connect::{Connector, Flavor},
    native::{self, instance, metadata},
    read::QueryRead,
};
use culprit_core::{ErrorCode, Result, telemetry::*};

pub(crate) struct Context {
    pub connection: metadata::ConnectionContext,
    member: Option<SharedResourceIdentity>,
    started_second: Option<u64>,
    pub instrumented: bool,
    pub stages_instrumented: Option<bool>,
}

#[derive(Clone, Copy)]
enum Purpose {
    SessionAction,
    Retained,
    Live,
}

impl Context {
    pub(crate) async fn read<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Option<Self>> {
        match Self::required(reader, connector).await {
            Ok(context) => Ok(Some(context)),
            Err(error) if optional(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn required<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Self> {
        let context = Self::acquire(reader, connector, Purpose::SessionAction).await?;
        if context.member.is_none() || context.started_second.is_none() || !context.connection.performance_schema {
            return Err(culprit_core::SessionActionTarget::unavailable());
        }
        Ok(context)
    }

    /// Current diagnostic facts may retain unknown member identity. Strict action admission remains in `read`.
    pub(crate) async fn capture<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Self> {
        Self::acquire(reader, connector, Purpose::Retained).await
    }

    pub(crate) async fn capture_live<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Self> {
        Self::acquire(reader, connector, Purpose::Live).await
    }

    async fn acquire<R: QueryRead>(reader: &mut R, connector: &Connector, purpose: Purpose) -> Result<Self> {
        let connection = metadata::connection_context(reader, connector).await?;
        let mut context =
            Self { connection, member: None, started_second: None, instrumented: false, stages_instrumented: None };
        if !context.connection.performance_schema {
            return Ok(context);
        }
        match context.read_native_facts(reader, purpose).await {
            Ok(()) => Ok(context),
            // A settled optional-source refusal can preserve an independent interactive session point. Native
            // shape, cancellation, timeout, transport and budget failures remain terminal; actions and retained
            // sources use their existing stricter policy.
            Err(error) if matches!(purpose, Purpose::Live) && unavailable(&error) => Ok(context),
            Err(error) => Err(error),
        }
    }

    async fn read_native_facts<R: QueryRead>(&mut self, reader: &mut R, purpose: Purpose) -> Result<()> {
        let sql = match self.connection.flavor {
            Flavor::Mysql => Some("SELECT @@GLOBAL.server_uuid AS server_uuid"),
            Flavor::Mariadb if has_mariadb_uid(&self.connection.version) => {
                Some("SELECT @@GLOBAL.server_uid AS server_uid")
            }
            Flavor::Mariadb => None,
        };
        self.member = match sql {
            Some(sql) => {
                let configuration = native::one(reader, sql, vec![]).await?;
                let field = match self.connection.flavor {
                    Flavor::Mysql => "server_uuid",
                    Flavor::Mariadb => "server_uid",
                };
                configuration
                    .native_text(field)?
                    .map(|value| instance::resource(self.connection.flavor, &value))
                    .transpose()?
            }
            None => None,
        };
        if matches!(purpose, Purpose::SessionAction) && self.member.is_none() {
            // Session actions retain the original admission order: unavailable native identity must not
            // start more optional source work. Diagnostic captures may retain a partial native point.
            return Err(culprit_core::SessionActionTarget::unavailable());
        }
        self.started_second = Some(metadata::process_started_second(reader, self.connection.flavor).await?);
        let (instrumented, stages_instrumented) = if !matches!(purpose, Purpose::SessionAction) {
            let consumers = native::activity::consumers(reader, true).await?;
            (consumers.statements, consumers.stages)
        } else {
            (native::activity::instrumentation_enabled(reader).await?, None)
        };
        self.instrumented = instrumented;
        self.stages_instrumented = stages_instrumented;
        Ok(())
    }

    pub(crate) fn completed(&self, after: &Self) -> Option<NativeReadContext> {
        if self.instrumented != after.instrumented {
            return None;
        }
        self.checked_context(after).filter(NativeReadContext::has_qualified_local_owner)
    }

    pub(crate) fn current_context(&self) -> Option<NativeReadContext> {
        self.checked_context(self)
    }

    pub(crate) fn checked_context(&self, after: &Self) -> Option<NativeReadContext> {
        if !self.connection.same_source(&after.connection)
            || self.member != after.member
            || self.started_second != after.started_second
        {
            return None;
        }
        let context = NativeReadContext {
            target: self.connection.database.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: self.member.clone(),
                epoch: self.started_second.map(process_epoch),
            },
            observed_from: canonical_time(&self.connection.at).ok()?,
            observed_until: canonical_time(&after.connection.at).ok()?,
            database_writable: Some(!self.connection.read_only),
        };
        context.is_valid().then_some(context)
    }
}

pub(crate) fn process_epoch(started_second: u64) -> String {
    digest(format!("mysql-native-process-v1|{started_second}").as_bytes())
}

pub(crate) fn optional(error: &culprit_core::Error) -> bool {
    unavailable(error)
        || error.detail == Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}

pub(crate) fn unavailable(error: &culprit_core::Error) -> bool {
    error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
}

// Native server_uid is a MAC/port-derived server identifier, not a process generation or a global host UUID.
// https://mariadb.com/docs/server/server-management/variables-and-modes/server-system-variables#server_uid
pub(crate) fn has_mariadb_uid(version: &str) -> bool {
    let version = version.strip_prefix("5.5.5-").unwrap_or(version);
    const BACKPORTS: [[u32; 3]; 7] =
        [[10, 5, 26], [10, 6, 19], [10, 11, 9], [11, 1, 6], [11, 2, 5], [11, 4, 3], [11, 5, 2]];
    culprit_core::version_at_least(version, [11, 6, 1])
        || BACKPORTS.iter().any(|minimum| {
            culprit_core::version_at_least(version, *minimum)
                && !culprit_core::version_at_least(version, [minimum[0], minimum[1] + 1, 0])
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mariadb_identity_queries_are_limited_to_releases_that_have_the_native_variable() {
        for version in ["10.5.26-MariaDB", "5.5.5-10.11.9-MariaDB", "11.4.3-MariaDB", "11.8.9-MariaDB"] {
            assert!(has_mariadb_uid(version), "{version}");
        }
        for version in ["10.5.25-MariaDB", "10.6.18-MariaDB", "10.11.8-MariaDB", "11.3.2-MariaDB", "11.6.0-MariaDB"] {
            assert!(!has_mariadb_uid(version), "{version}");
        }
    }
}
