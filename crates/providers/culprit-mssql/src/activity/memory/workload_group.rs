//! Optional SQL Server17+ tempdb data governance comes from the selected original grant's group and pool.
//! The same SELECT/inventory owns these rows; nullable stored limits are never effective enforcement proof.
use super::*;

const OBJECT: &str = "sys.dm_resource_governor_workload_groups";
const STORED_OBJECT: &str = "sys.resource_governor_workload_groups";
const MIN_NATIVE_MAJOR: u32 = 17;
const NATIVE_EDITIONS: &[i64] = &[2, 3, 4];
const KEYS: &[&str] = &["group_id", "pool_id"];

struct Field {
    name: &'static str,
    read: fn(&Rec, &str) -> Result<Option<String>>,
    assign: fn(&mut MssqlWorkloadGroupTempdbObservation, Option<String>),
}

const FIELDS: &[Field] = &[
    Field {
        name: "statistics_start_time",
        read: optional_text,
        assign: |group, value| group.statistics_started_without_offset = value,
    },
    Field {
        name: "tempdb_data_space_kb",
        read: optional_counter,
        assign: |group, value| group.current_kibibytes = value,
    },
    Field {
        name: "peak_tempdb_data_space_kb",
        read: optional_counter,
        assign: |group, value| group.peak_kibibytes = value,
    },
    Field {
        name: "total_tempdb_data_limit_violation_count",
        read: optional_counter,
        assign: |group, value| group.limit_violations = value,
    },
];
const LIMITS: &[&str] = &["group_max_tempdb_data_mb", "group_max_tempdb_data_percent"];
pub(super) const COLUMN_COUNT: usize = KEYS.len() + FIELDS.len() + KEYS.len() + LIMITS.len();

fn names() -> impl Iterator<Item = &'static str> {
    KEYS.iter().copied().chain(FIELDS.iter().map(|field| field.name))
}
fn limit_names() -> impl Iterator<Item = &'static str> {
    KEYS.iter().copied().chain(LIMITS.iter().copied())
}
fn inventory_name(name: &str) -> String {
    format!("workload_group.{name}")
}
fn limit_inventory_name(name: &str) -> String {
    format!("workload_group_limits.{name}")
}
fn alias(name: &str) -> String {
    format!("workload_group_{name}")
}
fn limit_alias(name: &str) -> String {
    format!("workload_group_limits_{name}")
}

pub(super) fn qualified_scope(owner: &crate::local_source::Context, shared_scope: bool) -> bool {
    shared_scope && owner.activity.major >= MIN_NATIVE_MAJOR && NATIVE_EDITIONS.contains(&owner.activity.edition)
}

pub(super) fn inventory_sql(shared_scope: bool) -> String {
    let names = names().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
    let limits = limit_names().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
    let editions = NATIVE_EDITIONS.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let admitted = format!(
        "{}=1 AND TRY_CONVERT(int,SERVERPROPERTY('EngineEdition')) IN({editions}) AND TRY_CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))>={MIN_NATIVE_MAJOR} AND HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1",
        u8::from(shared_scope)
    );
    format!(
        "SELECT N'workload_group.'+name AS name FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{OBJECT}') AND name IN({names}) AND {admitted} \
         UNION ALL SELECT N'workload_group_limits.'+name AS name FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{STORED_OBJECT}') AND name IN({limits}) AND {admitted} AND HAS_PERMS_BY_NAME(NULL,NULL,'VIEW ANY DEFINITION')=1"
    )
}

pub(super) fn is_inventory_name(name: &str) -> bool {
    names().any(|field| inventory_name(field) == name) || limit_names().any(|field| limit_inventory_name(field) == name)
}

pub(super) struct Layout {
    available: bool,
    stored_limits: bool,
}
impl Layout {
    pub(super) fn from_inventory(columns: &BTreeSet<String>, shared_scope: bool) -> Self {
        let available = shared_scope && names().all(|name| columns.contains(&inventory_name(name)));
        Self {
            available,
            stored_limits: available && limit_names().all(|name| columns.contains(&limit_inventory_name(name))),
        }
    }

    pub(super) fn joins(&self) -> &'static str {
        match (self.available, self.stored_limits) {
            (false, _) => "",
            (true, false) => {
                "LEFT JOIN sys.dm_resource_governor_workload_groups wg ON wg.group_id=mg.group_id AND wg.pool_id=mg.pool_id"
            }
            (true, true) => {
                "LEFT JOIN sys.dm_resource_governor_workload_groups wg ON wg.group_id=mg.group_id AND wg.pool_id=mg.pool_id LEFT JOIN sys.resource_governor_workload_groups wc ON wc.group_id=wg.group_id AND wc.pool_id=wg.pool_id"
            }
        }
    }

    pub(super) fn projection(&self) -> String {
        let counters = names().map(|name| {
            let value = match (self.available, name) {
                (true, "statistics_start_time") => {
                    "CONVERT(varchar(40),CAST(wg.statistics_start_time AS datetime2),126)".into()
                }
                (true, _) => format!("wg.[{name}]"),
                (false, "statistics_start_time") => "CAST(NULL AS varchar(40))".into(),
                _ => "CAST(NULL AS bigint)".into(),
            };
            format!("{value} AS {}", alias(name))
        });
        let limits = limit_names().map(|name| {
            let value = if self.stored_limits { format!("wc.[{name}]") } else { "CAST(NULL AS float)".into() };
            format!("{value} AS {}", limit_alias(name))
        });
        counters.chain(limits).collect::<Vec<_>>().join(",")
    }

    pub(super) fn decode(&self, row: &Rec, grant: &mut ExecutionGrantObservation) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        let group = optional_counter(row, &alias("group_id"))?;
        let pool = optional_counter(row, &alias("pool_id"))?;
        for (name, value) in [("group_id", &group), ("pool_id", &pool)] {
            fields::record_column(grant, inventory_name(name), value.is_none());
        }
        let mut observation = MssqlWorkloadGroupTempdbObservation {
            group_id: group.clone().unwrap_or_default(),
            pool_id: pool.clone().unwrap_or_default(),
            statistics_started_without_offset: None,
            current_kibibytes: None,
            peak_kibibytes: None,
            limit_violations: None,
            stored_limits: None,
        };
        let mut has_value = false;
        for field in FIELDS {
            let value = (field.read)(row, &alias(field.name))?;
            has_value |= value.is_some();
            fields::record_column(grant, inventory_name(field.name), value.is_none());
            (field.assign)(&mut observation, value);
        }
        match (&group, &pool) {
            (None, None) if !has_value => return Ok(()),
            (Some(group), Some(pool)) if group == "0" || pool == "0" => return Ok(()),
            (Some(_), Some(_)) if observation.is_valid() && observation.matches_grant(grant) => {}
            _ => return Err(invalid_data()),
        }
        if self.stored_limits {
            observation.stored_limits = stored_limits(row, grant, &observation)?;
        }
        grant.workload_group_tempdb = Some(observation);
        Ok(())
    }
}

fn stored_limits(
    row: &Rec,
    grant: &mut ExecutionGrantObservation,
    group: &MssqlWorkloadGroupTempdbObservation,
) -> Result<Option<MssqlStoredTempdbDataLimits>> {
    let group_id = optional_counter(row, &limit_alias("group_id"))?;
    let pool_id = optional_counter(row, &limit_alias("pool_id"))?;
    let fixed = crate::read::optional_nonnegative_number(row, &limit_alias(LIMITS[0]))?.map(|value| value.to_string());
    let percent =
        crate::read::optional_nonnegative_number(row, &limit_alias(LIMITS[1]))?.map(|value| value.to_string());
    for (name, value) in [("group_id", &group_id), ("pool_id", &pool_id), (LIMITS[0], &fixed), (LIMITS[1], &percent)] {
        fields::record_column(grant, limit_inventory_name(name), value.is_none());
    }
    let limits = MssqlStoredTempdbDataLimits { fixed_mebibytes: fixed, percent };
    match (group_id, pool_id) {
        (None, None) if limits.fixed_mebibytes.is_none() && limits.percent.is_none() => Ok(None),
        (Some(group_id), Some(pool_id))
            if group_id == group.group_id && pool_id == group.pool_id && limits.is_valid() =>
        {
            Ok(Some(limits))
        }
        _ => Err(invalid_data()),
    }
}

#[cfg(test)]
mod tests;
