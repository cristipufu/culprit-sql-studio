//! A nonce-owned fixture resource boundary. It never changes a production reader or issues cost authority.
use super::{MAX_FIXTURE_MEMORY_KIB, MAX_FIXTURE_REQUESTS};
use crate::{
    conn::{MsClient, execute, map_error},
    read::{boolean, integer, invalid_data, optional_counter, text},
    value::Rec,
};
use culprit_core::Result;

const POOL_MEMORY_PERCENT: i64 = 20;
const REQUEST_GRANT_PERCENT: i64 = 100;

const INITIAL: &str = "SELECT /* culprit */ DB_NAME() AS database_name,c.classifier_function_id AS configured_classifier,d.classifier_function_id AS effective_classifier,c.is_enabled,d.is_reconfiguration_pending,CASE WHEN EXISTS(SELECT 1 FROM sys.resource_governor_resource_pools WHERE name NOT IN(N'internal',N'default')) OR EXISTS(SELECT 1 FROM sys.resource_governor_workload_groups WHERE name NOT IN(N'internal',N'default')) OR OBJECT_ID(@P3) IS NOT NULL THEN 1 ELSE 0 END AS collision FROM sys.resource_governor_configuration c CROSS JOIN sys.dm_resource_governor_configuration d";
const EFFECTIVE: &str = "SELECT TOP(1) /* culprit */ c.classifier_function_id AS configured_classifier,d.classifier_function_id AS effective_classifier,c.is_enabled,d.is_reconfiguration_pending,OBJECT_ID(@P3,N'FN') AS classifier_id,p.pool_id,p.name AS pool_name,p.min_memory_percent,p.max_memory_percent,p.max_memory_kb,p.target_memory_kb,g.group_id,g.name AS group_name,g.request_max_memory_grant_percent_numeric AS grant_percent,g.group_max_requests,CASE WHEN EXISTS(SELECT 1 FROM sys.resource_governor_resource_pools WHERE name NOT IN(N'internal',N'default',@P1)) OR EXISTS(SELECT 1 FROM sys.resource_governor_workload_groups WHERE name NOT IN(N'internal',N'default',@P2)) THEN 1 ELSE 0 END AS foreign_profiles,(SELECT COUNT_BIG(*) FROM sys.dm_exec_sessions s WHERE s.group_id=g.group_id) AS group_sessions FROM sys.resource_governor_configuration c CROSS JOIN sys.dm_resource_governor_configuration d JOIN sys.dm_resource_governor_resource_pools p ON p.name=@P1 JOIN sys.dm_resource_governor_workload_groups g ON g.name=@P2 AND g.pool_id=p.pool_id";

pub(super) struct ActorRoute {
    pub application: String,
    pub pool: String,
    pub group: String,
    pub pool_id: i64,
    pub group_id: i64,
    pub classifier_id: i64,
}
impl ActorRoute {
    pub fn matches(&self, row: &Rec) -> Result<bool> {
        let _ = capacity(row)?;
        Ok(text(row, "actor_application")? == self.application
            && text(row, "pool_name")? == self.pool
            && text(row, "group_name")? == self.group
            && integer(row, "pool_id")? == self.pool_id
            && integer(row, "group_id")? == self.group_id
            && integer(row, "configured_classifier")? == self.classifier_id
            && integer(row, "effective_classifier")? == self.classifier_id
            && !boolean(row, "is_reconfiguration_pending")?)
    }
}

struct Proof {
    pool_id: i64,
    group_id: i64,
    classifier_id: i64,
    initial_max_memory_kib: u64,
    initial_target_memory_kib: u64,
}

pub(super) struct ResourceProfile {
    pool: String,
    group: String,
    classifier: String,
    applications: Vec<String>,
    original_enabled: Option<bool>,
    proof: Option<Proof>,
    removed: bool,
}
impl ResourceProfile {
    pub fn new(owned_database: &str) -> Result<Self> {
        let suffix = owned_database.replace('-', "_");
        let pool = format!("grant_pool_{suffix}");
        let group = format!("grant_group_{suffix}");
        let classifier = format!("grant_route_{suffix}");
        if [&pool, &group, &classifier]
            .iter()
            .any(|name| name.len() > 128 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return Err(invalid_data());
        }
        let applications =
            (0..MAX_FIXTURE_REQUESTS).map(|index| format!("culprit_grant_{suffix}_{index}")).collect::<Vec<_>>();
        if applications.iter().any(|name| name.len() > 128) {
            return Err(invalid_data());
        }
        Ok(Self { pool, group, classifier, applications, original_enabled: None, proof: None, removed: false })
    }

    pub async fn install(&mut self, admin: &mut MsClient) -> Result<()> {
        let original = self.read(admin, INITIAL).await?;
        self.original_enabled = Some(initial_enabled(&original)?);
        execute(admin, &self.create_resources()).await.map_err(map_error)?;
        // CREATE FUNCTION is its own batch. All names/literals are closed, checked owned identifiers.
        execute(admin, &self.create_classifier()).await.map_err(map_error)?;
        execute(admin, &self.bind_classifier()).await.map_err(map_error)?;
        let effective = self.read(admin, EFFECTIVE).await?;
        let proof = self.validate_effective(&effective)?;
        if integer(&effective, "group_sessions")? != 0 {
            return Err(invalid_data());
        }
        self.proof = Some(proof);
        Ok(())
    }

    pub fn route(&self, index: usize) -> Result<ActorRoute> {
        let proof = self.proof.as_ref().filter(|_| !self.removed).ok_or_else(invalid_data)?;
        Ok(ActorRoute {
            application: self.applications.get(index).ok_or_else(invalid_data)?.clone(),
            pool: self.pool.clone(),
            group: self.group.clone(),
            pool_id: proof.pool_id,
            group_id: proof.group_id,
            classifier_id: proof.classifier_id,
        })
    }

    pub async fn remove(&mut self, admin: &mut MsClient) -> Result<()> {
        let original_enabled = self.original_enabled.ok_or_else(invalid_data)?;
        // Closed workload sockets are not a session-census acknowledgement. A fixed bounded settlement pause
        // precedes the actual native zero-session check; no retry, KILL or disappearance-based recovery oracle.
        execute(admin, "/* culprit */ WAITFOR DELAY '00:00:00.010'").await.map_err(map_error)?;
        let current = self.read(admin, EFFECTIVE).await?;
        self.validate_cleanup(&current)?;
        let expected = self.proof.as_ref().ok_or_else(invalid_data)?;
        execute(
            admin,
            "/* culprit */ ALTER RESOURCE GOVERNOR WITH(CLASSIFIER_FUNCTION=NULL);ALTER RESOURCE GOVERNOR RECONFIGURE",
        )
        .await
        .map_err(map_error)?;
        let detached = self.read(admin, INITIAL).await?;
        if integer(&detached, "configured_classifier")? != 0
            || integer(&detached, "effective_classifier")? != 0
            || boolean(&detached, "is_reconfiguration_pending")?
        {
            return Err(invalid_data());
        }
        execute(admin, &self.drop_resources(expected, original_enabled)).await.map_err(map_error)?;
        let restored = self.read(admin, INITIAL).await?;
        if initial_enabled(&restored)? != original_enabled {
            return Err(invalid_data());
        }
        self.removed = true;
        Ok(())
    }

    fn validate_cleanup(&self, current: &Rec) -> Result<()> {
        let expected = self.proof.as_ref().ok_or_else(invalid_data)?;
        let actual = self.validate_effective(current)?;
        if actual.pool_id != expected.pool_id
            || actual.group_id != expected.group_id
            || actual.classifier_id != expected.classifier_id
            || integer(current, "group_sessions")? != 0
        {
            return Err(invalid_data());
        }
        Ok(())
    }

    fn create_resources(&self) -> String {
        format!(
            "/* culprit */ CREATE RESOURCE POOL [{}] WITH(MIN_MEMORY_PERCENT=0,MAX_MEMORY_PERCENT={POOL_MEMORY_PERCENT});CREATE WORKLOAD GROUP [{}] WITH(REQUEST_MAX_MEMORY_GRANT_PERCENT={REQUEST_GRANT_PERCENT},GROUP_MAX_REQUESTS=0) USING [{}]",
            self.pool, self.group, self.pool
        )
    }

    fn create_classifier(&self) -> String {
        let apps = self.applications.iter().map(|value| format!("N'{value}'")).collect::<Vec<_>>().join(",");
        format!(
            "/* culprit */ CREATE FUNCTION [dbo].[{}]() RETURNS sysname WITH SCHEMABINDING AS BEGIN DECLARE @group sysname;IF APP_NAME() IN({apps}) SET @group=N'{}';RETURN @group;END",
            self.classifier, self.group
        )
    }

    fn bind_classifier(&self) -> String {
        format!(
            "/* culprit */ ALTER RESOURCE GOVERNOR WITH(CLASSIFIER_FUNCTION=[dbo].[{}]);ALTER RESOURCE GOVERNOR RECONFIGURE",
            self.classifier
        )
    }

    fn drop_resources(&self, expected: &Proof, original_enabled: bool) -> String {
        let ids = format!(
            "IF ISNULL(OBJECT_ID(N'dbo.{}',N'FN'),0)<>{} OR NOT EXISTS(SELECT 1 FROM sys.resource_governor_workload_groups WHERE name=N'{}' AND group_id={}) OR NOT EXISTS(SELECT 1 FROM sys.resource_governor_resource_pools WHERE name=N'{}' AND pool_id={}) THROW 51000,N'Owned grant profile changed',1;",
            self.classifier, expected.classifier_id, self.group, expected.group_id, self.pool, expected.pool_id
        );
        format!(
            "/* culprit */ {ids} DROP FUNCTION [dbo].[{}];DROP WORKLOAD GROUP [{}];DROP RESOURCE POOL [{}];ALTER RESOURCE GOVERNOR RECONFIGURE;{}",
            self.classifier,
            self.group,
            self.pool,
            if original_enabled { "" } else { "ALTER RESOURCE GOVERNOR DISABLE;" }
        )
    }

    fn validate_effective(&self, row: &Rec) -> Result<Proof> {
        let classifier_id = integer(row, "classifier_id")?;
        let pool_id = integer(row, "pool_id")?;
        let group_id = integer(row, "group_id")?;
        let (max, target) = capacity(row)?;
        if classifier_id <= 0
            || pool_id <= 2
            || group_id <= 2
            || integer(row, "configured_classifier")? != classifier_id
            || integer(row, "effective_classifier")? != classifier_id
            || boolean(row, "is_reconfiguration_pending")?
            || text(row, "pool_name")? != self.pool
            || text(row, "group_name")? != self.group
            || integer(row, "foreign_profiles")? != 0
        {
            return Err(invalid_data());
        }
        Ok(Proof { pool_id, group_id, classifier_id, initial_max_memory_kib: max, initial_target_memory_kib: target })
    }

    async fn read(&self, admin: &mut MsClient, sql: &str) -> Result<Rec> {
        let mut query = tiberius::Query::new(sql);
        let classifier = format!("dbo.{}", self.classifier);
        query.bind(self.pool.as_str());
        query.bind(self.group.as_str());
        query.bind(classifier.as_str());
        let results = query.query(admin).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
        let [rows] = results.as_slice() else {
            return Err(invalid_data());
        };
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        Ok(Rec::from_row(row))
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "pool":self.pool,
            "group":self.group,
            "classifier":format!("dbo.{}",self.classifier),
            "actor_applications":self.applications,
            "pool_memory_percent":POOL_MEMORY_PERCENT,
            "request_max_memory_grant_percent":REQUEST_GRANT_PERCENT,
            "group_max_requests":0,
            "original_enabled":self.original_enabled,
            "installed":self.proof.is_some(),
            "removed":self.removed,
            "native":self.proof.as_ref().map(|p|serde_json::json!({
                "pool_id":p.pool_id,
                "group_id":p.group_id,
                "classifier_id":p.classifier_id,
                "max_memory_kibibytes":p.initial_max_memory_kib.to_string(),
                "target_memory_kibibytes":p.initial_target_memory_kib.to_string(),
            })),
        })
    }
}

fn capacity(row: &Rec) -> Result<(u64, u64)> {
    let max = positive(row, "max_memory_kb")?;
    let target = positive(row, "target_memory_kb")?;
    if !boolean(row, "is_enabled")?
        || integer(row, "min_memory_percent")? != 0
        || integer(row, "max_memory_percent")? != POOL_MEMORY_PERCENT
        || row.f64("grant_percent") != Some(REQUEST_GRANT_PERCENT as f64)
        || integer(row, "group_max_requests")? != 0
        || target > max
        || !max.checked_mul(MAX_FIXTURE_REQUESTS as u64).is_some_and(|value| value <= MAX_FIXTURE_MEMORY_KIB)
    {
        return Err(invalid_data());
    }
    Ok((max, target))
}

fn initial_enabled(row: &Rec) -> Result<bool> {
    if text(row, "database_name")? != "master"
        || integer(row, "configured_classifier")? != 0
        || integer(row, "effective_classifier")? != 0
        || boolean(row, "is_reconfiguration_pending")?
        || integer(row, "collision")? != 0
    {
        return Err(invalid_data());
    }
    boolean(row, "is_enabled")
}
fn positive(row: &Rec, name: &str) -> Result<u64> {
    let value = optional_counter(row, name)?.ok_or_else(invalid_data)?.parse::<u64>().map_err(|_| invalid_data())?;
    if value == 0 {
        return Err(invalid_data());
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
