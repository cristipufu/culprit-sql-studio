//! Query Store wait facts stay in milliseconds and retain their native category.
use crate::{
    read::{integer, invalid_data, text},
    value::Rec,
};
use culprit_core::Result;
pub(crate) const RELATION: &str =
    "WITH culprit_waits AS (SELECT /* culprit */ w.* FROM sys.query_store_wait_stats w)\n";
pub(crate) struct Summary {
    pub plan_id: Option<String>,
    pub category: String,
    pub total_ms: u64,
}
impl Summary {
    pub(crate) fn decode(row: &Rec, by_plan: bool) -> Result<Self> {
        Ok(Self {
            plan_id: if by_plan { Some(integer(row, "plan_id")?.to_string()) } else { None },
            category: text(row, "wait_category_desc")?,
            total_ms: integer(row, "wait_ms")?.try_into().map_err(|_| invalid_data())?,
        })
    }
}
pub(crate) fn summary_sql(by_plan: bool) -> String {
    let declaration = if by_plan { "DECLARE /* culprit */ @ids xml=CONVERT(xml,@P2);\n" } else { "" };
    let plan = if by_plan { "ws.plan_id," } else { "" };
    let selected = if by_plan {
        " AND ws.plan_id IN(SELECT x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))"
    } else {
        ""
    };
    format!(
        r#"{declaration}{RELATION}SELECT /* culprit */ {plan} ws.wait_category_desc,SUM(ws.total_query_wait_time_ms) AS wait_ms
FROM culprit_waits ws
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=ws.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id=ws.plan_id
JOIN sys.query_store_query q ON q.query_id=p.query_id
JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE i.end_time>=DATEADD(minute,-@P1,SYSDATETIMEOFFSET()) AND {workload}{selected}
GROUP BY {plan} ws.wait_category_desc OPTION(MAXDOP 1)"#,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    )
}
