use crate::{
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_integer,
        optional_signed_integer, optional_text, text,
    },
    sql_time::{native_time, utc_local_iso, utc_offset_precise_iso},
    value::Rec,
};
use culprit_core::{Result, Source, telemetry::*};

const LAYOUT: &str = r#"SELECT
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility,
 CASE WHEN OBJECT_ID(N'sys.database_automatic_tuning_options') IS NOT NULL THEN 1 ELSE 0 END AS automatic_tuning,
 CASE WHEN OBJECT_ID(N'sys.dm_db_tuning_recommendations') IS NOT NULL THEN 1 ELSE 0 END AS recommendations,
 CASE WHEN OBJECT_ID(N'sys.query_store_plan') IS NOT NULL THEN 1 ELSE 0 END AS query_store,
 CASE WHEN OBJECT_ID(N'sys.query_store_query_hints') IS NOT NULL THEN 1 ELSE 0 END AS hints,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_plan') AND name=N'plan_forcing_type_desc') THEN 1 ELSE 0 END AS forcing_type,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_query_hints') AND name=N'replica_group_id') THEN 1 ELSE 0 END AS hint_replica,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_query_hints') AND name=N'source') THEN 1 ELSE 0 END AS hint_source"#;
const PAYLOAD_NATIVE_BYTES: i64 = 256 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Options,
    Recommendations,
    ForcedPlans,
    Hints,
}
impl Kind {
    pub const ALL: [Self; 4] = [Self::Options, Self::Recommendations, Self::ForcedPlans, Self::Hints];
    pub fn source(self) -> &'static Source {
        use crate::sources;
        match self {
            Self::Options => &sources::TUNING_OPTIONS,
            Self::Recommendations => &sources::TUNING_RECOMMENDATIONS,
            Self::ForcedPlans => &sources::FORCED_PLANS,
            Self::Hints => &sources::QUERY_HINTS,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Layout {
    metadata_visible: bool,
    automatic_tuning: bool,
    recommendations: bool,
    query_store: bool,
    forcing_type: bool,
    hints: bool,
    hint_replica: bool,
    hint_source: bool,
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let rows = reader.query(LAYOUT, vec![], 2).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let flag = |name| -> Result<bool> {
            match integer(row, name)? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(invalid_data()),
            }
        };
        Ok(Self {
            metadata_visible: crate::read::optional_boolean(row, "metadata_visibility")? == Some(true),
            automatic_tuning: flag("automatic_tuning")?,
            recommendations: flag("recommendations")?,
            query_store: flag("query_store")?,
            forcing_type: flag("forcing_type")?,
            hints: flag("hints")?,
            hint_replica: flag("hint_replica")?,
            hint_source: flag("hint_source")?,
        })
    }
    pub fn available(&self, kind: Kind) -> Result<bool> {
        let supported = match kind {
            Kind::Options => self.automatic_tuning,
            Kind::Recommendations => self.recommendations,
            Kind::ForcedPlans => self.query_store,
            Kind::Hints => self.query_store && self.hints,
        };
        if !self.metadata_visible && !supported {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::PermissionDenied,
                culprit_core::t!("provider.mssql.tuning.metadata_unavailable"),
            )
            .with_hint("GRANT VIEW DEFINITION TO [<user>];"));
        }
        Ok(supported)
    }
}
pub(crate) async fn read_kind<R: NativeRead>(
    reader: &mut R,
    kind: Kind,
    layout: &Layout,
    limit: usize,
) -> Result<Option<Vec<TuningFact>>> {
    if !layout.available(kind)? {
        return Ok(None);
    }
    if limit == 0 || limit > 10_000 {
        return Err(CollectionStop::RowLimit.error());
    }
    let sql=match kind {
        Kind::Options=>"SELECT TOP (@P1) name,desired_state_desc,actual_state_desc,reason_desc FROM sys.database_automatic_tuning_options ORDER BY name OPTION(MAXDOP 1)".into(),
        Kind::Recommendations=>recommendations_sql(),
        Kind::ForcedPlans=>forced_plans_sql(layout.forcing_type),
        Kind::Hints=>hints_sql(layout),
    };
    let mut parameters = vec![Parameter::Integer((limit + 1) as i64)];
    if matches!(kind, Kind::Hints) {
        parameters.push(Parameter::Integer(PAYLOAD_NATIVE_BYTES));
    }
    let rows = reader.query(&sql, parameters, limit + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut keys = std::collections::BTreeSet::new();
    let facts = rows
        .iter()
        .map(|row| {
            let fact = decode(row, kind, layout)?;
            if !fact.is_valid() || !keys.insert(fact.key()) {
                return Err(invalid_data());
            }
            Ok(fact)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(facts))
}
pub(super) fn recommendations_sql() -> String {
    format!(
        r#"SELECT TOP (@P1) name,type,reason,score,valid_since AS native_valid_since,last_refresh,
 execute_action_start_time AS native_applied,is_executable_action,is_revertable_action,
 {valid} AS valid_since,{applied} AS applied_at,
 JSON_VALUE(state,'$.currentValue') AS state,JSON_VALUE(state,'$.reason') AS state_reason,
 JSON_VALUE(details,'$.planForceDetails.queryId') AS query_id,
 JSON_VALUE(details,'$.planForceDetails.regressedPlanId') AS regressed_plan_id,
 JSON_VALUE(details,'$.planForceDetails.recommendedPlanId') AS recommended_plan_id,
 JSON_VALUE(details,'$.planForceDetails.regressedPlanExecutionCount') AS regressed_executions,
 JSON_VALUE(details,'$.planForceDetails.recommendedPlanExecutionCount') AS recommended_executions,
 JSON_VALUE(details,'$.planForceDetails.regressedPlanAbortedCount') AS regressed_aborted,
 JSON_VALUE(details,'$.planForceDetails.recommendedPlanAbortedCount') AS recommended_aborted,
 JSON_VALUE(details,'$.planForceDetails.regressedPlanCpuTimeAverage') AS regressed_cpu,
 JSON_VALUE(details,'$.planForceDetails.recommendedPlanCpuTimeAverage') AS recommended_cpu,
 JSON_VALUE(details,'$.planForceDetails.regressedPlanCpuTimeStddev') AS regressed_cpu_stdev,
 JSON_VALUE(details,'$.planForceDetails.recommendedPlanCpuTimeStddev') AS recommended_cpu_stdev,
 JSON_VALUE(details,'$.implementationDetails.script') AS script,
 COALESCE(JSON_VALUE(details,'$.createIndexDetails.schema'),JSON_VALUE(details,'$.dropIndexDetails.schema')) AS index_schema,
 COALESCE(JSON_VALUE(details,'$.createIndexDetails.table'),JSON_VALUE(details,'$.dropIndexDetails.table')) AS index_table,
 COALESCE(JSON_VALUE(details,'$.createIndexDetails.indexName'),JSON_VALUE(details,'$.dropIndexDetails.indexName')) AS index_name,
 COALESCE(JSON_VALUE(details,'$.createIndexDetails.indexColumns'),JSON_VALUE(details,'$.dropIndexDetails.indexColumns')) AS index_columns,
 COALESCE(JSON_VALUE(details,'$.createIndexDetails.includedColumns'),JSON_VALUE(details,'$.dropIndexDetails.includedColumns')) AS included_columns,
 (SELECT {last} FROM sys.query_store_plan p WHERE p.query_id=TRY_CAST(JSON_VALUE(details,'$.planForceDetails.queryId') AS bigint)) AS last_execution
 FROM sys.dm_db_tuning_recommendations ORDER BY name OPTION(MAXDOP 1)"#,
        valid = utc_local_iso("valid_since"),
        applied = utc_local_iso("execute_action_start_time"),
        last = utc_offset_precise_iso("MAX(p.last_execution_time)")
    )
}
pub(super) fn forced_plans_sql(with_forcing: bool) -> String {
    format!(
        "SELECT TOP (@P1) query_id,plan_id,force_failure_count,last_force_failure_reason_desc,{last} AS last_execution,{forcing} AS forcing FROM sys.query_store_plan WHERE is_forced_plan=1 ORDER BY plan_id OPTION(MAXDOP 1)",
        last = utc_offset_precise_iso("last_execution_time"),
        forcing = if with_forcing { "plan_forcing_type_desc" } else { "CAST(NULL AS nvarchar(60))" }
    )
}
fn hints_sql(layout: &Layout) -> String {
    format!(
        "SELECT TOP (@P1) query_hint_id,query_id,{replica} AS replica_group_id,DATALENGTH(query_hint_text) AS source_bytes,CASE WHEN DATALENGTH(query_hint_text)<=@P2 THEN query_hint_text END AS query_hint_text,query_hint_failure_count,last_query_hint_failure_reason_desc,{source} FROM sys.query_store_query_hints ORDER BY query_hint_id{order} OPTION(MAXDOP 1)",
        replica = if layout.hint_replica { "replica_group_id" } else { "CAST(NULL AS bigint)" },
        source = if layout.hint_source {
            "source AS native_source,source_desc AS native_source_name"
        } else {
            "CAST(NULL AS int) AS native_source,CAST(NULL AS nvarchar(128)) AS native_source_name"
        },
        order = if layout.hint_replica { ",replica_group_id" } else { "" }
    )
}
fn decode(row: &Rec, kind: Kind, layout: &Layout) -> Result<TuningFact> {
    Ok(match kind {
        Kind::Options => TuningFact::AutomaticOption(culprit_core::AutomaticTuningOption {
            name: text(row, "name")?,
            desired: text(row, "desired_state_desc")?,
            actual: text(row, "actual_state_desc")?,
            reason: optional_text(row, "reason_desc")?,
        }),
        Kind::Recommendations => TuningFact::Recommendation(Box::new(recommendation(row)?)),
        Kind::ForcedPlans => TuningFact::ForcedPlan(NativeForcedPlan {
            query_id: integer(row, "query_id")?.to_string(),
            plan_id: integer(row, "plan_id")?.to_string(),
            failures: integer(row, "force_failure_count")?.to_string(),
            last_failure_reason: optional_text(row, "last_force_failure_reason_desc")?,
            last_execution_at: instant(row, "last_execution")?,
            forcing: optional_text(row, "forcing")?,
            reset: culprit_core::ResetMarker::Unknown,
        }),
        Kind::Hints => TuningFact::QueryHint(NativeQueryHint {
            hint_id: integer(row, "query_hint_id")?.to_string(),
            query_id: integer(row, "query_id")?.to_string(),
            scope: hint_scope(row, layout)?,
            hint: hint_payload(row)?,
            failures: integer(row, "query_hint_failure_count")?.to_string(),
            last_failure_reason: optional_text(row, "last_query_hint_failure_reason_desc")?,
            native_source: optional_signed_integer(row, "native_source")?
                .map(i32::try_from)
                .transpose()
                .map_err(|_| invalid_data())?,
            native_source_name: optional_text(row, "native_source_name")?,
            reset: culprit_core::ResetMarker::Unknown,
        }),
    })
}
fn hint_scope(row: &Rec, layout: &Layout) -> Result<culprit_core::QueryStoreHintScope> {
    use culprit_core::QueryStoreHintScope;
    Ok(match (layout.hint_replica, optional_integer(row, "replica_group_id")?) {
        (true, Some(id)) => QueryStoreHintScope::ReplicaGroup { id: id.to_string() },
        (false, Some(_)) => return Err(invalid_data()),
        (false, _) if layout.metadata_visible => QueryStoreHintScope::Local,
        _ => QueryStoreHintScope::Unknown,
    })
}
fn recommendation(row: &Rec) -> Result<NativeTuningRecommendation> {
    let kind = text(row, "type")?;
    let script = optional_text(row, "script")?;
    let index =
        kind.to_ascii_lowercase().contains("index").then(|| super::recommended_index(row, script.as_deref())).flatten();
    Ok(NativeTuningRecommendation {
        name: text(row, "name")?,
        kind,
        reason: optional_text(row, "reason")?,
        state: optional_text(row, "state")?,
        state_reason: optional_text(row, "state_reason")?,
        score: optional_integer(row, "score")?.map(u8::try_from).transpose().map_err(|_| invalid_data())?,
        query_id: json_counter(row, "query_id")?,
        regressed: plan_statistics(row, "regressed")?,
        recommended: plan_statistics(row, "recommended")?,
        script: script
            .map(unredacted_payload)
            .unwrap_or(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }),
        index,
        valid_since_without_offset: local(row, "native_valid_since")?,
        last_refresh_without_offset: local(row, "last_refresh")?,
        applied_without_offset: local(row, "native_applied")?,
        valid_since_utc_estimate: instant(row, "valid_since")?,
        applied_utc_estimate: instant(row, "applied_at")?,
        last_execution_at: instant(row, "last_execution")?,
        is_executable: optional_boolean(row, "is_executable_action")?,
        is_revertable: optional_boolean(row, "is_revertable_action")?,
    })
}
fn plan_statistics(row: &Rec, prefix: &str) -> Result<TuningPlanStatistics> {
    Ok(TuningPlanStatistics {
        plan_id: json_counter(row, &format!("{prefix}_plan_id"))?,
        executions: json_counter(row, &format!("{prefix}_executions"))?,
        aborted: json_counter(row, &format!("{prefix}_aborted"))?,
        avg_cpu_us: json_number(row, &format!("{prefix}_cpu"))?,
        stdev_cpu_us: json_number(row, &format!("{prefix}_cpu_stdev"))?,
    })
}
fn json_counter(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|v| v.parse::<u64>().map(|v| v.to_string()).map_err(|_| invalid_data())).transpose()
}
fn json_number(row: &Rec, name: &str) -> Result<Option<f64>> {
    optional_text(row, name)?
        .map(|v| v.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(invalid_data))
        .transpose()
}
fn local(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|v| native_time(&v)).transpose()
}
fn instant(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|v| canonical_time(&v)).transpose()
}
fn hint_payload(row: &Rec) -> Result<DefinitionPayload> {
    let bytes = optional_integer(row, "source_bytes")?;
    match (bytes, optional_text(row, "query_hint_text")?) {
        (Some(bytes), None) if bytes > PAYLOAD_NATIVE_BYTES => {
            Ok(DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized })
        }
        (Some(bytes), Some(value))
            if bytes <= PAYLOAD_NATIVE_BYTES && value.encode_utf16().count() as i64 * 2 == bytes =>
        {
            Ok(unredacted_payload(value))
        }
        _ => Err(invalid_data()),
    }
}
/// Native application display keeps SQL intact. ExportedRecord owns redaction before any durable write.
fn unredacted_payload(text: String) -> DefinitionPayload {
    DefinitionPayload::Available { sha256: digest(text.as_bytes()), text, redaction_version: REDACTION_VERSION }
}

#[cfg(test)]
pub(crate) mod tests;
