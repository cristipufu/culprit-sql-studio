//! Length-bounded native definitions. Identity and payload availability precede consumer redaction.
use super::{catalog_sql, decode, payload::Payload, reader::Reader};
use crate::{
    read::{NativeRead, Parameter, ReadError, ReadResult, integer, invalid_data},
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
    pub variant_family: Option<culprit_core::QueryVariantFamily>,
    pub object_name: Option<String>,
    pub payload: Payload,
}

/// One referenced batch, including optional native family metadata. Both interactive and telemetry readers use the
/// same projection and exact one-row-per-ID admission before either interprets payloads.
pub(crate) async fn read_payload<R: NativeRead>(
    reader: &mut R,
    ids: &[String],
    kind: Kind,
    limit: usize,
    with_families: bool,
) -> ReadResult<Vec<Rec>> {
    let sql = match kind {
        Kind::Query => catalog_sql::query_definitions(with_families),
        Kind::Plan => catalog_sql::PLAN_DEFINITIONS.to_owned(),
    };
    let native_limit = i64::try_from(limit).map_err(|_| invalid_data())?;
    let row_limit = ids.len().checked_add(1).ok_or_else(invalid_data)?;
    let result =
        reader.query(&sql, vec![super::reader::ids_parameter(ids)?, Parameter::Integer(native_limit)], row_limit).await;
    let rows = match result {
        Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::RowLimit)) => {
            return Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::SourceChanged));
        }
        result => result?,
    };
    if rows.len() != ids.len()
        || rows.iter().zip(ids).any(|(row, id)| integer(row, "id").ok().map(|id| id.to_string()).as_ref() != Some(id))
    {
        return Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::SourceChanged));
    }
    Ok(rows)
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
    let length_sql = match kind {
        Kind::Query => catalog_sql::QUERY_LENGTHS,
        Kind::Plan => catalog_sql::PLAN_LENGTHS,
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
            let with_families = reader.variant_families_supported();
            let rows =
                read_payload(reader, &selected, kind, limit, with_families).await.map_err(ReadError::into_error)?;
            let mut decoded = BTreeMap::new();
            for row in &rows {
                let id = integer(row, "id")?;
                if !batch[start..end].contains(&id) || decoded.contains_key(&id) {
                    return Err(invalid_data());
                }
                let query = if row.str("query_id").is_some() {
                    Some(
                        decode::query_identity(row)
                            .map_err(|_| culprit_core::telemetry::CollectionStop::SourceChanged.error())?,
                    )
                } else {
                    None
                };
                let plan = if matches!(kind, Kind::Plan) { decode::plan_identity(row)? } else { None };
                let variant_family = if matches!(kind, Kind::Query) && reader.variant_families_supported() {
                    super::variants_sql::decode(row)?
                } else {
                    None
                };
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
                        variant_family,
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
