//! SQL Server's retained statement-cache counters. Entry identity does not establish continuity after eviction.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementGrantStatistics {
    pub context: CachedStatementContext,
    pub available_columns: Vec<String>,
    pub executions: Option<String>,
    pub last_execution_without_offset: Option<String>,
    pub total_grant_kibibytes: Option<String>,
    pub total_used_kibibytes: Option<String>,
    pub last_grant_kibibytes: Option<String>,
    pub last_used_kibibytes: Option<String>,
    pub min_grant_kibibytes: Option<String>,
    pub min_used_kibibytes: Option<String>,
    pub max_grant_kibibytes: Option<String>,
    pub max_used_kibibytes: Option<String>,
    pub reset: crate::ResetMarker,
}
impl StatementGrantStatistics {
    pub fn is_valid(&self) -> bool {
        let mut columns = std::collections::BTreeSet::new();
        let counters = [
            &self.total_grant_kibibytes,
            &self.total_used_kibibytes,
            &self.last_grant_kibibytes,
            &self.last_used_kibibytes,
            &self.min_grant_kibibytes,
            &self.min_used_kibibytes,
            &self.max_grant_kibibytes,
            &self.max_used_kibibytes,
        ];
        self.context.is_valid()
            && self.executions.as_deref().is_none_or(valid_counter)
            && self.executions.is_some() == self.context.identity.has_plan_handle()
            && self.available_columns.len() <= 128
            && self.available_columns.iter().all(|name| valid_key(name) && columns.insert(name))
            && counters.into_iter().flatten().all(|value| valid_counter(value))
            && (self.context.identity.has_plan_handle() || counters.iter().all(|value| value.is_none()))
            && self.last_execution_without_offset.as_deref().is_none_or(|v| {
                v.len() <= 64 && chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
            })
            && valid_reset(&self.reset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_grants_keep_native_identity_independent_of_current_values_and_observation_limits_are_explicit() {
        let mut identity = CachedStatementIdentity {
            plan_handle: "0x0102".into(),
            sql_handle: "0x0304".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2020-01-01T10:00:00.0030000".into(),
            plan_generation: "1".into(),
        };
        assert!(identity.is_valid());
        let original = identity.key().unwrap();
        let mut equivalent = identity.clone();
        equivalent.created_without_offset = "2020-01-01T10:00:00.003".into();
        equivalent.plan_generation = "01".into();
        assert_eq!(equivalent.key().unwrap(), original);
        equivalent.plan_handle = "0xAABB".into();
        let upper = equivalent.key().unwrap();
        equivalent.plan_handle = "0xaabb".into();
        assert_eq!(equivalent.key().unwrap(), upper);
        identity.plan_generation = "2".into();
        assert_ne!(identity.key().unwrap(), original);
        identity.statement_start_bytes = 3;
        assert!(!identity.is_valid(), "UTF-16 offsets are byte pairs");
        let mut budget = CollectionBudget { max_snapshot_rows: 100_000, ..Default::default() };
        assert!(budget.validate().is_ok());
        budget.max_snapshot_rows += 1;
        assert!(budget.validate().is_err());
    }
}
