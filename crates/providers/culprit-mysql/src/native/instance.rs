//! Native hosting identity shared by explicit instance readers. A route alias never establishes resource ownership.
use super::{flag, integer, invalid, one, text};
use crate::{connect::Flavor, read::QueryRead, rows::Rec};
use culprit_core::{Engine, Result, telemetry::*};

pub(crate) struct Platform {
    pub version: String,
    pub flavor: Flavor,
    pub performance_schema: bool,
    pub observed_at: String,
    connection_id: u64,
    login: String,
    server_id: u64,
    server_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub resource: SharedResourceIdentity,
    pub connection_id: u64,
    pub started_second: u64,
    login: String,
    version: String,
    server_id: u64,
    server_name: String,
}

pub(crate) async fn platform<R: QueryRead>(reader: &mut R) -> Result<Platform> {
    let row = one(
        reader,
        "SELECT VERSION() AS version, @@performance_schema AS performance_schema,
            CONNECTION_ID() AS connection_id, CURRENT_USER() AS login,
            @@GLOBAL.server_id AS server_id, @@hostname AS server_name,
            DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at",
        vec![],
    )
    .await?;
    let version = text(&row, "version")?;
    Ok(Platform {
        flavor: Flavor::of_version(&version),
        version,
        performance_schema: flag(&row, "performance_schema")?,
        observed_at: text(&row, "observed_at")?,
        connection_id: integer(&row, "connection_id")?,
        login: text(&row, "login")?,
        server_id: integer(&row, "server_id")?,
        server_name: text(&row, "server_name")?,
    })
}

impl Platform {
    /// Reuse a native connection point already observed by the selected source; no new probe or inferred owner.
    pub(crate) fn from_connection(connection: &super::metadata::ConnectionContext) -> Self {
        Self {
            version: connection.version.clone(),
            flavor: connection.flavor,
            performance_schema: connection.performance_schema,
            observed_at: connection.at.clone(),
            connection_id: connection.connection_id,
            login: connection.login.clone(),
            server_id: connection.server_id,
            server_name: connection.server_name.clone(),
        }
    }

    /// Only call after the owning source has qualified the native product and UUID availability.
    pub(crate) async fn identify<R: QueryRead>(&self, reader: &mut R, configuration: &Rec) -> Result<Identity> {
        let field = match self.flavor {
            Flavor::Mysql => "server_uuid",
            Flavor::Mariadb => "server_uid",
        };
        let resource = resource(self.flavor, &text(configuration, field)?)?;
        Ok(Identity {
            resource,
            connection_id: self.connection_id,
            started_second: super::metadata::process_started_second(reader, self.flavor).await?,
            login: self.login.clone(),
            version: self.version.clone(),
            server_id: self.server_id,
            server_name: self.server_name.clone(),
        })
    }
}

/// Decode the native member independently of a process/read; callers must retain their own native bounds.
pub(crate) fn resource(flavor: Flavor, value: &str) -> Result<SharedResourceIdentity> {
    let resource = match flavor {
        Flavor::Mysql => {
            let uuid = value.to_ascii_lowercase();
            if !valid_uuid(&uuid) {
                return Err(invalid());
            }
            SharedResourceIdentity {
                engine: Engine::Mysql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", &uuid),
                native: NativeIdentity::new(uuid),
            }
        }
        Flavor::Mariadb => {
            if value.len() != 28
                || !value.ends_with('=')
                || !value[..27].bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
            {
                return Err(invalid());
            }
            SharedResourceIdentity {
                engine: Engine::Mysql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new(MARIADB_SERVER_UID_NAMESPACE),
                native: NativeIdentity::new(value),
            }
        }
    };
    if !resource.is_valid() {
        return Err(invalid());
    }
    Ok(resource)
}

fn valid_uuid(uuid: &str) -> bool {
    uuid.len() == 36
        && uuid.bytes().any(|value| value.is_ascii_hexdigit() && value != b'0')
        && uuid.bytes().enumerate().all(|(offset, value)| {
            if matches!(offset, 8 | 13 | 18 | 23) { value == b'-' } else { value.is_ascii_hexdigit() }
        })
}

/// The existing native identifier availability policy, shared by discovery and local ownership.
pub(crate) fn configuration_query(flavor: Flavor, version: &str) -> Option<&'static str> {
    match flavor {
        Flavor::Mysql => Some("SELECT @@GLOBAL.server_uuid AS server_uuid"),
        Flavor::Mariadb if super::local_context::has_mariadb_uid(version) => {
            Some("SELECT @@GLOBAL.server_uid AS server_uid")
        }
        Flavor::Mariadb => None,
    }
}
