//! Read-only admission to an explicitly selected nonce-owned loopback smoke database.
use super::{config, connect};
use crate::{
    conn::{MsClient, Pool, SessionKind, map_error},
    read::{invalid_data, text},
    value::Rec,
};
use std::{sync::Arc, time::Instant};

const OWNER_BYTES: usize = 24;
const SHORT_HOST_OWNER_BYTES: usize = 14;

struct OwnedSourceAdmission {
    database: String,
    machine: String,
}

impl OwnedSourceAdmission {
    fn from_environment() -> culprit_core::Result<Self> {
        let owner = std::env::var("CULPRIT_SMOKE_OWNER").map_err(|_| invalid_data())?;
        let database = std::env::var("CULPRIT_SMOKE_DATABASE").map_err(|_| invalid_data())?;
        let role = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").ok();
        let group = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP").ok();
        Self::new(&owner, &database, role.as_deref(), group.as_deref())
    }

    fn new(owner: &str, database: &str, role: Option<&str>, group: Option<&str>) -> culprit_core::Result<Self> {
        if !valid_owner(owner) {
            return Err(invalid_data());
        }
        let machine = if database == "culprit_smoke" {
            if role.is_some() || group.is_some() {
                return Err(invalid_data());
            }
            format!("culprit-smoke-mssql-{owner}")
        } else {
            let (primary, secondary) = database
                .strip_prefix("culprit_smoke_")
                .and_then(|suffix| suffix.split_once('_'))
                .filter(|(primary, secondary)| valid_owner(primary) && valid_owner(secondary) && primary != secondary)
                .ok_or_else(invalid_data)?;
            if group != Some(format!("culprit_ag_{primary}").as_str()) {
                return Err(invalid_data());
            }
            let prefix = match role {
                Some("primary") if owner == primary => 'p',
                Some("secondary") if owner == secondary => 's',
                _ => return Err(invalid_data()),
            };
            format!("{prefix}{}", &owner[..SHORT_HOST_OWNER_BYTES])
        };
        Ok(Self { database: database.into(), machine })
    }

    fn validate_row(&self, row: &Rec) -> culprit_core::Result<()> {
        if text(row, "machine")? != self.machine || text(row, "owned_database")? != self.database {
            return Err(invalid_data());
        }
        Ok(())
    }

    async fn verify(&self, client: &mut MsClient) -> culprit_core::Result<()> {
        let mut query = tiberius::Query::new(
            "SELECT /* culprit fixture guard */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine, \
             CONVERT(nvarchar(128),DB_NAME(DB_ID(@P1))) AS owned_database",
        );
        query.bind(self.database.as_str());
        let rows = query.query(client).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
        let [rows] = rows.as_slice() else { return Err(invalid_data()) };
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        self.validate_row(&Rec::from_row(row))
    }
}

fn valid_owner(value: &str) -> bool {
    value.len() == OWNER_BYTES && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) struct OwnedSourceFixture {
    pub(crate) port: u16,
    pub(crate) database: String,
    pub(crate) pool: Arc<Pool>,
    pub(crate) admission_elapsed_ms: f64,
    password: String,
}

impl OwnedSourceFixture {
    pub(crate) async fn open(application: &str) -> culprit_core::Result<Self> {
        let started = Instant::now();
        let authorized = OwnedSourceAdmission::from_environment()?;
        let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").expect("owned loopback port").parse().unwrap();
        let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").expect("owned source password");
        let database = authorized.database.clone();
        let mut admission =
            connect(&config(port, "master", &password, "cost-fixture-admission"), SessionKind::User).await?;
        authorized.verify(&mut admission).await?;
        Ok(Self {
            pool: Arc::new(Pool::new(config(port, &database, &password, application), None)),
            port,
            database,
            password,
            admission_elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}

pub(crate) async fn verify_owned_source(client: &mut MsClient) -> culprit_core::Result<()> {
    OwnedSourceAdmission::from_environment()?.verify(client).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;

    fn native(machine: &str, database: &str) -> Rec {
        Rec::of(&[("machine", Val::Text(machine.into())), ("owned_database", Val::Text(database.into()))])
    }

    #[test]
    fn ordinary_admission_keeps_the_full_native_hostname_and_fixed_database() {
        let owner = "a".repeat(OWNER_BYTES);
        let admission = OwnedSourceAdmission::new(&owner, "culprit_smoke", None, None).unwrap();
        assert!(admission.validate_row(&native(&format!("culprit-smoke-mssql-{owner}"), "culprit_smoke")).is_ok());
        assert!(
            admission
                .validate_row(&native(&format!("p{}", &owner[..SHORT_HOST_OWNER_BYTES]), "culprit_smoke"))
                .is_err()
        );
        assert!(admission.validate_row(&native(&format!("culprit-smoke-mssql-{owner}"), "another_db")).is_err());
    }

    #[test]
    fn both_ag_members_require_their_role_and_both_complete_native_database_nonces() {
        let primary = "a".repeat(OWNER_BYTES);
        let secondary = "b".repeat(OWNER_BYTES);
        let database = format!("culprit_smoke_{primary}_{secondary}");
        let group = format!("culprit_ag_{primary}");
        for (role, owner, prefix) in [("primary", &primary, 'p'), ("secondary", &secondary, 's')] {
            let admission = OwnedSourceAdmission::new(owner, &database, Some(role), Some(&group)).unwrap();
            let machine = format!("{prefix}{}", &owner[..SHORT_HOST_OWNER_BYTES]);
            assert!(admission.validate_row(&native(&machine, &database)).is_ok());
            let foreign_database = format!(
                "culprit_smoke_{}{}_{secondary}",
                &primary[..SHORT_HOST_OWNER_BYTES],
                "c".repeat(OWNER_BYTES - SHORT_HOST_OWNER_BYTES)
            );
            assert!(
                admission.validate_row(&native(&machine, &foreign_database)).is_err(),
                "a matching short host prefix is not full ownership"
            );
            assert!(admission.validate_row(&native(&machine, &database[..database.len() - 1])).is_err());
            assert!(admission.validate_row(&native("another-native-host", &database)).is_err());
            let missing = Rec::of(&[("machine", Val::Text(machine)), ("owned_database", Val::Null)]);
            assert!(
                admission.validate_row(&missing).is_err(),
                "an environment name cannot replace native DB_ID/DB_NAME evidence"
            );
        }
    }

    #[test]
    fn malformed_foreign_partial_or_inconsistent_configuration_is_refused_before_native_admission() {
        let primary = "a".repeat(OWNER_BYTES);
        let secondary = "b".repeat(OWNER_BYTES);
        let database = format!("culprit_smoke_{primary}_{secondary}");
        let group = format!("culprit_ag_{primary}");
        for (owner, name, role, expected_group) in [
            (primary.as_str(), database.as_str(), Some("secondary"), Some(group.as_str())),
            (secondary.as_str(), database.as_str(), Some("primary"), Some(group.as_str())),
            (primary.as_str(), database.as_str(), None, Some(group.as_str())),
            (primary.as_str(), database.as_str(), Some("primary"), None),
            (primary.as_str(), database.as_str(), Some("primary"), Some("culprit_ag_foreign")),
            (primary.as_str(), "culprit_smoke", Some("primary"), Some(group.as_str())),
            ("AAAAAAAAAAAAAAAAAAAAAAAA", database.as_str(), Some("primary"), Some(group.as_str())),
            (primary.as_str(), "culprit_smoke_partial", Some("primary"), Some(group.as_str())),
            (primary.as_str(), "culprit_smoke_a'; DROP DATABASE master;--", Some("primary"), Some(group.as_str())),
        ] {
            assert!(OwnedSourceAdmission::new(owner, name, role, expected_group).is_err());
        }
        let duplicate = format!("culprit_smoke_{primary}_{primary}");
        assert!(OwnedSourceAdmission::new(&primary, &duplicate, Some("primary"), Some(&group)).is_err());
        assert!(OwnedSourceAdmission::new(&primary, &(database + "_extra"), Some("primary"), Some(&group)).is_err());
    }
}
