//! Fetch only referenced definitions. Length probes pack bounded payload batches; SQL never truncates a payload.
use crate::query_store::{catalog_sql as sql, decode};
use crate::read::{NativeRead, Parameter, ReadError};
use crate::value::Rec;
use culprit_core::{
    Engine, Result,
    telemetry::{
        CollectionBudget, CollectionDescription, CollectionRead, CollectionStop, DefinitionPayload, MAX_CURSOR_BYTES,
        NativeDefinitionReference, NativeIdentity, NativePlanIdentity, NativeQueryIdentity, PayloadUnavailable,
        PlanDefinition, QueryDefinition, StatementReferences, TelemetryRecord, TelemetryStream,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_REFERENCED_PLANS: usize = 10_000;
const LENGTH_BATCH_ROWS: usize = 128;
const RECORD_OVERHEAD_BYTES: usize = 4096;
// A UTF-16 source code unit can become a six-byte JSON escape. Redaction replacements fit the metadata reserve.
const JSON_BYTES_PER_SOURCE_BYTE: usize = 3;

#[derive(Default)]
pub(super) struct Definitions {
    plans: BTreeMap<i64, NativePlanIdentity>,
    queries: BTreeMap<i64, NativeQueryIdentity>,
    missing: BTreeSet<i64>,
    frozen: bool,
}

#[derive(Serialize, Deserialize)]
struct DefinitionCursor {
    stream: TelemetryStream,
    interval: String,
    offset: usize,
}

impl Definitions {
    pub(super) fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub(super) fn reference(&mut self, row: &Rec, source: &CollectionDescription) -> Result<StatementReferences> {
        if self.frozen {
            return Err(CollectionStop::InvalidPage.error());
        }
        let id = decode::integer(row, "plan_id")?;
        if !self.plans.contains_key(&id)
            && !self.missing.contains(&id)
            && self.plans.len() + self.missing.len() >= MAX_REFERENCED_PLANS
        {
            return Err(CollectionStop::RowLimit.error());
        }
        let Some(identity) = decode::plan_identity(row)? else {
            if self.plans.contains_key(&id) {
                return Err(CollectionStop::SourceChanged.error());
            }
            self.missing.insert(id);
            return Ok(StatementReferences {
                query: None,
                plan: Some(NativeDefinitionReference {
                    native: NativeIdentity::new(id.to_string()),
                    definition_id: None,
                }),
            });
        };
        let query_id = identity.query.query_id.parse().map_err(|_| decode::malformed())?;
        if identity.plan_id != id.to_string()
            || self.missing.contains(&id)
            || self.plans.get(&id).is_some_and(|before| before != &identity)
            || self.queries.get(&query_id).is_some_and(|before| before != &identity.query)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let references = StatementReferences {
            query: Some(NativeDefinitionReference {
                native: identity.query.native(),
                definition_id: Some(identity.query.definition_id(source)?),
            }),
            plan: Some(NativeDefinitionReference {
                native: identity.native(),
                definition_id: Some(identity.definition_id(source)?),
            }),
        };
        self.queries.insert(query_id, identity.query.clone());
        self.plans.insert(id, identity);
        Ok(references)
    }

    pub(super) async fn page<R: NativeRead>(
        &mut self,
        session: &mut R,
        source: &CollectionDescription,
        budget: &CollectionBudget,
        stream: TelemetryStream,
        token: Option<&str>,
    ) -> std::result::Result<CollectionRead, ReadError> {
        self.frozen = true;
        let offset =
            cursor_offset(token, stream, &source.query_history().ok_or_else(decode::malformed)?.interval.native_id)?;
        let ids: Vec<_> = match stream {
            TelemetryStream::QueryDefinitions => self.queries.keys().copied().collect(),
            TelemetryStream::PlanDefinitions => self.plans.keys().copied().collect(),
            _ => return Err(decode::malformed().into()),
        };
        if offset > ids.len() {
            return Err(decode::malformed().into());
        }
        if offset == ids.len() {
            return Ok(if stream == TelemetryStream::PlanDefinitions && !self.missing.is_empty() {
                CollectionRead::Stopped(CollectionStop::MissingDefinitions)
            } else {
                CollectionRead::Page { records: vec![], next_cursor: None }
            });
        }
        let end = (offset + LENGTH_BATCH_ROWS.min(budget.page_rows as usize)).min(ids.len());
        let lengths_sql =
            if stream == TelemetryStream::QueryDefinitions { sql::QUERY_LENGTHS } else { sql::PLAN_LENGTHS };
        let rows = session.query(lengths_sql, vec![ids_parameter(&ids[offset..end])?], end - offset).await?;
        if rows.len() != end - offset
            || rows.iter().zip(&ids[offset..end]).any(|(row, id)| decode::integer(row, "id").ok() != Some(*id))
        {
            return Err(decode::malformed().into());
        }
        let limit = payload_limit(stream, budget)?;
        let count = packed_prefix(&rows, budget.page_bytes as usize, limit)?;
        let selected = &ids[offset..offset + count];
        let payload_sql =
            if stream == TelemetryStream::QueryDefinitions { sql::QUERY_DEFINITIONS } else { sql::PLAN_DEFINITIONS };
        let rows =
            session.query(payload_sql, vec![ids_parameter(selected)?, Parameter::Integer(limit as i64)], count).await?;
        if rows.len() != count {
            return Err(decode::malformed().into());
        }
        let mut records = Vec::with_capacity(count);
        for (row, id) in rows.iter().zip(selected) {
            if decode::integer(row, "id")? != *id {
                return Err(decode::malformed().into());
            }
            records.push(self.decode(row, *id, source, stream, limit)?);
        }
        let next = offset + count;
        let next_cursor =
            if next < ids.len() || (stream == TelemetryStream::PlanDefinitions && !self.missing.is_empty()) {
                Some(
                    serde_json::to_string(&DefinitionCursor {
                        stream,
                        interval: source.query_history().ok_or_else(decode::malformed)?.interval.native_id.clone(),
                        offset: next,
                    })
                    .map_err(|_| decode::malformed())?,
                )
            } else {
                None
            };
        Ok(CollectionRead::Page { records, next_cursor })
    }

    fn decode(
        &self,
        row: &Rec,
        id: i64,
        source: &CollectionDescription,
        stream: TelemetryStream,
        limit: usize,
    ) -> Result<TelemetryRecord> {
        let bytes = optional_size(row)?;
        match stream {
            TelemetryStream::QueryDefinitions => {
                let identity = decode::query_identity(row).map_err(|_| CollectionStop::SourceChanged.error())?;
                if self.queries.get(&id) != Some(&identity) {
                    return Err(CollectionStop::SourceChanged.error());
                }
                let context = [
                    "set_options",
                    "language_id",
                    "date_format",
                    "date_first",
                    "status",
                    "required_cursor_options",
                    "acceptable_cursor_options",
                    "merge_action_type",
                    "default_schema_id",
                ]
                .into_iter()
                .filter_map(|name| row.str(name).map(|value| (name.into(), value)))
                .collect();
                let native = native_payload(row, bytes, limit);
                let provenance = crate::statement_origin::read(row, native.text());
                Ok(TelemetryRecord::QueryDefinition(QueryDefinition {
                    origin: provenance.origin,
                    monitoring_template_id: provenance.template_id,
                    id: identity.definition_id(source)?,
                    identity,
                    context,
                    context_available: row.str("observed_context_settings_id").is_some(),
                    source_text_bytes: bytes.map(|bytes| bytes.to_string()),
                    sql: payload(native, stream),
                }))
            }
            TelemetryStream::PlanDefinitions => {
                let identity = decode::plan_identity(row)?.ok_or_else(|| CollectionStop::SourceChanged.error())?;
                if self.plans.get(&id) != Some(&identity) {
                    return Err(CollectionStop::SourceChanged.error());
                }
                Ok(TelemetryRecord::PlanDefinition(PlanDefinition {
                    id: identity.definition_id(source)?,
                    query_definition_id: identity.query.definition_id(source)?,
                    identity,
                    engine_version: Some(decode::text(row, "engine_version")?),
                    native_properties: [(
                        "compatibility_level".into(),
                        u16::try_from(decode::integer(row, "compatibility_level")?)
                            .map_err(|_| decode::malformed())?
                            .to_string(),
                    )]
                    .into(),
                    source_text_bytes: bytes.map(|bytes| bytes.to_string()),
                    plan: payload(native_payload(row, bytes, limit), stream),
                }))
            }
            _ => Err(decode::malformed()),
        }
    }
}

fn ids_parameter(ids: &[i64]) -> Result<Parameter> {
    crate::query_store::reader::ids_parameter(&ids.iter().map(i64::to_string).collect::<Vec<_>>())
}
fn cursor_offset(token: Option<&str>, stream: TelemetryStream, interval: &str) -> Result<usize> {
    let Some(token) = token else {
        return Ok(0);
    };
    if token.len() > MAX_CURSOR_BYTES {
        return Err(decode::malformed());
    }
    let cursor: DefinitionCursor = serde_json::from_str(token).map_err(|_| decode::malformed())?;
    if cursor.stream != stream || cursor.interval != interval {
        return Err(decode::malformed());
    }
    Ok(cursor.offset)
}
fn optional_size(row: &Rec) -> Result<Option<usize>> {
    row.str("source_bytes")
        .map(|_| {
            decode::integer(row, "source_bytes").and_then(|bytes| bytes.try_into().map_err(|_| decode::malformed()))
        })
        .transpose()
}
fn payload_limit(stream: TelemetryStream, budget: &CollectionBudget) -> std::result::Result<usize, ReadError> {
    let max =
        if stream == TelemetryStream::QueryDefinitions { budget.max_sql_bytes } else { budget.max_plan_bytes } as usize;
    let limit = (budget.page_bytes as usize).saturating_sub(RECORD_OVERHEAD_BYTES) / JSON_BYTES_PER_SOURCE_BYTE;
    if limit == 0 {
        return Err(ReadError::Stopped(CollectionStop::ByteLimit));
    }
    Ok(limit.min(max))
}
fn packed_prefix(rows: &[Rec], page_bytes: usize, payload_limit: usize) -> Result<usize> {
    let mut used = 0;
    let mut count = 0;
    for row in rows {
        let size = optional_size(row)?.filter(|bytes| *bytes <= payload_limit).unwrap_or(0);
        let estimated = size.saturating_mul(JSON_BYTES_PER_SOURCE_BYTE).saturating_add(RECORD_OVERHEAD_BYTES);
        if used + estimated > page_bytes {
            break;
        }
        used += estimated;
        count += 1;
    }
    if count == 0 {
        return Err(CollectionStop::ByteLimit.error());
    }
    Ok(count)
}
fn native_payload(row: &Rec, bytes: Option<usize>, limit: usize) -> crate::query_store::payload::Payload {
    crate::query_store::payload::Payload::read(row, bytes, limit)
        .unwrap_or(crate::query_store::payload::Payload::Unavailable(PayloadUnavailable::SourceUnavailable))
}
fn payload(native: crate::query_store::payload::Payload, stream: TelemetryStream) -> DefinitionPayload {
    use crate::query_store::payload::Payload;
    match native {
        Payload::Text(raw) => match stream {
            TelemetryStream::QueryDefinitions => DefinitionPayload::sql(&raw, Engine::Mssql),
            TelemetryStream::PlanDefinitions => DefinitionPayload::plan(&raw, Engine::Mssql),
            _ => DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing },
        },
        Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;
    fn export_row(row: &Rec, stream: TelemetryStream, bytes: Option<usize>, limit: usize) -> DefinitionPayload {
        payload(native_payload(row, bytes, limit), stream)
    }

    #[test]
    fn length_probes_pack_complete_values_and_oversized_values_remain_explicit() {
        let rows = vec![Rec::of(&[("source_bytes", Val::Int(1000))]), Rec::of(&[("source_bytes", Val::Int(1000))])];
        assert_eq!(packed_prefix(&rows, 10_000, 2000).unwrap(), 1);
        let oversized =
            Rec::of(&[("source_bytes", Val::Int(1_000_000)), ("payload", Val::Text("must not escape".into()))]);
        assert_eq!(packed_prefix(std::slice::from_ref(&oversized), 10_000, 2000).unwrap(), 1);
        assert_eq!(
            export_row(&oversized, TelemetryStream::PlanDefinitions, Some(1_000_000), 2000),
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
        );
        assert!(matches!(
            payload_limit(
                TelemetryStream::PlanDefinitions,
                &CollectionBudget { page_bytes: 1024, ..CollectionBudget::default() }
            ),
            Err(ReadError::Stopped(CollectionStop::ByteLimit))
        ));
    }
    #[test]
    fn encrypted_restricted_missing_and_malformed_payloads_never_export_text() {
        for (flag, reason) in
            [("restricted", PayloadUnavailable::Restricted), ("encrypted", PayloadUnavailable::Encrypted)]
        {
            let row = Rec::of(&[(flag, Val::Bool(true)), ("payload", Val::Text("secret".into()))]);
            assert_eq!(
                export_row(&row, TelemetryStream::QueryDefinitions, Some(12), 100),
                DefinitionPayload::Unavailable { reason }
            );
        }
        assert_eq!(
            export_row(&Rec::default(), TelemetryStream::QueryDefinitions, None, 100),
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing }
        );
        let row = Rec::of(&[("payload", Val::Text("<ShowPlanXML><broken>".into()))]);
        assert_eq!(
            export_row(
                &row,
                TelemetryStream::PlanDefinitions,
                Some("<ShowPlanXML><broken>".encode_utf16().count() * 2),
                100
            ),
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::RedactionFailed }
        );
        let row = Rec::of(&[("payload", Val::Text("SELECT N'secret' /* private comment */".into()))]);
        let DefinitionPayload::Available { text, .. } = export_row(
            &row,
            TelemetryStream::QueryDefinitions,
            Some("SELECT N'secret' /* private comment */".encode_utf16().count() * 2),
            1000,
        ) else {
            panic!("available")
        };
        assert!(!text.contains("secret") && !text.contains("private comment"));
    }
    #[test]
    fn definition_cursors_are_scoped_to_the_frozen_local_inventory() {
        let token = serde_json::to_string(&DefinitionCursor {
            stream: TelemetryStream::QueryDefinitions,
            interval: "7".into(),
            offset: 12,
        })
        .unwrap();
        assert_eq!(cursor_offset(Some(&token), TelemetryStream::QueryDefinitions, "7").unwrap(), 12);
        assert!(cursor_offset(Some(&token), TelemetryStream::PlanDefinitions, "7").is_err());
        assert!(cursor_offset(Some(&token), TelemetryStream::QueryDefinitions, "8").is_err());
        assert!(
            cursor_offset(Some(&"x".repeat(MAX_CURSOR_BYTES + 1)), TelemetryStream::QueryDefinitions, "7").is_err()
        );
    }
}
