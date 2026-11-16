//! Bind a selected original native plan to the checked catalog object/index before XML privacy removes its
//! database namespace. This proves planned write access, never a physical page/partition contribution or owner.
use culprit_core::{
    Result,
    sql::identifier_parts,
    telemetry::{
        CollectionStop, MAX_PAGE_WAIT_WRITE_ACCESSES, MappedPageTarget, MssqlPageWriteAccess, NativeReadContext,
        PageWriteOperation, PageWriterRelation,
    },
};
use roxmltree::Node;
use std::collections::BTreeSet;

pub(super) struct Access {
    pub relation: PageWriterRelation,
    pub writes: Vec<MssqlPageWriteAccess>,
}

pub(super) fn classify(
    xml: &str,
    statement: &str,
    target: &MappedPageTarget,
    context: &NativeReadContext,
) -> Result<Access> {
    if !context.has_qualified_local_owner()
        || context.target.database().is_none()
        || target.writer_target_key().is_err()
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    let accesses = culprit_core::xml::read(xml, |document| {
        let document = document.ok()?;
        let selected = crate::plan_xml::select_native_statement(&document, None).ok()?;
        if !tag(selected, "StmtSimple")
            || selected.attribute("StatementText").map(str::trim) != Some(statement.trim())
            || !top_level(selected)
        {
            return None;
        }
        let query = crate::plan_xml::query_plan_of(selected)?;
        if query.descendants().filter(Node::is_element).any(|node| {
            node != query && tag(node, "QueryPlan")
                || matches!(
                    node.tag_name().name(),
                    "UDF" | "StoredProc" | "UserDefinedFunction" | "UserDefinedAggregate" | "TableValuedFunction"
                )
                || tag(node, "InternalInfo") && node.descendants().any(|child| tag(child, "RelOp"))
                || node.attribute("ContainsInlineScalarTsqlUdfs").is_some_and(|value| !matches!(value, "0" | "false"))
        }) {
            return None;
        }
        let database = &context.target.database()?.database;
        let mut ids = BTreeSet::new();
        let mut writes = Vec::new();
        let mut other = false;
        for relop in query.descendants().filter(|node| tag(*node, "RelOp")) {
            let id = relop.attribute("NodeId")?.parse::<u32>().ok()?;
            if !ids.insert(id) {
                return None;
            }
            let physical = relop.attribute("PhysicalOp")?;
            let logical = relop.attribute("LogicalOp").unwrap_or_default();
            let Some(operation) = operation(physical) else {
                if modifying(physical) || modifying(logical) {
                    return None;
                }
                continue;
            };
            let expected = match operation {
                PageWriteOperation::Insert => "Insert",
                PageWriteOperation::Update => "Update",
                PageWriteOperation::Delete => "Delete",
            };
            if logical != expected {
                return None;
            }
            let operator = crate::plan_xml::operator_element(relop)?;
            let mut objects = operator.children().filter(|node| tag(*node, "Object"));
            let object = objects.next()?;
            if objects.next().is_some() || object.attribute("Server").is_some() {
                return None;
            }
            match matches_object(object, database, target)? {
                true => writes.push(MssqlPageWriteAccess { node_id: id, operation }),
                false => other = true,
            }
            if writes.len() > MAX_PAGE_WAIT_WRITE_ACCESSES {
                return None;
            }
        }
        writes.sort_by_key(|write| write.node_id);
        if ids.is_empty() {
            return None;
        }
        if writes.is_empty() && !other && selected.attribute("StatementType") != Some("SELECT") {
            return None;
        }
        Some(Access {
            relation: if !writes.is_empty() {
                PageWriterRelation::TargetObjectWrite
            } else if other {
                PageWriterRelation::OtherTarget
            } else {
                PageWriterRelation::ReadOnlyPlan
            },
            writes,
        })
    });
    Ok(accesses.unwrap_or(Access { relation: PageWriterRelation::Unsupported, writes: vec![] }))
}

fn operation(physical: &str) -> Option<PageWriteOperation> {
    match physical {
        "Table Insert" | "Index Insert" | "Clustered Index Insert" => Some(PageWriteOperation::Insert),
        "Table Update" | "Index Update" | "Clustered Index Update" => Some(PageWriteOperation::Update),
        "Table Delete" | "Index Delete" | "Clustered Index Delete" => Some(PageWriteOperation::Delete),
        _ => None,
    }
}

fn modifying(value: &str) -> bool {
    value.split_ascii_whitespace().any(|word| matches!(word, "Insert" | "Update" | "Delete" | "Merge"))
}

fn matches_object(object: Node<'_, '_>, database: &str, target: &MappedPageTarget) -> Option<bool> {
    let expected = target.object.as_ref()?;
    let native_database = identifier(object.attribute("Database")?)?;
    let schema = identifier(object.attribute("Schema")?)?;
    let table = identifier(object.attribute("Table")?)?;
    let index = match object.attribute("Index") {
        Some(value) => Some(identifier(value)?),
        None => None,
    };
    if native_database != database || schema != expected.schema || table != expected.name {
        return Some(false);
    }
    match (&expected.index_name, index) {
        (Some(expected), Some(actual)) => Some(*expected == actual),
        (Some(_), None) => None,
        (None, actual) => Some(actual.is_none()),
    }
}

fn identifier(value: &str) -> Option<String> {
    let mut parts = identifier_parts(value)?;
    (parts.len() == 1).then(|| parts.remove(0))
}

fn tag(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn top_level(statement: Node<'_, '_>) -> bool {
    let mut parent = statement.parent();
    for name in ["Statements", "Batch", "BatchSequence", "ShowPlanXML"] {
        let Some(node) = parent else { return false };
        if !tag(node, name) {
            return false;
        }
        parent = node.parent();
    }
    parent.is_some_and(|node| node.is_root())
}
