//! Length-bounded native definitions. Identity and payload availability precede consumer redaction.
use super::{catalog_sql, decode, payload::Payload, reader::Reader};
use crate::{
    read::{NativeRead, Parameter, integer, invalid_data},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{NativePlanIdentity, NativeQueryIdentity},
};
use std::collections::BTreeMap;
const LENGTH_BATCH: usize = 128;
const PAGE_NATIVE_BYTES: usize = 8 * 1024 * 1024;
const METADATA_ALLOWANCE: usize = 16 * 1024;
const ENCODING_ALLOWANCE: usize = 3;
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Query,
    Plan,
}
pub(crate) struct Definition {
    pub id: String,
    pub origin: culprit_core::telemetry::StatementOrigin,
    pub query: Option<NativeQueryIdentity>,
    pub plan: Option<NativePlanIdentity>,
    pub object_name: Option<String>,
    pub payload: Payload,
}
pub(crate) async fn read<R: NativeRead>(
    reader: &mut Reader<R>,
    ids: &[String],
    kind: Kind,
    limit: usize,
) -> Result<Vec<Definition>> {
    let mut ids = ids
        .iter()
        .map(|id| id.parse::<i64>().ok().filter(|id| *id >= 0).ok_or_else(invalid_data))
        .collect::<Result<Vec<_>>>()?;
    ids.sort_unstable();
    ids.dedup();
    let (length_sql, payload_sql) = match kind {
        Kind::Query => (catalog_sql::QUERY_LENGTHS, catalog_sql::QUERY_DEFINITIONS),
        Kind::Plan => (catalog_sql::PLAN_LENGTHS, catalog_sql::PLAN_DEFINITIONS),
    };
    let mut definitions = Vec::new();
    for batch in ids.chunks(LENGTH_BATCH) {
        let selected = batch.iter().map(i64::to_string).collect::<Vec<_>>();
        let lengths = reader.read(length_sql, vec![super::reader::ids_parameter(&selected)?], batch.len()).await?;
        if lengths.len() != batch.len() {
            return Err(invalid_data());
        }
        let mut by_id = BTreeMap::new();
        for row in &lengths {
            if by_id.insert(integer(row, "id")?, size(row)?).is_some() {
                return Err(invalid_data());
            }
        }
        if by_id.keys().copied().collect::<Vec<_>>() != batch {
            return Err(invalid_data());
        }
        let mut start = 0;
        while start < batch.len() {
            let mut used = 0usize;
            let mut end = start;
            while end < batch.len() {
                let bytes = by_id[&batch[end]].filter(|bytes| *bytes <= limit).unwrap_or(0);
                let size = bytes.saturating_mul(ENCODING_ALLOWANCE).saturating_add(METADATA_ALLOWANCE);
                if used + size > PAGE_NATIVE_BYTES {
                    break;
                }
                used += size;
                end += 1;
            }
            if end == start {
                return Err(culprit_core::telemetry::CollectionStop::ByteLimit.error());
            }
            let selected = batch[start..end].iter().map(i64::to_string).collect::<Vec<_>>();
            let rows = reader
                .read(
                    payload_sql,
                    vec![super::reader::ids_parameter(&selected)?, Parameter::Integer(limit as i64)],
                    selected.len(),
                )
                .await?;
            if rows.len() != selected.len() {
                return Err(invalid_data());
            }
            let mut decoded = BTreeMap::new();
            for row in &rows {
                let id = integer(row, "id")?;
                if !batch[start..end].contains(&id) || decoded.contains_key(&id) {
                    return Err(invalid_data());
                }
                let query = if row.str("query_id").is_some() { Some(decode::query_identity(row)?) } else { None };
                let plan = if matches!(kind, Kind::Plan) { decode::plan_identity(row)? } else { None };
                let payload = Payload::read(row, size(row)?, limit)?;
                if matches!(kind, Kind::Query) && query.as_ref().is_some_and(|query| query.query_id != id.to_string())
                    || matches!(kind, Kind::Plan) && plan.as_ref().is_some_and(|plan| plan.plan_id != id.to_string())
                    || query.is_none() && payload.text().is_some()
                {
                    return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
                }
                decoded.insert(
                    id,
                    Definition {
                        id: id.to_string(),
                        origin: crate::statement_origin::read(row, payload.text()).origin,
                        query,
                        plan,
                        object_name: row.str("object_name"),
                        payload,
                    },
                );
            }
            definitions.extend(decoded.into_values());
            start = end;
        }
    }
    Ok(definitions)
}
fn size(row: &Rec) -> Result<Option<usize>> {
    use crate::value::Val;
    match row.columns().find(|(name, _)| *name == "source_bytes").map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Int(value)) if *value >= 0 => Ok(Some((*value).try_into().map_err(|_| invalid_data())?)),
        _ => Err(invalid_data()),
    }
}
