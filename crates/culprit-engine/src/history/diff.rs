use std::collections::{BTreeMap, BTreeSet};

use culprit_core::changes::epoch;
use culprit_core::{
    Capability, ChangeEvent, Engine, EventAction, EventKind, EventSource, IndexStat, Snapshot, server_label, sql,
};

use super::{SAME_TIME, USAGE, same_database};
use crate::trends;

/// Optimizer statistics updates kept per scan (tables the heaviest queries use, most recent first).
const MAX_STATISTICS: usize = 200;
/// Setting changes in one scan beyond this are folded into one event.
const MAX_SETTINGS: usize = 12;

/// What changed from `previous` to `current` (the same database), plus what the database dated itself (`recorded`).
pub fn diff(previous: Option<&Snapshot>, current: &Snapshot, recorded: Vec<ChangeEvent>) -> Vec<ChangeEvent> {
    let now = epoch(&current.taken_at).unwrap_or(0.0);
    let previous = previous.filter(|p| same_database(p, current) && epoch(&p.taken_at).is_some_and(|t| t < now));
    let since = previous.and_then(|p| epoch(&p.taken_at)).unwrap_or(now - trends::HISTORY_DAYS * 86_400.0);
    let mut out = Vec::new();
    if let Some(p) = previous {
        let at = (p.taken_at.as_str(), current.taken_at.as_str());
        schema(p, current, at, &mut out);
        settings(p, current, at, &mut out);
        version(p, current, at, &mut out);
        usage_reset(p, current, at, &mut out);
    }
    restart(previous, current, since, &mut out);
    stats_reset(previous, current, since, &mut out);
    statistics(previous, current, since, &mut out);

    // The database's own timestamps make "between two scans" exact where exactly one change can be behind them, and
    // stand on their own where Culprit's comparison found nothing (a procedure altered, a table rebuilt).
    let recorded: Vec<ChangeEvent> =
        recorded.into_iter().filter(|e| e.start() > since && e.start() <= now + 60.0).collect();
    let mut explained = vec![false; recorded.len()];
    for (i, r) in recorded.iter().enumerate() {
        let matches: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, e)| e.source == EventSource::Scans && explains(r, e))
            .map(|(j, _)| j)
            .collect();
        if matches.is_empty() {
            continue;
        }
        explained[i] = true;
        // One reload applies every setting it changed; one table alter date only dates one change.
        if matches.len() == 1 || r.kind == EventKind::Setting {
            for j in matches {
                let e = &mut out[j];
                // Ids end with the time; keep them unique and stable under the exact one.
                e.id = format!("{}{}", e.id.strip_suffix(e.at.as_str()).unwrap_or(&e.id), r.at);
                e.at = r.at.clone();
                e.after = None;
                e.source = EventSource::Database;
            }
        }
    }
    // A runtime SET of one setting is only news when the scans saw its global value change (sessions set them too).
    out.extend(
        recorded
            .into_iter()
            .zip(explained)
            .filter(|(r, done)| !done && !(r.kind == EventKind::Setting && r.object.is_some()))
            .map(|(r, _)| r),
    );
    let mut seen = BTreeSet::new();
    out.retain(|e| seen.insert(e.id.clone()));
    out.sort_by(|a, b| a.start().total_cmp(&b.start()));
    out
}

fn explains(recorded: &ChangeEvent, e: &ChangeEvent) -> bool {
    let inside = recorded.start() >= e.start() - 1.0 && recorded.start() <= e.end() + 1.0;
    let same_table = recorded.table.as_deref().zip(e.table.as_deref()).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    inside
        && match (recorded.kind, e.kind) {
            // MySQL's CREATE_TIME moves on create and alter alike, so "changed" dates either.
            (EventKind::Table, EventKind::Table) => {
                same_table && (recorded.action == e.action || recorded.action == Some(EventAction::Changed))
            }
            // A table's alter date moves when its columns or indexes change.
            (EventKind::Table, EventKind::Index) => same_table && recorded.action == Some(EventAction::Changed),
            (EventKind::Index, EventKind::Index) => {
                same_table
                    && recorded.action == e.action
                    && recorded
                        .object
                        .as_deref()
                        .zip(e.object.as_deref())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }
            (EventKind::Code, EventKind::Code) => {
                same_table
                    && (recorded.action == e.action || recorded.action == Some(EventAction::Changed))
                    && recorded.object.as_deref().zip(e.object.as_deref()).is_some_and(|(a, b)| {
                        let a_leaf = a.rsplit('.').next().unwrap_or(a);
                        let b_leaf = b.rsplit('.').next().unwrap_or(b);
                        a_leaf.eq_ignore_ascii_case(b_leaf)
                    })
            }
            (EventKind::Setting, EventKind::Setting) => match (&recorded.object, &e.object) {
                (None, _) => true,
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            },
            _ => false,
        }
}

fn key(schema: &str, name: &str) -> String {
    format!("{}.{}", schema.to_ascii_lowercase(), name.to_ascii_lowercase())
}

fn scans(event: ChangeEvent, after: &str) -> ChangeEvent {
    event.between(after)
}

/// Tables, columns and indexes. Skipped when either scan has no table data (a permission or collector problem, not a
/// schema change), or when most tables vanished at once.
fn schema(p: &Snapshot, c: &Snapshot, (after, at): (&str, &str), out: &mut Vec<ChangeEvent>) {
    if p.tables.is_empty() || c.tables.is_empty() {
        return;
    }
    let tables = |s: &Snapshot| -> BTreeMap<String, String> {
        s.tables.iter().map(|t| (key(&t.schema, &t.name), t.qualified())).collect()
    };
    let (ta, tb) = (tables(p), tables(c));
    let gone = ta.keys().filter(|k| !tb.contains_key(*k)).count();
    if ta.len() > 20 && gone * 2 > ta.len() {
        return;
    }
    for (k, name) in &tb {
        if !ta.contains_key(k) {
            out.push(scans(
                ChangeEvent::new(EventKind::Table, EventSource::Scans, name, at, format!("Table {name} was created"))
                    .action(EventAction::Created)
                    .table(name.clone()),
                after,
            ));
        }
    }
    for (k, name) in &ta {
        if !tb.contains_key(k) {
            out.push(scans(
                ChangeEvent::new(EventKind::Table, EventSource::Scans, name, at, format!("Table {name} was dropped"))
                    .action(EventAction::Dropped)
                    .table(name.clone()),
                after,
            ));
        }
    }
    let common: BTreeMap<&String, &String> = tb.iter().filter(|(k, _)| ta.contains_key(*k)).collect();
    if !p.columns.is_empty() && !c.columns.is_empty() {
        columns(p, c, &common, (after, at), out);
    }
    if !p.indexes.is_empty() || !c.indexes.is_empty() {
        indexes(p, c, &common, (after, at), out);
    }
    if !p.foreign_keys.is_empty() || !c.foreign_keys.is_empty() {
        foreign_keys(p, c, &common, (after, at), out);
    }
    if !p.constraints.is_empty() || !c.constraints.is_empty() {
        constraints(p, c, &common, (after, at), out);
    }
    triggers(p, c, &common, (after, at), out);
}

fn columns(
    p: &Snapshot,
    c: &Snapshot,
    common: &BTreeMap<&String, &String>,
    (after, at): (&str, &str),
    out: &mut Vec<ChangeEvent>,
) {
    let by_table = |s: &Snapshot| {
        let mut map: BTreeMap<String, BTreeMap<String, (String, String, bool)>> = BTreeMap::new();
        for col in &s.columns {
            map.entry(key(&col.schema, &col.table))
                .or_default()
                .insert(col.name.to_ascii_lowercase(), (col.name.clone(), col.data_type.clone(), col.nullable));
        }
        map
    };
    let (ca, cb) = (by_table(p), by_table(c));
    for (k, table) in common {
        let (Some(a), Some(b)) = (ca.get(*k), cb.get(*k)) else { continue };
        let shape = |t: &str, nullable: bool| format!("{t}{}", if nullable { "" } else { " NOT NULL" });
        let mut changes = Vec::new();
        for (name, (display, ty, nullable)) in b {
            match a.get(name) {
                None => changes.push(format!("added {display} {}", shape(ty, *nullable))),
                Some((_, old, old_null)) if !old.eq_ignore_ascii_case(ty) || old_null != nullable => {
                    changes.push(format!("{display} {} → {}", shape(old, *old_null), shape(ty, *nullable)))
                }
                Some(_) => {}
            }
        }
        for (name, (display, _, _)) in a {
            if !b.contains_key(name) {
                changes.push(format!("dropped {display}"));
            }
        }
        if changes.is_empty() {
            continue;
        }
        let title = if changes.len() == 1 {
            format!("Column change on {table}: {}", changes[0])
        } else {
            format!("{} column changes on {table}", changes.len())
        };
        out.push(scans(
            ChangeEvent::new(EventKind::Table, EventSource::Scans, &format!("{table}.columns"), at, title)
                .action(EventAction::Changed)
                .table(table.to_string())
                .detail(changes.join("; ")),
            after,
        ));
    }
}

/// What makes two indexes the same index, whatever they are called.
fn signature(i: &IndexStat) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        i.kind.to_ascii_lowercase(),
        i.columns.join(",").to_ascii_lowercase(),
        i.include.join(",").to_ascii_lowercase(),
        i.filter.as_deref().unwrap_or_default().to_ascii_lowercase(),
        i.is_unique
    )
}

fn describe(i: &IndexStat) -> String {
    if let Some(definition) = i.definition.as_deref().filter(|d| !d.is_empty()) {
        return definition.to_string();
    }
    let mut text = format!("({})", i.columns.join(", "));
    if !i.include.is_empty() {
        text.push_str(&format!(" INCLUDE ({})", i.include.join(", ")));
    }
    if let Some(filter) = &i.filter {
        text.push_str(&format!(" WHERE {filter}"));
    }
    if i.is_unique {
        text = format!("UNIQUE {text}");
    }
    text
}

/// Indexes on the tables both scans have, by table and name.
fn index_map<'a>(s: &'a Snapshot, common: &BTreeMap<&String, &String>) -> BTreeMap<(String, String), &'a IndexStat> {
    s.indexes
        .iter()
        .filter(|i| common.contains_key(&key(&i.schema, &i.table)))
        .map(|i| ((key(&i.schema, &i.table), i.name.to_ascii_lowercase()), i))
        .collect()
}

fn indexes(
    p: &Snapshot,
    c: &Snapshot,
    common: &BTreeMap<&String, &String>,
    (after, at): (&str, &str),
    out: &mut Vec<ChangeEvent>,
) {
    let (ia, ib) = (index_map(p, common), index_map(c, common));
    let created: Vec<&IndexStat> = ib.iter().filter(|(k, _)| !ia.contains_key(*k)).map(|(_, i)| *i).collect();
    let mut dropped: Vec<&IndexStat> = ia.iter().filter(|(k, _)| !ib.contains_key(*k)).map(|(_, i)| *i).collect();
    let event = |i: &IndexStat, title: String, action: EventAction| {
        let table = i.qualified_table();
        scans(
            ChangeEvent::new(EventKind::Index, EventSource::Scans, &format!("{table}.{}", i.name), at, title)
                .action(action)
                .table(table)
                .object(i.name.clone()),
            after,
        )
    };
    for i in created {
        let table = i.qualified_table();
        // The same definition under a new name: a rename, which doesn't change plans.
        if let Some(pos) = dropped
            .iter()
            .position(|d| d.qualified_table().eq_ignore_ascii_case(&table) && signature(d) == signature(i))
        {
            let old = dropped.remove(pos);
            out.push(
                event(i, format!("Index {} on {table} was renamed to {}", old.name, i.name), EventAction::Changed)
                    .minor(),
            );
            continue;
        }
        out.push(
            event(i, format!("Index {} was created on {table}", i.name), EventAction::Created).detail(describe(i)),
        );
    }
    for i in dropped {
        let table = i.qualified_table();
        out.push(
            event(i, format!("Index {} was dropped from {table}", i.name), EventAction::Dropped).detail(describe(i)),
        );
    }
    for (k, b) in &ib {
        let Some(a) = ia.get(k) else { continue };
        let table = b.qualified_table();
        if a.is_disabled != b.is_disabled {
            let (verb, action) = if b.is_disabled {
                ("disabled", EventAction::Dropped)
            } else {
                ("enabled again", EventAction::Created)
            };
            out.push(event(b, format!("Index {} on {table} was {verb}", b.name), action));
        } else if signature(a) != signature(b) {
            out.push(
                event(b, format!("Index {} on {table} was redefined", b.name), EventAction::Changed).detail(format!(
                    "{} → {}",
                    describe(a),
                    describe(b)
                )),
            );
        }
    }
}

fn source_recorded(s: &Snapshot, names: &[&str]) -> bool {
    s.sources.is_empty()
        || names.iter().any(|name| {
            s.source_outcome(name).is_some_and(|o| {
                matches!(o.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty)
            })
        })
}

fn describe_fk(fk: &culprit_core::ForeignKey) -> String {
    let mut text = format!(
        "({}) REFERENCES {}.{}({})",
        fk.columns.join(", "),
        fk.ref_schema,
        fk.ref_table,
        fk.ref_columns.join(", ")
    );
    if let Some(rule) = fk.delete_rule.as_deref().filter(|r| !r.is_empty()) {
        text.push_str(&format!(" ON DELETE {}", rule.to_ascii_uppercase()));
    }
    text
}

fn fk_map<'a>(
    s: &'a Snapshot,
    common: &BTreeMap<&String, &String>,
) -> BTreeMap<(String, String, String, String), &'a culprit_core::ForeignKey> {
    s.foreign_keys
        .iter()
        .filter(|fk| common.contains_key(&key(&fk.schema, &fk.table)))
        .map(|fk| {
            (
                (
                    key(&fk.schema, &fk.table),
                    fk.columns.join(",").to_ascii_lowercase(),
                    key(&fk.ref_schema, &fk.ref_table),
                    fk.ref_columns.join(",").to_ascii_lowercase(),
                ),
                fk,
            )
        })
        .collect()
}

fn foreign_keys(
    p: &Snapshot,
    c: &Snapshot,
    common: &BTreeMap<&String, &String>,
    (after, at): (&str, &str),
    out: &mut Vec<ChangeEvent>,
) {
    let sources = &[culprit_core::source_id::FOREIGN_KEYS, culprit_core::source_id::CATALOG];
    if (p.foreign_keys.is_empty() && !source_recorded(p, sources))
        || (c.foreign_keys.is_empty() && !source_recorded(c, sources))
    {
        return;
    }
    let (fa, fb) = (fk_map(p, common), fk_map(c, common));
    let event = |fk: &culprit_core::ForeignKey, title: String, action: EventAction| {
        let table = format!("{}.{}", fk.schema, fk.table);
        scans(
            ChangeEvent::new(EventKind::Table, EventSource::Scans, &format!("{table}.{}", fk.name), at, title)
                .action(action)
                .table(table)
                .object(fk.name.clone()),
            after,
        )
    };
    for (k, b) in &fb {
        let table = format!("{}.{}", b.schema, b.table);
        match fa.get(k) {
            None => {
                out.push(
                    event(b, format!("Foreign key {} was added on {table}", b.name), EventAction::Created)
                        .detail(describe_fk(b)),
                );
            }
            Some(a) => {
                if let (Some(ra), Some(rb)) = (a.delete_rule.as_deref(), b.delete_rule.as_deref())
                    && !ra.eq_ignore_ascii_case(rb)
                {
                    out.push(
                        event(
                            b,
                            format!("Foreign key {} on {table} changed ON DELETE rule", b.name),
                            EventAction::Changed,
                        )
                        .detail(format!(
                            "ON DELETE {} → ON DELETE {}",
                            ra.to_ascii_uppercase(),
                            rb.to_ascii_uppercase()
                        )),
                    );
                }
            }
        }
    }
    for (k, a) in &fa {
        if !fb.contains_key(k) {
            let table = format!("{}.{}", a.schema, a.table);
            out.push(
                event(a, format!("Foreign key {} was dropped from {table}", a.name), EventAction::Dropped)
                    .detail(describe_fk(a)),
            );
        }
    }
}

fn con_map<'a>(
    s: &'a Snapshot,
    common: &BTreeMap<&String, &String>,
) -> BTreeMap<(String, String), &'a culprit_core::ConstraintIssue> {
    s.constraints
        .iter()
        .filter(|con| common.contains_key(&key(&con.schema, &con.table)))
        .map(|con| ((key(&con.schema, &con.table), con.name.to_ascii_lowercase()), con))
        .collect()
}

fn constraints(
    p: &Snapshot,
    c: &Snapshot,
    common: &BTreeMap<&String, &String>,
    (after, at): (&str, &str),
    out: &mut Vec<ChangeEvent>,
) {
    let sources = &[culprit_core::source_id::CONSTRAINTS, culprit_core::source_id::CATALOG];
    if (p.constraints.is_empty() && !source_recorded(p, sources))
        || (c.constraints.is_empty() && !source_recorded(c, sources))
    {
        return;
    }
    let (ca, cb) = (con_map(p, common), con_map(c, common));
    let state = |con: &culprit_core::ConstraintIssue| {
        if con.disabled { "disabled" } else { "NOT VALID / untrusted" }
    };
    let event = |con: &culprit_core::ConstraintIssue, title: String, action: EventAction| {
        let table = format!("{}.{}", con.schema, con.table);
        scans(
            ChangeEvent::new(EventKind::Table, EventSource::Scans, &format!("{table}.{}", con.name), at, title)
                .action(action)
                .table(table)
                .object(con.name.clone()),
            after,
        )
    };
    for (k, b) in &cb {
        let table = format!("{}.{}", b.schema, b.table);
        match ca.get(k) {
            None => {
                out.push(
                    event(b, format!("Constraint {} on {table} is {}", b.name, state(b)), EventAction::Changed)
                        .detail(format!("{:?}", b.kind).to_ascii_uppercase()),
                );
            }
            Some(a) if a.disabled != b.disabled => {
                out.push(
                    event(b, format!("Constraint {} on {table} changed state", b.name), EventAction::Changed)
                        .detail(format!("{} → {}", state(a), state(b))),
                );
            }
            Some(_) => {}
        }
    }
    for (k, a) in &ca {
        if !cb.contains_key(k) {
            let table = format!("{}.{}", a.schema, a.table);
            out.push(
                event(a, format!("Constraint {} on {table} was validated or removed", a.name), EventAction::Changed)
                    .detail(format!("was {}", state(a))),
            );
        }
    }
}

fn describe_trigger_events(events: &str) -> String {
    let mut parts = Vec::new();
    for ch in events.chars() {
        match ch.to_ascii_uppercase() {
            'I' => parts.push("INSERT"),
            'U' => parts.push("UPDATE"),
            'D' => parts.push("DELETE"),
            _ => {}
        }
    }
    if parts.is_empty() { events.to_string() } else { parts.join(" OR ") }
}

fn triggers(
    p: &Snapshot,
    c: &Snapshot,
    common: &BTreeMap<&String, &String>,
    (after, at): (&str, &str),
    out: &mut Vec<ChangeEvent>,
) {
    let tr_map = |s: &Snapshot| -> BTreeMap<(String, String), (String, String, String)> {
        let mut m = BTreeMap::new();
        for t in &s.tables {
            let tk = key(&t.schema, &t.name);
            if !common.contains_key(&tk) {
                continue;
            }
            if let Some(storage) = &t.storage {
                for (name, events) in storage.triggers() {
                    m.insert(
                        (tk.clone(), name.to_ascii_lowercase()),
                        (t.qualified(), name.to_string(), events.to_string()),
                    );
                }
            }
        }
        m
    };
    let (ta, tb) = (tr_map(p), tr_map(c));
    if ta.is_empty() && tb.is_empty() {
        return;
    }
    let event = |table: &str, name: &str, title: String, action: EventAction| {
        scans(
            ChangeEvent::new(EventKind::Code, EventSource::Scans, &format!("{table}.{name}"), at, title)
                .action(action)
                .table(table.to_string())
                .object(name.to_string()),
            after,
        )
    };
    for (k, (table, name, ev_b)) in &tb {
        match ta.get(k) {
            None => {
                out.push(
                    event(table, name, format!("Trigger {name} was created on {table}"), EventAction::Created)
                        .detail(describe_trigger_events(ev_b)),
                );
            }
            Some((_, _, ev_a)) if !ev_a.eq_ignore_ascii_case(ev_b) => {
                out.push(
                    event(table, name, format!("Trigger {name} on {table} was redefined"), EventAction::Changed)
                        .detail(format!("{} → {}", describe_trigger_events(ev_a), describe_trigger_events(ev_b))),
                );
            }
            Some(_) => {}
        }
    }
    for (k, (table, name, ev_a)) in &ta {
        if !tb.contains_key(k) {
            out.push(
                event(table, name, format!("Trigger {name} was dropped from {table}"), EventAction::Dropped)
                    .detail(describe_trigger_events(ev_a)),
            );
        }
    }
}

/// A setting value as people write it: MySQL prints `long_query_time` as `10.000000`; it reads as `10`.
pub(super) fn setting_text(value: &str) -> String {
    let value = value.trim();
    match value.split_once('.') {
        Some((whole, fraction))
            if !whole.is_empty()
                && whole.trim_start_matches('-').chars().all(|c| c.is_ascii_digit())
                && fraction.chars().all(|c| c.is_ascii_digit()) =>
        {
            let fraction = fraction.trim_end_matches('0');
            if fraction.is_empty() { whole.to_string() } else { format!("{whole}.{fraction}") }
        }
        _ => value.to_string(),
    }
}

fn settings(p: &Snapshot, c: &Snapshot, (after, at): (&str, &str), out: &mut Vec<ChangeEvent>) {
    let skip = |name: &str| culprit_checks::drift::NOT_SETTINGS.iter().any(|n| n.eq_ignore_ascii_case(name));
    let changed: Vec<(String, String, String)> = c
        .settings
        .iter()
        .filter(|s| !skip(&s.name))
        .filter_map(|s| {
            let old = p.setting(&s.name)?;
            (!old.value.trim().eq_ignore_ascii_case(s.value.trim()))
                .then(|| (s.name.clone(), setting_text(&old.value), setting_text(&s.value)))
        })
        .collect();
    if changed.len() > MAX_SETTINGS {
        let detail: Vec<String> = changed.iter().take(8).map(|(n, a, b)| format!("{n}: {a} → {b}")).collect();
        out.push(scans(
            ChangeEvent::new(
                EventKind::Setting,
                EventSource::Scans,
                "many",
                at,
                format!("{} settings changed", changed.len()),
            )
            .action(EventAction::Changed)
            .detail(format!("{}; …", detail.join("; "))),
            after,
        ));
        return;
    }
    for (name, a, b) in changed {
        out.push(scans(
            ChangeEvent::new(
                EventKind::Setting,
                EventSource::Scans,
                &name,
                at,
                format!("{name} changed from {a} to {b}"),
            )
            .action(EventAction::Changed)
            .object(name.clone()),
            after,
        ));
    }
}

fn version(p: &Snapshot, c: &Snapshot, (after, at): (&str, &str), out: &mut Vec<ChangeEvent>) {
    if p.server.version == c.server.version {
        return;
    }
    let (a, b) =
        (server_label(&p.server.product, &p.server.version), server_label(&c.server.product, &c.server.version));
    let mut event = ChangeEvent::new(
        EventKind::Version,
        EventSource::Scans,
        &c.server.version,
        at,
        format!("The server's version changed: {a} → {b}"),
    )
    .action(EventAction::Changed);
    // Upgrades restart the server, which dates them.
    match c.server.started_at.as_deref().filter(|s| epoch(s).is_some_and(|t| t > epoch(after).unwrap_or(0.0))) {
        Some(started) => {
            event.at = started.to_string();
            event.source = EventSource::Database;
        }
        None => event = event.between(after),
    }
    out.push(event);
}

fn restart(p: Option<&Snapshot>, c: &Snapshot, since: f64, out: &mut Vec<ChangeEvent>) {
    let Some(started) = c.server.started_at.as_deref() else { return };
    let Some(t) = epoch(started) else { return };
    let moved = match p.and_then(|p| p.server.started_at.as_deref()).and_then(epoch) {
        Some(before) => (t - before).abs() > SAME_TIME,
        None => true,
    };
    if !moved || t <= since {
        return;
    }
    let title = if p.is_some() { "The server restarted" } else { "The server started" };
    out.push(
        ChangeEvent::new(EventKind::Restart, EventSource::Database, "server", started, title)
            .detail("Caches start empty and every plan compiles again, so the first minutes run slower."),
    );
}

/// Usage counters that went down since the scan before without the server restarting (a restart starts every counter
/// over, and the server's start time says so): the database was taken offline, restored or failed over (SQL Server
/// keeps index usage per database, and AUTO_CLOSE or a serverless pause takes it offline), a table was altered or
/// optimized (MySQL), its statistics were reset. Reads counted before are gone, so no index may be judged unread over
/// time its counters didn't see. One event for the database when most indexes started over, else one per table.
fn usage_reset(p: &Snapshot, c: &Snapshot, (after, at): (&str, &str), out: &mut Vec<ChangeEvent>) {
    if !p.server.has(Capability::IndexUsage) || !c.server.has(Capability::IndexUsage) {
        return;
    }
    let started = |s: &Snapshot| s.server.started_at.as_deref().and_then(epoch);
    if let (Some(a), Some(b)) = (started(p), started(c))
        && (a - b).abs() > SAME_TIME
    {
        return;
    }
    // Counters the server says started after the scan before (a database-wide statistics reset): already the window.
    if let (Some(hours), Some(now), Some(then)) =
        (c.pressure.as_ref().and_then(|x| x.uptime_hours), epoch(&c.taken_at), epoch(&p.taken_at))
        && now - hours * 3600.0 > then + SAME_TIME
    {
        return;
    }
    let fell = |pairs: &[(Option<f64>, Option<f64>)]| {
        pairs.iter().any(|(before, now)| match (before, now) {
            (Some(b), Some(n)) => *n < *b - 0.5,
            // Counted before, nothing now: the row went with the counters.
            (Some(b), None) => *b > 0.0,
            _ => false,
        })
    };
    let before: BTreeMap<(String, String), &IndexStat> =
        p.indexes.iter().map(|i| ((key(&i.schema, &i.table), i.name.to_ascii_lowercase()), i)).collect();
    let (mut counted, mut started_over) = (0usize, 0usize);
    let mut tables: BTreeMap<String, String> = BTreeMap::new();
    for i in &c.indexes {
        let Some(b) = before.get(&(key(&i.schema, &i.table), i.name.to_ascii_lowercase())) else { continue };
        let pairs =
            [(b.reads, i.reads), (b.writes, i.writes), (b.seeks, i.seeks), (b.scans, i.scans), (b.lookups, i.lookups)];
        if pairs.iter().all(|(before, _)| before.unwrap_or(0.0) <= 0.0) {
            continue;
        }
        counted += 1;
        if fell(&pairs) {
            started_over += 1;
            tables.insert(key(&i.schema, &i.table), i.qualified_table());
        }
    }
    for t in &c.tables {
        let Some(b) = p.table(&t.schema, &t.name) else { continue };
        let pairs = [
            (b.seq_scans, t.seq_scans),
            (b.index_scans, t.index_scans),
            (b.inserts, t.inserts),
            (b.updates, t.updates),
            (b.deletes, t.deletes),
        ];
        if pairs.iter().any(|(before, now)| matches!((before, now), (Some(b), Some(n)) if *n < *b - 0.5)) {
            tables.insert(key(&t.schema, &t.name), t.qualified());
        }
    }
    if tables.is_empty() {
        return;
    }
    let detail =
        "Reads counted before are gone, so its indexes aren't judged unread until they've been counted for a week.";
    if counted >= 4 && started_over * 2 > counted {
        out.push(
            ChangeEvent::new(EventKind::StatsReset, EventSource::Scans, USAGE, at, "Index usage counters started over")
                .object(USAGE)
                .detail(format!(
                    "Without a server restart: the database was taken offline, restored or failed over. {detail}"
                ))
                .between(after),
        );
        return;
    }
    for name in tables.values() {
        out.push(
            ChangeEvent::new(
                EventKind::StatsReset,
                EventSource::Scans,
                &format!("{USAGE}:{name}"),
                at,
                format!("Usage counters on {name} started over"),
            )
            .object(USAGE)
            .table(name.clone())
            .detail(format!("The table was altered, rebuilt or had its statistics reset. {detail}"))
            .minor()
            .between(after),
        );
    }
}

/// pg_stat_statements says when it was last reset; other sources' windows are inferred and would raise false alarms.
fn stats_reset(p: Option<&Snapshot>, c: &Snapshot, since: f64, out: &mut Vec<ChangeEvent>) {
    let Some(w) = c.workload.as_ref().filter(|w| !w.is_windowed() && c.server.engine == Engine::Postgres) else {
        return;
    };
    let Some(reset) = w.window_start.as_deref() else { return };
    let Some(t) = epoch(reset) else { return };
    let before = p.and_then(|p| p.workload.as_ref()).and_then(|w| w.window_start.as_deref()).and_then(epoch);
    let started = c.server.started_at.as_deref().and_then(epoch);
    if t <= since
        || before.is_some_and(|b| (t - b).abs() <= SAME_TIME)
        || started.is_some_and(|s| (t - s).abs() <= SAME_TIME)
    {
        return;
    }
    out.push(
        ChangeEvent::new(
            EventKind::StatsReset,
            EventSource::Database,
            &w.source,
            reset,
            format!("{} was reset", w.source),
        )
        .detail("Query statistics start again from zero; periods before and after it can't be compared through scans."),
    );
}

/// Fresh optimizer statistics on the tables the heaviest queries use: routine, but the usual reason a plan changes.
fn statistics(p: Option<&Snapshot>, c: &Snapshot, since: f64, out: &mut Vec<ChangeEvent>) {
    let engine = c.server.engine;
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut queries: Vec<_> = c.queries().iter().collect();
    queries.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    for q in queries.iter().take(100) {
        for t in sql::tables_in(&q.text, engine) {
            used.insert(t.rsplit('.').next().unwrap_or(&t).to_ascii_lowercase());
        }
    }
    if used.is_empty() {
        return;
    }
    let latest = |s: &Snapshot| -> BTreeMap<String, (String, String)> {
        let mut map: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut keep = |schema: &str, table: &str, at: &str| {
            let entry = map.entry(key(schema, table)).or_insert_with(|| (format!("{schema}.{table}"), at.to_string()));
            if epoch(at) > epoch(&entry.1) {
                entry.1 = at.to_string();
            }
        };
        for t in &s.tables {
            if let Some(at) = &t.last_analyze {
                keep(&t.schema, &t.name, at);
            }
        }
        for st in &s.statistics {
            if let Some(at) = &st.last_updated {
                keep(&st.schema, &st.table, at);
            }
        }
        map
    };
    let before = p.map(latest).unwrap_or_default();
    let mut found: Vec<ChangeEvent> = latest(c)
        .into_iter()
        .filter(|(k, _)| used.contains(k.rsplit('.').next().unwrap_or(k)))
        .filter(|(k, (_, at))| epoch(at).is_some_and(|t| t > since) && before.get(k).is_none_or(|(_, b)| b != at))
        .map(|(_, (table, at))| {
            ChangeEvent::new(
                EventKind::Statistics,
                EventSource::Database,
                &table,
                &at,
                format!("Statistics on {table} were updated"),
            )
            .table(table)
            .minor()
        })
        .collect();
    found.sort_by(|a, b| b.at.cmp(&a.at));
    found.truncate(MAX_STATISTICS);
    out.extend(found);
}
