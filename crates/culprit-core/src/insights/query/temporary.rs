//! The selected native operation supplies the mechanism; SQL/display strings supply no missing plan facts.
use super::*;
use crate::plan_facts::PlanTemporaryTableUse;

enum Mechanism {
    Grouping,
    DuplicateRemoval,
    Ordering,
    AggregateOrdering,
    MaterializedSorting,
    UnknownSortedResult,
}

struct TemporaryWork<'a> {
    node: &'a PlanNode,
    sorting: Option<&'a PlanNode>,
    mechanism: Mechanism,
    rows: f64,
}

pub(in crate::insights) fn temp_table_sort(cx: &Cx) -> Vec<Draft> {
    cx.nodes.iter().filter_map(|node| decide(cx, node)).map(|case| render(cx, case)).collect()
}

fn decide<'a>(cx: &Cx<'a>, node: &'a PlanNode) -> Option<TemporaryWork<'a>> {
    let temporary = node.facts().temporary_table?;
    let rows = node.children.iter().map(rows_out).fold(0.0, f64::max).max(rows_out(node));
    if rows < MANY_ROWS {
        return None;
    }
    let sorting = if node.facts().filesort == Some(true) {
        Some(node)
    } else {
        cx.parent(node).filter(|parent| parent.facts().filesort == Some(true))
    };
    let mechanism = match temporary {
        PlanTemporaryTableUse::Grouping => Mechanism::Grouping,
        PlanTemporaryTableUse::DuplicateRemoval => Mechanism::DuplicateRemoval,
        PlanTemporaryTableUse::Ordering => {
            // One explanation covers a directly connected temporary-result/sort chain. A different query block
            // farther below cannot establish grouping, ordered input or the reason for this ordering step.
            if node.children.iter().any(|child| child.facts().temporary_table.is_some()) || sorting.is_none() {
                return None;
            }
            if node.children.iter().any(|child| child.kind == NodeKind::Aggregate) {
                Mechanism::AggregateOrdering
            } else {
                Mechanism::Ordering
            }
        }
        PlanTemporaryTableUse::Materialization => {
            sorting?;
            Mechanism::MaterializedSorting
        }
        PlanTemporaryTableUse::Unknown => {
            sorting?;
            Mechanism::UnknownSortedResult
        }
    };
    Some(TemporaryWork { node, sorting, mechanism, rows })
}

fn render(cx: &Cx, case: TemporaryWork<'_>) -> Draft {
    let mut nodes = vec![case.node.id];
    let mut impact = cx.share(case.node);
    if let Some(sorting) = case.sorting.filter(|sorting| sorting.id != case.node.id) {
        nodes.push(sorting.id);
        impact += cx.share(sorting);
    }
    let area = match case.mechanism {
        Mechanism::Grouping | Mechanism::DuplicateRemoval => InsightArea::Index,
        _ => InsightArea::Query,
    };
    let mut draft = Draft::new("temp-table-sort", area, impact.min(1.0), nodes);
    // A native strategy identifies intermediate work. Its presence does not prove harmful disk I/O or latency.
    draft.cap = Severity::Low;
    let count = format::count(case.rows);
    let (title, detail, fix) = match case.mechanism {
        Mechanism::Grouping => (
            if case.sorting.is_some() {
                t!("insight.temp_table_sort.group.title_sorted", n = case.rows, count = &count)
            } else {
                t!("insight.temp_table_sort.group.title", n = case.rows, count = &count)
            },
            t!("insight.temp_table_sort.group.detail"),
            t!("insight.temp_table_sort.group.fix"),
        ),
        Mechanism::DuplicateRemoval => (
            if case.sorting.is_some() {
                t!("insight.temp_table_sort.duplicates.title_sorted", n = case.rows, count = &count)
            } else {
                t!("insight.temp_table_sort.duplicates.title", n = case.rows, count = &count)
            },
            t!("insight.temp_table_sort.duplicates.detail"),
            t!("insight.temp_table_sort.duplicates.fix"),
        ),
        Mechanism::Ordering => (
            t!("insight.temp_table_sort.order.title", n = case.rows, count = &count),
            t!("insight.temp_table_sort.order.detail"),
            t!("insight.temp_table_sort.order.fix"),
        ),
        Mechanism::AggregateOrdering => (
            t!("insight.temp_table_sort.groups.title"),
            t!("insight.temp_table_sort.groups.detail"),
            t!("insight.temp_table_sort.groups.fix"),
        ),
        Mechanism::MaterializedSorting => (
            t!("insight.temp_table_sort.materialization.title", n = case.rows, count = &count),
            t!("insight.temp_table_sort.materialization.detail"),
            t!("insight.temp_table_sort.materialization.fix"),
        ),
        Mechanism::UnknownSortedResult => (
            t!("insight.temp_table_sort.temporary.title", n = case.rows, count = &count),
            t!("insight.temp_table_sort.temporary.detail"),
            t!("insight.temp_table_sort.temporary.fix"),
        ),
    };
    draft.title = title;
    let mut explanation = vec![detail];
    if case.sorting.is_some() {
        explanation.push(t!("insight.temp_table_sort.sort.detail"));
    }
    explanation.push(t!("insight.temp_table_sort.qualification"));
    draft.detail = explanation.join(" ");
    draft.fix = fix;
    draft.verify = t!("insight.temp_table_sort.verify");
    draft
}
