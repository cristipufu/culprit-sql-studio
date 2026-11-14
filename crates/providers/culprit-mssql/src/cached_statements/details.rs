//! Interactive detail selection is bounded and parameterized; only requested plans invoke the plan-text function.
use super::{DATABASE_PREDICATE, MAX_HASH_SELECTORS, hashes_parameter, relation, valid_hash};
use crate::{
    query_store::payload::Payload,
    read::{NativeRead, Parameter, ReadError, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{CollectionBudget, CollectionStop, PayloadUnavailable},
};
use std::collections::BTreeMap;

const ENCODING_ALLOWANCE: usize = 3;
const ROW_ALLOWANCE: usize = 16 * 1024;

pub(crate) struct Detail {
    pub context: culprit_core::telemetry::CachedStatementContext,
    pub statement: Payload,
    pub plan: Payload,
    pub object_name: Option<String>,
    pub module_parameters: Option<culprit_core::telemetry::MssqlModuleParameters>,
}
impl Detail {
    fn withhold_enrichment(&mut self, reason: PayloadUnavailable) {
        if self.plan.text().is_some() {
            self.plan = Payload::Unavailable(reason);
        }
        self.module_parameters = None;
        self.object_name = None;
    }
}
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    hashes: &[String],
    plans: usize,
    budget: &CollectionBudget,
) -> Result<BTreeMap<String, Detail>> {
    budget.validate()?;
    let mut unique = std::collections::BTreeSet::new();
    if !hashes.iter().all(|hash| valid_hash(hash) && unique.insert(hash.to_ascii_lowercase())) {
        return Err(invalid_data());
    }
    let (planned, text_only) = hashes.split_at(plans.min(hashes.len()));
    let mut details = BTreeMap::new();
    for (selected, with_plan) in [(planned, true), (text_only, false)] {
        if selected.is_empty() {
            continue;
        }
        for batch in selected.chunks(selectors_per_read(budget, with_plan)?) {
            read_batch(reader, batch, with_plan, budget, &mut details).await?;
        }
    }
    verify_plans(reader, &mut details).await?;
    Ok(details)
}

async fn verify_plans<R: NativeRead>(reader: &mut R, details: &mut BTreeMap<String, Detail>) -> Result<()> {
    let mut bindings = crate::relational_inputs::Bindings::default();
    let mut changed = false;
    for detail in details.values_mut() {
        if detail.plan.text().is_none() && detail.module_parameters.is_none() {
            continue;
        }
        match bindings.admit(&detail.context) {
            Ok(true) => {}
            Ok(false) => detail.withhold_enrichment(PayloadUnavailable::NotCollected),
            Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => {
                changed = true;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    if changed {
        for detail in details.values_mut() {
            detail.withhold_enrichment(PayloadUnavailable::SourceUnavailable);
        }
        return Ok(());
    }
    let verified = match bindings.verify(reader).await {
        Ok(verified) => verified,
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(_) => crate::relational_inputs::VerifiedBindings::default(),
    };
    for detail in details.values_mut() {
        if bindings.contains(&detail.context) && !verified.contains(&detail.context) {
            detail.withhold_enrichment(PayloadUnavailable::SourceUnavailable);
        }
    }
    Ok(())
}
async fn read_batch<R: NativeRead>(
    reader: &mut R,
    batch: &[String],
    with_plan: bool,
    budget: &CollectionBudget,
    details: &mut BTreeMap<String, Detail>,
) -> Result<()> {
    let sql = sql(with_plan);
    let mut pending = batch.to_vec();
    while !pending.is_empty() {
        // Reserve every row's metadata first; deferred rows still return their identity and length facts.
        let payload_budget = budget.page_bytes as usize - pending.len() * ROW_ALLOWANCE;
        let parameters = vec![
            hashes_parameter(&pending)?,
            Parameter::Integer(i64::from(budget.max_sql_bytes)),
            Parameter::Integer(i64::from(budget.max_plan_bytes)),
            Parameter::Integer(payload_budget as i64),
        ];
        let rows = reader.query(&sql, parameters, pending.len() + 1).await.map_err(ReadError::into_error)?;
        if rows.len() > pending.len() {
            return Err(CollectionStop::RowLimit.error());
        }
        let deferred = accept_rows(rows, &pending, with_plan, budget, details)?;
        if !deferred.is_empty() && deferred.len() >= pending.len() {
            return Err(CollectionStop::ByteLimit.error());
        }
        pending = deferred;
    }
    Ok(())
}
fn accept_rows(
    rows: Vec<Rec>,
    selected: &[String],
    with_plan: bool,
    budget: &CollectionBudget,
    details: &mut BTreeMap<String, Detail>,
) -> Result<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut deferred = Vec::new();
    for row in rows {
        let hash = text(&row, "query_hash")?;
        let selected =
            selected.iter().find(|selected| selected.eq_ignore_ascii_case(&hash)).ok_or_else(invalid_data)?;
        if !seen.insert(selected.clone()) || details.contains_key(selected) {
            return Err(invalid_data());
        }
        match crate::read::optional_boolean(&row, "payload_included")? {
            Some(true) => {
                details.insert(selected.clone(), decode(&row, with_plan, budget)?);
            }
            Some(false) => deferred.push(selected.clone()),
            None => return Err(invalid_data()),
        }
    }
    Ok(deferred)
}
fn selectors_per_read(budget: &CollectionBudget, with_plan: bool) -> Result<usize> {
    let bytes = budget.max_sql_bytes as usize + if with_plan { budget.max_plan_bytes as usize } else { 0 };
    let maximum_payload = bytes.saturating_mul(ENCODING_ALLOWANCE);
    let metadata_budget = (budget.page_bytes as usize).saturating_sub(maximum_payload);
    let rows = (metadata_budget / ROW_ALLOWANCE).min(MAX_HASH_SELECTORS);
    if rows == 0 {
        return Err(CollectionStop::ByteLimit.error());
    }
    Ok(rows)
}

fn decode(row: &Rec, with_plan: bool, budget: &CollectionBudget) -> Result<Detail> {
    let bytes =
        |column| optional_integer(row, column)?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose();
    let context = super::context(row)?;
    let module_parameters = if with_plan { crate::relational_inputs::parameters(row, &context)? } else { None };
    Ok(Detail {
        context,
        statement: Payload::read_column(
            row,
            "statement_text",
            bytes("statement_bytes")?,
            budget.max_sql_bytes as usize,
        )?,
        plan: if with_plan {
            Payload::read_column(row, "query_plan", bytes("plan_bytes")?, budget.max_plan_bytes as usize)?
        } else {
            Payload::Unavailable(PayloadUnavailable::NotCollected)
        },
        object_name: optional_text(row, "object_name")?,
        module_parameters,
    })
}
fn sql(with_plan: bool) -> String {
    let (plan, plan_read) = if with_plan {
        (
            "CAST(DATALENGTH(qp.query_plan) AS bigint) AS plan_bytes,qp.query_plan AS plan_value,",
            super::statement_plan_relation("r", "qp"),
        )
    } else {
        ("CAST(NULL AS bigint) AS plan_bytes,CAST(NULL AS nvarchar(max)) AS plan_value,", String::new())
    };
    format!(
        r#"DECLARE @hashes xml=CONVERT(xml,@P1);
WITH ranked AS (
 SELECT qs.query_hash,qs.sql_handle,qs.plan_handle,qs.statement_start_offset,qs.statement_end_offset,
 qs.creation_time,qs.plan_generation_num,CAST(pa.value AS int) AS compilation_database_id,
 ROW_NUMBER() OVER(PARTITION BY qs.query_hash ORDER BY qs.last_execution_time DESC,qs.total_elapsed_time DESC) AS rn
 {relation} WHERE {database} AND qs.query_hash IN (
 SELECT CONVERT(binary(8),n.h.value('@v[1]','varchar(18)'),1) FROM @hashes.nodes('/hashes/h') n(h)) AND {application}
), culprit_payloads AS (
 SELECT r.query_hash,{context},{module},CAST(DATALENGTH(body.statement_text) AS bigint) AS statement_bytes,
 body.statement_text AS statement_value,{plan} st.encrypted,
 OBJECT_SCHEMA_NAME(st.objectid,st.dbid)+'.'+OBJECT_NAME(st.objectid,st.dbid) AS object_name
 FROM ranked r OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) st
 OUTER APPLY (SELECT {statement} AS statement_text) body
 {plan_read} WHERE r.rn=1
), culprit_packed AS (
 SELECT *,SUM(CASE WHEN statement_bytes<=@P2 THEN statement_bytes*{encoding} ELSE 0 END+
 CASE WHEN plan_bytes<=@P3 THEN plan_bytes*{encoding} ELSE 0 END)
 OVER(ORDER BY query_hash ROWS UNBOUNDED PRECEDING) AS packed_bytes FROM culprit_payloads
)
SELECT {output_context},{output_module},statement_bytes,plan_bytes,encrypted,object_name,
 CAST(CASE WHEN packed_bytes<=@P4 THEN 1 ELSE 0 END AS bit) AS payload_included,
 CASE WHEN packed_bytes<=@P4 AND statement_bytes<=@P2 THEN statement_value ELSE NULL END AS statement_text,
 CASE WHEN packed_bytes<=@P4 AND plan_bytes<=@P3 THEN plan_value ELSE NULL END AS query_plan
FROM culprit_packed OPTION(MAXDOP 1)"#,
        relation = relation(),
        database = DATABASE_PREDICATE,
        application = crate::statement_origin::application_candidates_sql("st.text"),
        statement = super::statement_expression("r", "st.text"),
        encoding = ENCODING_ALLOWANCE,
        context = cache_context_columns(),
        module = crate::relational_inputs::module_columns("st"),
        output_context = "plan_handle,sql_handle,statement_start_offset,statement_end_offset,creation_time,plan_generation_num,database_id,CONVERT(varchar(18),query_hash,1) AS query_hash,source_text_bytes,text_complete,text_prefix,engine_internal,cloud_internal",
        output_module = "module_database_id,module_object_id,module_number",
    )
}

fn cache_context_columns() -> String {
    // query_hash is retained as binary for the existing packing order; every other native context field uses its
    // shared projection. Excluding the hash at construction avoids SQL string replacement or duplicate aliases.
    super::context_columns_without_hash_from("r", "st", "r.compilation_database_id")
}

#[cfg(test)]
mod tests;
