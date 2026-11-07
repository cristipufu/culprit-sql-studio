//! What the schema view says under each index and about each foreign key, from the predicates the index checks use, so
//! the app states the checks' evidence instead of judging the counters itself (`Summary::index_notes`,
//! `Summary::foreign_key_indexes`).

use culprit_core::{ForeignKeyIndex, IndexNote, IndexState};

use super::*;

/// The notes for every index the scan read, in catalog order.
pub(crate) fn index_notes(snapshot: &Snapshot, findings: &[Finding]) -> Vec<IndexNote> {
    let unused = unused_indexes(snapshot);
    let covered = redundant_indexes(snapshot);
    let since = since_phrase(snapshot);
    let finding_id = |id: String| findings.iter().any(|f| f.id == id).then_some(id);
    let mut out = Vec::new();
    for i in &snapshot.indexes {
        let mut note = |tone: Severity, text: String, finding: Option<String>| {
            out.push(IndexNote {
                schema: i.schema.clone(),
                table: i.table.clone(),
                index: i.name.clone(),
                tone,
                text,
                finding,
            });
        };
        if i.is_disabled {
            note(Severity::Medium, disabled(i), None);
        } else if i.reads == Some(0.0) && !i.is_primary_key && !is_partition_copy(i) {
            let wider = covered.iter().any(|(a, _)| std::ptr::eq(*a, i));
            if let Some((tone, text, finding)) = unread(snapshot, i, &unused, &since, wider) {
                note(tone, text, finding.and_then(finding_id));
            }
        }
        if let Some((_, b)) = covered.iter().find(|(a, _)| std::ptr::eq(*a, i)) {
            let text = if same_keys(i, b) {
                format!("Same keys as {}: one of them is enough", b.name)
            } else {
                format!("Covered by {}: it leads with the same keys", b.name)
            };
            note(Severity::Medium, text, finding_id(format!("duplicate-index:{}.{}.{}", i.schema, i.table, i.name)));
        }
    }
    out
}

/// What a disabled index is to the optimizer and to writes.
fn disabled(i: &IndexStat) -> String {
    match i.state {
        Some(IndexState::Invisible) => "Invisible to the optimizer; still maintained by writes".into(),
        Some(IndexState::Ignored) => "Ignored by the optimizer; still maintained by writes".into(),
        Some(IndexState::Invalid) => format!(
            "Invalid: unavailable to the optimizer; {}",
            match i.maintained {
                None => "maintenance state was not recorded",
                Some(true) => "still maintained by writes",
                Some(false) => "not ready for write maintenance",
            }
        ),
        _ => "Disabled: unavailable to the optimizer and not maintained".into(),
    }
}

/// What no reads says about an index: unused exactly where the unused-index check says so (with the foreign key it
/// also backs, as that finding's caveat does); otherwise why not: it serves a foreign key or uniqueness, it may be new,
/// the counters haven't run a week, it holds the rows, it costs too little or its size is unknown. Nothing when a
/// wider index covers it: that note says what to do.
fn unread(
    snapshot: &Snapshot,
    i: &IndexStat,
    unused: &[&IndexStat],
    since: &str,
    wider: bool,
) -> Option<(Severity, String, Option<String>)> {
    let fk = backs_foreign_key(snapshot, i);
    if unused.iter().any(|u| std::ptr::eq(*u, i)) {
        let also = fk
            .map(|fk| {
                format!(
                    "; it also backs foreign key {}, so deletes and updates of {} rows rely on it",
                    fk.name, fk.ref_table
                )
            })
            .unwrap_or_default();
        let text = format!("Never read {since}, yet every write to {} maintains it{also}", i.table);
        return Some((Severity::High, text, Some(format!("unused-index:{}.{}", i.schema, i.table))));
    }
    if let Some(fk) = fk {
        let why = if snapshot.server.engine == Engine::Mysql {
            "InnoDB requires it".to_string()
        } else {
            format!("without it, deleting or updating a {} row scans {}", fk.ref_table, i.table)
        };
        return Some((Severity::Low, format!("Never read {since}, but it backs foreign key {}: {why}", fk.name), None));
    }
    if i.is_unique {
        return Some((Severity::Low, format!("Never read {since}; it still enforces uniqueness"), None));
    }
    if wider {
        return None;
    }
    let text = if too_new(snapshot, i) {
        let created = i.created_at_latest.as_deref().map(|c| format::friendly_date(c, &snapshot.taken_at));
        let by = created.map(|c| format!(" (it existed by {c})")).unwrap_or_default();
        format!("Not read yet, but it may be new{by}: too soon to call it unused")
    } else if !watched_long_enough(snapshot) {
        match crate::support::watched_days(snapshot) {
            Some(_) => format!("No reads {since}; a week of counting is needed before calling it unused"),
            None => "No reads recorded over a period the counters don't establish; a week of known counting is \
                     needed before calling it unused"
                .to_string(),
        }
    } else if i.is_clustered {
        format!("Never read {since}; it holds the table's rows")
    } else if let Some(size) = i.size_bytes {
        format!("Never read {since}; at {} it costs too little to be worth dropping", format::bytes(size))
    } else {
        format!("Never read {since}; its size isn't known, so its upkeep can't be weighed")
    };
    Some((Severity::Low, text, None))
}

/// The period the index usage counters cover, as the notes say it; empty when the scan read no index usage.
pub(crate) fn index_counting(snapshot: &Snapshot) -> String {
    if snapshot.indexes.iter().all(|i| i.reads.is_none() && i.writes.is_none()) {
        return String::new();
    }
    since_phrase(snapshot)
}

/// Whether an index can serve each foreign key, as the unindexed-foreign-key check decides it; unknown where the
/// catalog of the key's table can't establish absence.
pub(crate) fn foreign_key_indexes(snapshot: &Snapshot) -> Vec<ForeignKeyIndex> {
    let catalog_complete = |fk: &culprit_core::ForeignKey| {
        crate::evidence::index_catalog_for_table(snapshot, &fk.schema, &fk.table).is_none()
    };
    snapshot
        .foreign_keys
        .iter()
        .map(|fk| ForeignKeyIndex {
            schema: fk.schema.clone(),
            table: fk.table.clone(),
            name: fk.name.clone(),
            indexed: if fk_is_indexed(snapshot, fk) {
                Some(true)
            } else if catalog_complete(fk) {
                Some(false)
            } else {
                None
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SQL Server demo with its indexes and keys cleared, and counters started `days` before the scan.
    fn snapshot(days: u32) -> Snapshot {
        let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
        s.server.started_at = Some(format!("2026-10-{:02}T12:13:47Z", 11 - days));
        s.indexes.clear();
        s.foreign_keys.clear();
        s
    }

    fn index(name: &str, columns: &[&str]) -> IndexStat {
        let s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
        let mut i = s.indexes.into_iter().find(|i| !i.is_primary_key).unwrap();
        i.schema = "shop".into();
        i.table = "orders".into();
        i.name = name.into();
        i.kind = "nonclustered".into();
        i.columns = columns.iter().map(|c| (*c).into()).collect();
        i.include = Vec::new();
        i.filter = None;
        i.reads = Some(0.0);
        i.writes = Some(5_000.0);
        i.size_bytes = Some(8.0 * 1024.0 * 1024.0);
        (i.is_primary_key, i.is_unique, i.is_clustered, i.is_disabled) = (false, false, false, false);
        i.created_at_latest = None;
        i.partition_of = None;
        i.state = None;
        i
    }

    /// The notes the analyzed report carries, as the app reads them.
    fn notes_for(s: &Snapshot) -> Vec<(Severity, String, Option<String>)> {
        crate::analyze(s.clone()).summary.index_notes.into_iter().map(|n| (n.tone, n.text, n.finding)).collect()
    }

    #[test]
    fn an_index_nothing_reads_is_unused_only_where_the_check_says_so_and_links_its_finding() {
        let mut s = snapshot(10);
        s.indexes = vec![index("ix_orders_status", &["status"])];
        let notes = notes_for(&s);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].0, Severity::High);
        assert!(notes[0].1.starts_with("Never read in the 10 days since"), "{}", notes[0].1);
        assert!(notes[0].1.ends_with("yet every write to orders maintains it"), "{}", notes[0].1);
        assert_eq!(notes[0].2.as_deref(), Some("unused-index:shop.orders"), "the note opens the check's finding");

        s.indexes[0].reads = Some(12.0);
        assert!(notes_for(&s).is_empty());
        s.indexes[0].reads = None;
        assert!(notes_for(&s).is_empty(), "no counter says nothing");
    }

    #[test]
    fn under_a_week_of_counters_no_reads_is_not_called_unused() {
        let mut s = snapshot(2);
        s.indexes = vec![index("ix_orders_status", &["status"])];
        let notes = notes_for(&s);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].0, Severity::Low);
        assert!(notes[0].1.starts_with("No reads in the 2 days since"), "{}", notes[0].1);
        assert!(notes[0].1.ends_with("a week of counting is needed before calling it unused"), "{}", notes[0].1);
        assert_eq!(notes[0].2, None);
    }

    #[test]
    fn an_unread_index_says_what_keeps_it() {
        let mut s = snapshot(10);
        let mut young = index("ix_new", &["status"]);
        young.created_at_latest = Some("2026-10-09T09:00:00Z".into());
        let mut unique = index("ux_orders_number", &["number"]);
        unique.is_unique = true;
        let mut pk = index("pk_orders", &["id"]);
        (pk.is_primary_key, pk.is_unique) = (true, true);
        let mut small = index("ix_small", &["channel"]);
        small.size_bytes = Some(16.0 * 1024.0);
        s.indexes = vec![young, unique, pk, small];
        let notes = notes_for(&s);
        let text =
            |name: &str| index_notes(&s, &[]).into_iter().find(|n| n.index == name).map(|n| n.text).unwrap_or_default();
        assert!(text("ix_new").contains("too soon to call it unused"), "{notes:?}");
        assert!(text("ux_orders_number").ends_with("it still enforces uniqueness"));
        assert_eq!(text("pk_orders"), "");
        assert!(text("ix_small").contains("costs too little"), "{}", text("ix_small"));
        assert!(notes.iter().all(|(tone, ..)| *tone == Severity::Low), "{notes:?}");
    }

    #[test]
    fn an_index_a_foreign_key_relies_on_says_so_and_innodb_requires_it() {
        let mut s = snapshot(10);
        s.foreign_keys = vec![culprit_core::ForeignKey {
            name: "fk_orders_customer".into(),
            schema: "shop".into(),
            table: "orders".into(),
            columns: vec!["customer_id".into()],
            ref_schema: "shop".into(),
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            delete_rule: None,
        }];
        let mut backing = index("ix_orders_customer", &["customer_id"]);
        backing.size_bytes = Some(16.0 * 1024.0);
        s.indexes = vec![backing];
        let notes = notes_for(&s);
        assert_eq!(notes[0].0, Severity::Low);
        assert!(
            notes[0].1.ends_with(
                "but it backs foreign key fk_orders_customer: without it, deleting or updating a customers row scans orders"
            ),
            "{}",
            notes[0].1
        );
        assert_eq!(foreign_key_indexes(&s)[0].indexed, Some(true));

        // Large enough to be unused: the check flags it, and the note says what else relies on it.
        s.indexes[0].size_bytes = Some(8.0 * 1024.0 * 1024.0);
        let notes = notes_for(&s);
        assert_eq!(notes[0].0, Severity::High);
        assert!(notes[0].1.ends_with(
            "it also backs foreign key fk_orders_customer, so deletes and updates of customers rows rely on it"
        ));

        s.server.engine = Engine::Mysql;
        assert!(index_notes(&s, &[])[0].text.ends_with("fk_orders_customer: InnoDB requires it"));
    }

    #[test]
    fn a_disabled_index_says_what_its_engine_does_with_it() {
        let disabled_on = |engine: Engine| {
            let mut s = snapshot(10);
            s.server.engine = engine;
            let mut off = index("ix_off", &["channel"]);
            (off.is_disabled, off.reads) = (true, Some(4.0));
            s.indexes = vec![off];
            crate::analyze(s).summary.index_notes.into_iter().map(|n| (n.tone, n.text)).collect::<Vec<_>>()
        };
        let disabled = disabled_on(Engine::Mssql);
        assert_eq!(disabled, [(Severity::Medium, "Disabled: unavailable to the optimizer and not maintained".into())]);
        let invisible = disabled_on(Engine::Mysql);
        assert_eq!(invisible[0].1, "Invisible to the optimizer; still maintained by writes");
        let invalid = disabled_on(Engine::Postgres);
        assert_eq!(invalid[0].1, "Invalid: unavailable to the optimizer; maintenance state was not recorded");
    }

    #[test]
    fn a_covered_index_points_at_what_covers_it_and_its_finding() {
        let mut s = snapshot(10);
        let narrow = index("ix_status", &["status"]);
        let mut wide = index("ix_status_created", &["status", "created_at"]);
        wide.reads = Some(90.0);
        s.indexes = vec![narrow, wide];
        let notes = crate::analyze(s).summary.index_notes;
        let of = |name: &str| notes.iter().filter(|n| n.index == name).collect::<Vec<_>>();
        let covered = of("ix_status");
        assert_eq!(covered.len(), 1, "covered, so the unread note leaves it to the covering one: {covered:?}");
        assert_eq!(covered[0].text, "Covered by ix_status_created: it leads with the same keys");
        assert_eq!(covered[0].tone, Severity::Medium);
        assert_eq!(covered[0].finding.as_deref(), Some("duplicate-index:shop.orders.ix_status"));
        assert!(of("ix_status_created").is_empty());
    }

    #[test]
    fn yugabyte_counters_without_a_known_period_and_a_catalog_without_sharding_say_unknown() {
        let mut s = snapshot(10);
        s.server.engine = Engine::Yugabyte;
        s.foreign_keys = vec![culprit_core::ForeignKey {
            name: "fk_orders_customer".into(),
            schema: "shop".into(),
            table: "orders".into(),
            columns: vec!["customer_id".into()],
            ref_schema: "shop".into(),
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            delete_rule: None,
        }];
        s.indexes = vec![index("ix_orders_status", &["status"])];
        let notes = index_notes(&s, &[]);
        assert_eq!(notes[0].tone, Severity::Low);
        assert!(
            notes[0].text.starts_with("No reads recorded over a period the counters don't establish"),
            "{}",
            notes[0].text
        );
        assert_eq!(foreign_key_indexes(&s)[0].indexed, None, "no sharding metadata: absence isn't established");
        s.indexes[0].docdb =
            Some(culprit_core::signals::DocdbIndex { hash_columns: vec!["status".into()], tablets: Some(3) });
        let mut elsewhere = index("ix_customers_email", &["email"]);
        elsewhere.table = "customers".into();
        s.indexes.push(elsewhere);
        assert_eq!(
            foreign_key_indexes(&s)[0].indexed,
            Some(false),
            "another table's missing metadata says nothing here"
        );
        s.server.engine = Engine::Postgres;
        assert_eq!(foreign_key_indexes(&s)[0].indexed, Some(false), "a complete catalog establishes it");
    }
}
