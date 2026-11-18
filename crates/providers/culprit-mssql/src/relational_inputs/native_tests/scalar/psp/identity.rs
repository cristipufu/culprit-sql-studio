//! Full original statement text, native plan/hash and cache generation are independent guards. A shared query
//! hash, module locator or PLAN PER VALUE text never selects a variant on its own.
use super::*;
use crate::relational_inputs::native_tests::NamedDefinition;
use culprit_core::telemetry::CachedStatementContext;

#[derive(Clone, Copy)]
pub(super) enum RoutingExpectation {
    Variant,
    Ordinary,
}
pub(super) struct BoundPlan {
    pub xml: usize,
    pub definition: usize,
    pub variant: Option<u32>,
}
struct NativeStatement {
    text: String,
    query_hash: String,
    plan_hash: String,
    variant: Option<u32>,
    statement_sql_handle: Option<String>,
    context_settings_id: Option<i64>,
}
fn native_statement(xml: &str) -> Option<NativeStatement> {
    culprit_core::xml::read(xml, |document| {
        let document = document.ok()?;
        if document.root_element().tag_name().name() != "ShowPlanXML" {
            return None;
        }
        let statements = document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "StmtSimple")
            .filter(|node| {
                node.attribute("StatementText")
                    .is_some_and(|text| text.contains(crate::test_support::parameter_variants::MARKER))
            })
            .collect::<Vec<_>>();
        let [statement] = statements.as_slice() else {
            return None;
        };
        let plans = statement
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "QueryPlan")
            .collect::<Vec<_>>();
        let [plan] = plans.as_slice() else {
            return None;
        };
        if !plan.descendants().any(|node| {
            node.is_element()
                && node.tag_name().name() == "RunTimeCountersPerThread"
                && node.ancestors().find(|ancestor| ancestor.is_element() && ancestor.tag_name().name() == "QueryPlan")
                    == Some(*plan)
                && node
                    .attribute("ActualRows")
                    .and_then(|value| value.parse::<f64>().ok())
                    .is_some_and(|rows| rows.is_finite() && rows >= 0.0)
                && node
                    .attribute("ActualExecutions")
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|runs| runs >= 1)
        }) {
            return None;
        }
        let variant = match plan.attribute("QueryVariantID") {
            Some(value) => Some(value.parse::<u32>().ok().filter(|id| *id > 0 && *id <= i32::MAX as u32)?),
            None => None,
        };
        Some(NativeStatement {
            text: statement.attribute("StatementText")?.trim().into(),
            query_hash: statement.attribute("QueryHash")?.into(),
            plan_hash: statement.attribute("QueryPlanHash")?.into(),
            variant,
            statement_sql_handle: statement.attribute("StatementSqlHandle").map(str::to_owned),
            context_settings_id: statement
                .attribute("DatabaseContextSettingsId")
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value >= 0),
        })
    })
}
/// Native execution-to-cache linkage, not the prepared entry's unrelated module object ID.
pub(super) struct CacheIdentity {
    pub statement_sql_handle: String,
    pub context_settings_id: i64,
    pub query_hash: String,
    pub plan_hash: String,
}
impl CacheIdentity {
    fn from_statement(statement: NativeStatement) -> Option<Self> {
        let handle = statement.statement_sql_handle?;
        let hex = handle.strip_prefix("0x")?;
        if statement.variant.is_none()
            || hex.is_empty()
            || hex.len() > 128
            || !hex.len().is_multiple_of(2)
            || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !hex.bytes().any(|byte| byte != b'0')
            || !crate::cached_statements::valid_hash(&statement.query_hash)
            || !crate::cached_statements::valid_hash(&statement.plan_hash)
        {
            return None;
        }
        Some(Self {
            statement_sql_handle: handle,
            context_settings_id: statement.context_settings_id?,
            query_hash: statement.query_hash,
            plan_hash: statement.plan_hash,
        })
    }
}
pub(super) fn cache_identity(xml: &[String]) -> Option<CacheIdentity> {
    let mut statements = xml.iter().filter_map(|xml| native_statement(xml));
    let selected = statements.next()?;
    if statements.next().is_some() {
        return None;
    }
    CacheIdentity::from_statement(selected)
}
pub(super) fn cache_identity_availability(xml: &[String]) -> serde_json::Value {
    let statements = xml.iter().filter_map(|xml| native_statement(xml)).take(2).collect::<Vec<_>>();
    serde_json::json!({"actual_statement_count_capped_at_two":statements.len(),
        "has_statement_sql_handle":statements.first().is_some_and(|value|value.statement_sql_handle.is_some()),
        "has_database_context_settings_id":statements.first().is_some_and(|value|value.context_settings_id.is_some()),
        "has_native_query_variant_id":statements.first().is_some_and(|value|value.variant.is_some()),
    })
}

pub(super) fn variant_id(xml: &str) -> Option<u32> {
    native_statement(xml)?.variant
}
pub(super) fn bind(
    xml: &[String],
    definitions: &[NamedDefinition],
    original: Option<&CachedStatementContext>,
    routing: RoutingExpectation,
) -> BoundPlan {
    let mut matches = Vec::new();
    for (xml_index, xml) in xml.iter().enumerate() {
        let Some(statement) = native_statement(xml) else {
            continue;
        };
        if !matches!(
            (routing, statement.variant),
            (RoutingExpectation::Variant, Some(_)) | (RoutingExpectation::Ordinary, None)
        ) {
            continue;
        }
        for (definition_index, definition) in definitions.iter().enumerate() {
            let context = &definition.fact.context;
            if definition.original_statement.trim() != statement.text
                || !context.query_hash.as_deref().is_some_and(|hash| hash.eq_ignore_ascii_case(&statement.query_hash))
                || !definition
                    .fact
                    .query_plan_hash
                    .as_deref()
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&statement.plan_hash))
                || original.is_some_and(|original| {
                    original.database_id != context.database_id
                        || original.identity.key().unwrap() != context.identity.key().unwrap()
                })
            {
                continue;
            }
            matches.push(BoundPlan { xml: xml_index, definition: definition_index, variant: statement.variant });
        }
    }
    let [matched] = matches.as_slice() else {
        let diagnostics = xml
            .iter()
            .filter_map(|xml| native_statement(xml))
            .take(2)
            .flat_map(|statement| {
                definitions.iter().take(2).map(move |definition| {
                    let context = &definition.fact.context;
                    serde_json::json!({
                        "exact_statement_text": definition.original_statement.trim() == statement.text,
                        "actual_statement_utf16_bytes": statement.text.encode_utf16().count() * 2,
                        "cache_statement_utf16_bytes": definition.original_statement.encode_utf16().count() * 2,
                        "query_hash_matches": context.query_hash.as_deref().is_some_and(|hash| hash.eq_ignore_ascii_case(&statement.query_hash)),
                        "plan_hash_matches": definition.fact.query_plan_hash.as_deref().is_some_and(|hash| hash.eq_ignore_ascii_case(&statement.plan_hash)),
                        "original_cache_key_matches": original.map(|original| {
                            original.database_id == context.database_id
                                && original.identity.key().ok() == context.identity.key().ok()
                        }),
                    })
                })
            })
            .collect::<Vec<_>>();
        eprintln!(
            "owned PSP strict identity mismatch (at most 4 pairs, no values): {}",
            serde_json::to_string(&diagnostics).unwrap()
        );
        panic!("one exact native actual/cache statement required, observed {}", matches.len());
    };
    BoundPlan { xml: matched.xml, definition: matched.definition, variant: matched.variant }
}
#[cfg(test)]
mod tests;
