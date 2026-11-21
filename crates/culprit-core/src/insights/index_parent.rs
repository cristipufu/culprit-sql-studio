//! A native original view kind refuses table-index advice without joining a current catalog by name.
use super::*;
use crate::telemetry::MssqlIndexParentKind;

/// Direct original ownership, or one objectless operator whose owned inputs are all the same known native view.
/// False means unproved here; it never establishes an ordinary table from a missing/general native index kind.
pub fn original_indexed_view_input(engine: Engine, primary: &PlanNode) -> bool {
    if engine != Engine::Mssql {
        return false;
    }
    if primary.object.is_some() {
        return primary.facts().mssql_index_parent_kind == Some(MssqlIndexParentKind::IndexedView);
    }
    let mut descendants = Vec::new();
    primary.walk(&mut descendants);
    let mut objects = descendants.into_iter().filter(|node| actionable_object(node).is_some());
    let Some(first) = objects.next() else { return false };
    first.facts().mssql_index_parent_kind == Some(MssqlIndexParentKind::IndexedView)
        && objects.all(|node| {
            node.object == first.object
                && node.facts().mssql_index_parent_kind == Some(MssqlIndexParentKind::IndexedView)
        })
}
pub(super) fn is_original_indexed_view(cx: &Cx<'_>, draft: &Draft) -> bool {
    if cx.engine() != Engine::Mssql || draft.area != InsightArea::Index || !cx.node_ids_unique {
        return false;
    }
    if let Some(subject) = draft.index_subject_node.and_then(|id| cx.node(id)) {
        return subject.facts().mssql_index_parent_kind == Some(MssqlIndexParentKind::IndexedView);
    }
    if let Some(hint) = &draft.index_hint {
        let mut owners = cx
            .nodes
            .iter()
            .filter(|node| node.object.as_deref().is_some_and(|object| same_original_subject(object, &hint.table)))
            .peekable();
        return owners.peek().is_some()
            && owners.all(|node| node.facts().mssql_index_parent_kind == Some(MssqlIndexParentKind::IndexedView));
    }
    draft
        .nodes
        .first()
        .and_then(|id| cx.node(*id))
        .is_some_and(|primary| original_indexed_view_input(cx.engine(), primary))
}
/// Only the original qualified subject can narrow an original hint; an unqualified leaf supplies no owner.
pub(super) fn same_original_subject(object: &str, requested: &str) -> bool {
    let clean = |value: &str| value.replace(['[', ']', '"', '`'], "");
    let (object, requested) = (clean(object), clean(requested));
    object.contains('.') && requested.contains('.') && object.eq_ignore_ascii_case(&requested)
}
