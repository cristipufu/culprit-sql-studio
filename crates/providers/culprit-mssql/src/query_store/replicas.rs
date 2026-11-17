//! Query Store replica groups describe retained runtime populations, not the connected compute member.
use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_text},
    value::Rec,
};
use culprit_core::{
    Error, ErrorCode, Result, RuntimePopulation, RuntimePopulationCatalog, i18n::Message, t, telemetry::NativeIdentity,
};
use serde::Serialize;
use std::collections::BTreeSet;

pub(crate) const MAX_REPLICA_GROUP_ROWS: usize = 100;
const MAX_REPLICA_NAME_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct ReplicaGroup {
    pub replica_group_id: i64,
    pub role_type: u8,
    pub replica_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Catalog {
    groups: Vec<ReplicaGroup>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Population {
    /// The native runtime layout has no replica dimension. This is not an invented replica ID.
    Unpartitioned,
    ReplicaGroup(i64),
    /// One native acquisition groups rows by their actual replica ID. This is never a selected/exported identity.
    AllGroups,
}

impl Population {
    /// IDs come only from native catalog rows or a fully drained window summary. Keep one predicate per aggregate;
    /// the group never becomes a query/plan identity or a physical compute-member identity.
    pub(crate) fn predicate(self, alias: &str) -> String {
        match self {
            Self::Unpartitioned | Self::AllGroups => "1=1".into(),
            Self::ReplicaGroup(group) => format!("{alias}.replica_group_id={group}"),
        }
    }

    pub(crate) fn identity(self) -> Result<Option<NativeIdentity>> {
        match self {
            Self::Unpartitioned => Ok(None),
            Self::ReplicaGroup(group) => Ok(Some(NativeIdentity::new(group.to_string()))),
            Self::AllGroups => Err(invalid_data()),
        }
    }

    pub(crate) fn projection(self, alias: &str) -> String {
        match self {
            Self::Unpartitioned => "CAST(NULL AS bigint) AS replica_group_id".into(),
            Self::ReplicaGroup(_) | Self::AllGroups => format!("{alias}.replica_group_id"),
        }
    }

    pub(crate) fn group_by(self, alias: &str) -> String {
        match self {
            Self::Unpartitioned => String::new(),
            Self::ReplicaGroup(_) | Self::AllGroups => format!(",{alias}.replica_group_id"),
        }
    }

    pub(crate) fn from_row(row: &Rec) -> Result<Self> {
        match crate::read::optional_integer(row, "replica_group_id")? {
            Some(group) if group >= 0 => Ok(Self::ReplicaGroup(group)),
            None => Ok(Self::Unpartitioned),
            _ => Err(invalid_data()),
        }
    }
}

impl Catalog {
    pub(crate) fn decode(rows: &[Rec]) -> Result<Self> {
        if rows.is_empty() || rows.len() > MAX_REPLICA_GROUP_ROWS {
            return Err(invalid_data());
        }
        let mut groups = BTreeSet::new();
        for row in rows {
            let replica_group_id = integer(row, "replica_group_id")?;
            let role_type = integer(row, "role_type")?.try_into().map_err(|_| invalid_data())?;
            let replica_name = optional_text(row, "replica_name")?;
            if replica_group_id < 0
                || replica_name.as_ref().is_some_and(|name| name.len() > MAX_REPLICA_NAME_BYTES)
                || !groups.insert(ReplicaGroup { replica_group_id, role_type, replica_name })
            {
                return Err(invalid_data());
            }
        }
        Ok(Self { groups: groups.into_iter().collect() })
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.groups
            .iter()
            .map(|group| group.replica_group_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| id.to_string())
            .collect()
    }

    pub(crate) fn select(&self, requested: Option<&NativeIdentity>) -> Result<Population> {
        let ids = self.groups.iter().map(|group| group.replica_group_id).collect::<BTreeSet<_>>();
        let selected = match requested {
            Some(requested) => {
                let id = requested_group(requested)?;
                if !ids.contains(&id) {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        t!("provider.mssql.query_store.population_missing"),
                    ));
                }
                id
            }
            None if ids.len() == 1 => *ids.first().ok_or_else(invalid_data)?,
            None => {
                return Err(Error::new(ErrorCode::Unsupported, t!("provider.mssql.query_store.population_required")));
            }
        };
        Ok(Population::ReplicaGroup(selected))
    }

    pub(crate) fn population_catalog(&self) -> Result<RuntimePopulationCatalog> {
        let populations = self
            .ids()
            .into_iter()
            .map(|id| {
                let groups =
                    self.groups.iter().filter(|group| group.replica_group_id.to_string() == id).collect::<Vec<_>>();
                Ok(RuntimePopulation {
                    identity: NativeIdentity::new(&id),
                    label: Message::list(groups.iter().map(|group| group.label()).collect()),
                    native_properties: [
                        ("replica_group_id".into(), id),
                        ("role_history".into(), serde_json::to_string(&groups).map_err(|_| invalid_data())?),
                    ]
                    .into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let catalog = RuntimePopulationCatalog {
            membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
            source: culprit_core::source_id::QUERY_STORE.into(),
            partitioned: true,
            requires_selection: populations.len() > 1,
            populations,
        };
        if catalog.is_valid() { Ok(catalog) } else { Err(invalid_data()) }
    }
}

pub(crate) fn requested_group(requested: &NativeIdentity) -> Result<i64> {
    requested
        .id
        .parse::<i64>()
        .ok()
        .filter(|id| *id >= 0 && NativeIdentity::new(id.to_string()) == *requested)
        .ok_or_else(|| Error::invalid(t!("provider.mssql.query_store.population_invalid")))
}

impl ReplicaGroup {
    fn label(&self) -> Message {
        let role = match self.role_type {
            1 => culprit_core::message!("provider.mssql.query_store.role.primary"),
            2 => culprit_core::message!("provider.mssql.query_store.role.secondary"),
            3 => culprit_core::message!("provider.mssql.query_store.role.geo_primary"),
            4 => culprit_core::message!("provider.mssql.query_store.role.geo_secondary"),
            _ => culprit_core::message!("provider.mssql.query_store.role.native", role = self.role_type.to_string()),
        };
        match &self.replica_name {
            Some(name) => culprit_core::message!(
                "provider.mssql.query_store.population_named",
                group = self.replica_group_id.to_string(),
                name = name,
                role = role
            ),
            None => culprit_core::message!(
                "provider.mssql.query_store.population_group",
                group = self.replica_group_id.to_string(),
                role = role
            ),
        }
    }
}

/// A runtime summary supplies positive group membership only. Roles and replica names require native metadata.
pub(super) fn observed_catalog(
    populations: impl Iterator<Item = Population>,
    from: &str,
    until: &str,
) -> Result<RuntimePopulationCatalog> {
    let mut ids = BTreeSet::new();
    for population in populations {
        let Population::ReplicaGroup(id) = population else { return Err(invalid_data()) };
        if id < 0 {
            return Err(invalid_data());
        }
        ids.insert(id);
        if ids.len() > culprit_core::MAX_RUNTIME_POPULATIONS {
            return Err(invalid_data());
        }
    }
    if ids.is_empty() {
        return Err(Error::new(ErrorCode::Unsupported, t!("provider.mssql.query_store.population_unobserved")));
    }
    let catalog = RuntimePopulationCatalog {
        source: culprit_core::source_id::QUERY_STORE.into(),
        membership: culprit_core::RuntimePopulationMembership::ObservedWindow {
            from: from.into(),
            until: until.into(),
        },
        partitioned: true,
        requires_selection: ids.len() > 1,
        populations: ids
            .into_iter()
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id.to_string()),
                label: culprit_core::message!("provider.mssql.query_store.population_observed", group = id.to_string()),
                native_properties: [("replica_group_id".into(), id.to_string())].into(),
            })
            .collect(),
    };
    if catalog.is_valid() { Ok(catalog) } else { Err(invalid_data()) }
}

pub(crate) fn unpartitioned_catalog() -> RuntimePopulationCatalog {
    RuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: false,
        requires_selection: false,
        populations: Vec::new(),
    }
}

pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Catalog> {
    let rows = reader
        .query(super::catalog_sql::REPLICAS, vec![], MAX_REPLICA_GROUP_ROWS + 1)
        .await
        .map_err(ReadError::into_error)?;
    Catalog::decode(&rows)
}

#[cfg(test)]
pub(crate) mod tests;
