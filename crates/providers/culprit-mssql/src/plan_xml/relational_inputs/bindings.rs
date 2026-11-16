//! A closed original SQL/Object map. Expanded views, synonyms, functions and ambiguous access paths cannot supply
//! another scope's variable kind merely because its Table or Alias has the same spelling.
use super::{LocalName, Node};
use culprit_core::{
    sql::{MssqlRelationName, MssqlRelationReference, identifier_parts, mssql_relational_references},
    telemetry::NativeReadContext,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn parameters(statement: Node<'_, '_>, read: Option<&NativeReadContext>) -> Option<BTreeMap<u32, String>> {
    let references = mssql_relational_references(statement.attribute("StatementText")?)?;
    // Stored XML removes native database names. An authenticated outer target cannot reconstruct those names;
    // positive archived named-relation binding needs typed proof retained before redaction.
    if read.is_none() && references.iter().any(|reference| matches!(reference.name, MssqlRelationName::Named(_))) {
        return None;
    }
    let mut ids = BTreeSet::new();
    for relop in statement.descendants().filter(|node| node.has_tag_name_local("RelOp")) {
        if !ids.insert(relop.attribute("NodeId")?.parse::<u32>().ok()?) {
            return None;
        }
    }
    let database = read.and_then(|read| read.target.database()).map(|database| database.database.as_str());
    let mut populations = vec![0usize; references.len()];
    let mut bound = BTreeMap::new();
    for object in statement.descendants().filter(|node| node.has_tag_name_local("Object")) {
        let operator = object.parent()?;
        if !matches!(operator.tag_name().name(), "TableScan" | "IndexScan")
            || super::super::children(operator, "Object").count() != 1
        {
            return None;
        }
        let relop = operator.parent().filter(|node| node.has_tag_name_local("RelOp"))?;
        if super::super::operator_element(relop) != Some(operator) || object.attribute("Server").is_some() {
            return None;
        }
        let native = NativeObject::read(object)?;
        let mut matching = references.iter().enumerate().filter(|(_, reference)| native.matches(reference, database));
        let (at, reference) = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        populations[at] += 1;
        if let MssqlRelationName::Parameter(name) = &reference.name {
            bound.insert(relop.attribute("NodeId")?.parse().ok()?, name.clone());
        }
    }
    references
        .iter()
        .zip(populations)
        .all(|(reference, count)| {
            count > 0 && (!matches!(reference.name, MssqlRelationName::Parameter(_)) || count == 1)
        })
        .then_some(bound)
}

struct NativeObject {
    database: Option<String>,
    schema: Option<String>,
    table: String,
    alias: Option<String>,
}

impl NativeObject {
    fn read(object: Node<'_, '_>) -> Option<Self> {
        Some(Self {
            database: optional_identifier(object, "Database")?,
            schema: optional_identifier(object, "Schema")?,
            table: identifier(object.attribute("Table")?)?,
            alias: optional_identifier(object, "Alias")?,
        })
    }

    fn matches(&self, reference: &MssqlRelationReference, database: Option<&str>) -> bool {
        let name_matches = match &reference.name {
            MssqlRelationName::Parameter(name) => self.table == *name,
            MssqlRelationName::Named(parts) => match parts.as_slice() {
                [schema, table] => {
                    database.is_some_and(|database| self.database.as_deref() == Some(database))
                        && self.schema.as_ref() == Some(schema)
                        && self.table == *table
                }
                [database, schema, table] => {
                    self.database.as_ref() == Some(database)
                        && self.schema.as_ref() == Some(schema)
                        && self.table == *table
                }
                _ => false,
            },
        };
        name_matches
            && match &reference.alias {
                Some(alias) => self.alias.as_ref() == Some(alias),
                None => self.alias.as_ref().is_none_or(|alias| alias == &self.table),
            }
    }
}

fn optional_identifier(object: Node<'_, '_>, name: &str) -> Option<Option<String>> {
    match object.attribute(name) {
        Some(value) => identifier(value).map(Some),
        None => Some(None),
    }
}

fn identifier(value: &str) -> Option<String> {
    let mut parts = identifier_parts(value)?;
    (parts.len() == 1).then(|| parts.remove(0))
}
