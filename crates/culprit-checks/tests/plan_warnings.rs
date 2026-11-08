//! Which of SQL Server's conversion warnings are a type mismatch worth a finding.

use culprit_core::Snapshot;

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).expect("fixture is a snapshot")
}

fn conversion_findings(snapshot: Snapshot) -> Vec<String> {
    culprit_checks::analyze(snapshot)
        .findings
        .into_iter()
        .map(|f| f.id)
        .filter(|id| id.starts_with("plan-warnings:") && id.contains("implicit-conversion"))
        .collect()
}

/// Query 7 of the demo compares `customers.email` (varchar) with an nvarchar parameter.
fn with_converts(converts: &str) -> Snapshot {
    let mut s = snapshot();
    let workload = s.workload.as_mut().expect("the demo has a workload");
    let plan = workload.queries.iter_mut().find(|q| q.id == "7").and_then(|q| q.plan.as_mut()).expect("query 7's plan");
    plan.props.insert("PlanAffectingConvert".into(), converts.into());
    // The conversions as the parser records them, one `issue: expression` a line: the plan's facts, not its display
    // properties, are what checks read.
    plan.facts_mut().conversions = converts
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(issue, expression)| culprit_core::PlanConversion {
            issue: culprit_core::PlanConversionIssue::from_source(issue),
            expression: expression.into(),
        })
        .collect();
    s
}

#[test]
fn a_column_converted_to_match_a_parameter_is_a_mismatch() {
    assert_eq!(conversion_findings(snapshot()), vec!["plan-warnings:implicit-conversion:customers.email"]);
}

#[test]
fn a_plan_captured_before_the_column_changed_type_is_not_a_mismatch() {
    // The demo's customers.email is varchar(200) and query 7's plan converts it. Once the column is nvarchar(200), the
    // plan Query Store kept describes a table that's gone.
    let mut s = snapshot();
    let email = s.columns.iter_mut().find(|c| c.table == "customers" && c.name == "email").expect("email");
    email.data_type = "nvarchar(200)".into();
    assert_eq!(conversion_findings(s), Vec::<String>::new());
}

#[test]
fn conversions_the_query_asks_for_are_not_a_mismatch() {
    // `SELECT CONVERT(varchar(36), c.id), CONVERT(varchar(30), c.created_at, 126) …`: SQL Server warns about every
    // CONVERT in the select list, though nothing is compared with a value of another type.
    let explicit = "Cardinality Estimate: CONVERT(varchar(36),[c].[id],0)\n\
                    Cardinality Estimate: CONVERT(varchar(30),[c].[created_at],126)";
    assert_eq!(conversion_findings(with_converts(explicit)), Vec::<String>::new());
    // Converting the parameter instead of the column costs nothing either.
    assert_eq!(
        conversion_findings(with_converts("Cardinality Estimate: CONVERT_IMPLICIT(varchar(64),[@P1],0)")),
        Vec::<String>::new()
    );
}

#[test]
fn a_mismatch_sql_server_says_stops_a_seek_is_titled_as_a_seek_not_any_use() {
    // Query 7's plan still reads ix_customers_email, by scanning it: the conversion stops a seek, not every use.
    let report = culprit_checks::analyze(snapshot());
    let f = report
        .findings
        .iter()
        .find(|f| f.id == "plan-warnings:implicit-conversion:customers.email")
        .expect("the mismatch");
    assert!(f.title.ends_with("from seeking an index"), "{}", f.title);
    assert!(!f.title.contains("using an index"), "{}", f.title);
}

// A saved compilation warning remains historical even when its columns appear in an index seek. Current catalog
// facts add context; they do not turn the warning into proof that a refresh is needed.
#[test]
fn a_saved_statistics_warning_keeps_current_catalog_facts_separate() {
    fn seek_on_priority(n: &mut culprit_core::PlanNode) -> Option<&mut culprit_core::PlanNode> {
        if n.index.as_deref() == Some("ix_support_tickets_priority") {
            return Some(n);
        }
        n.children.iter_mut().find_map(seek_on_priority)
    }
    let mut s = snapshot();
    let workload = s.workload.as_mut().expect("the demo has a workload");
    let plan =
        workload.queries.iter_mut().find(|q| q.id == "10").and_then(|q| q.plan.as_mut()).expect("query 10's plan");
    let seek = seek_on_priority(&mut plan.root).expect("the seek on priority");
    seek.warnings.push(culprit_core::PlanWarning::MissingStatistics);
    seek.props.insert("ColumnsWithNoStatistics".into(), "priority".into());
    seek.facts_mut().columns_without_statistics = vec!["priority".into()];
    let stats = s.statistics.iter_mut().find(|st| st.name == "ix_support_tickets_priority").expect("its statistics");
    stats.last_updated = Some("2026-08-28T08:58:07Z".into());
    stats.rows = Some(1.0);
    stats.modifications = Some(95.0);

    let report = culprit_checks::analyze(s);
    let f = report.findings.iter().find(|f| f.id == "plan-warnings:10:missing-statistics").expect("the warning");
    assert_eq!(f.actionability, culprit_core::Actionability::Information);
    assert!(f.title.contains("captured plan") && f.title.ends_with("statistics warning on priority"), "{}", f.title);
    assert!(
        f.summary.contains(
            "Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, when the table had 1 row, and have counted 95 changes since. The table held 300K rows when this plan was compiled, and holds 300K now."
        ),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("read like this"), "{}", f.summary);
    assert!(!f.fix.as_ref().and_then(|x| x.sql.as_deref()).unwrap_or_default().contains("UPDATE STATISTICS"));
    assert!(f.verify[0].contains("newly compiled plan") && f.verify[0].contains("original XML"));
}

fn walk(n: &mut culprit_core::PlanNode, f: &mut impl FnMut(&mut culprit_core::PlanNode)) {
    f(n);
    for child in &mut n.children {
        walk(child, f);
    }
}

fn query<'a>(s: &'a mut Snapshot, id: &str) -> &'a mut culprit_core::QueryStat {
    s.workload.as_mut().expect("a workload").queries.iter_mut().find(|q| q.id == id).expect("the query")
}

/// The demo `name`'s busiest statement that has a plan.
fn busiest_with_plan(s: &Snapshot) -> String {
    let w = s.workload.as_ref().expect("a workload");
    let q = w.queries.iter().filter(|q| q.plan.is_some()).max_by(|a, b| a.total_time_ms.total_cmp(&b.total_time_ms));
    q.expect("a statement with a plan").id.clone()
}

/// `s` with statement `id` stopped: it ran for a day, two days before the data ends.
fn stopped(mut s: Snapshot, id: &str) -> Snapshot {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let end = s.workload.as_ref().and_then(|w| w.window_end.as_deref()).and_then(epoch_seconds).expect("an end");
    let q = query(&mut s, id);
    (q.first_seen, q.last_seen) =
        (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)));
    s
}

/// `s` with statement 7 sent a second time as statement 70: the same plan, so the same mismatch, in two statements.
fn twice(mut s: Snapshot) -> Snapshot {
    let mut copy = query(&mut s, "7").clone();
    copy.id = "70".into();
    s.workload.as_mut().unwrap().queries.push(copy);
    s
}

/// `s` with `warning` on the root of statement `id`'s plan.
fn warned(mut s: Snapshot, id: &str, warning: culprit_core::PlanWarning) -> Snapshot {
    query(&mut s, id).plan.as_mut().expect("a plan").root.warnings.push(warning);
    s
}

// What plan-warnings says, word for word, beyond the demo's (a mismatch SQL Server says stops a seek): a mismatch that
// only affects estimates, one whose parameters or column aren't known, the same mismatch in two statements (with a
// day's time and without, stopped), a spill on each engine, a join without a condition, one the engine dropped and one
// asked for, and a statistics warning with what the catalog says now and without one. Its messages come from
// locales/en.json.
#[test]
fn plan_warnings_say_what_the_plan_warned_in_english() {
    use culprit_core::PlanWarning;
    assert_eq!(
        crate::info("plan-warnings"),
        "Plan warnings\n\
        Implicit conversions, spills, missing join predicates and missing statistics in the plans of heavy queries."
    );
    let said = |s: Snapshot| crate::run("plan-warnings", &s).iter().map(crate::words).collect::<Vec<_>>();
    let estimate =
        || with_converts("Cardinality Estimate: CONVERT_IMPLICIT(nvarchar(200),[shop].[customers].[email],0)");
    let mut declared = stopped(snapshot(), "7");
    query(&mut declared, "7").parameters = Some("@P1 varchar(200)".into());
    let mut undeclared = snapshot();
    query(&mut undeclared, "7").parameters = None;
    let mut no_column = snapshot();
    let plan = query(&mut no_column, "7").plan.as_mut().unwrap();
    plan.facts_mut().conversions.clear();
    walk(&mut plan.root, &mut |n| {
        n.facts_mut().conversions.clear();
        n.predicate = n.predicate.take().filter(|p| !p.contains("CONVERT"));
        n.seek_predicate = n.seek_predicate.take().filter(|p| !p.contains("CONVERT"));
    });
    let mut two_days = twice(snapshot());
    crate::last_hours(two_days.workload.as_mut().unwrap(), 48.0);
    two_days.workload.as_mut().unwrap().timeline = None;
    let start = two_days.workload.as_ref().unwrap().window_start.clone();
    for id in ["7", "70"] {
        let q = query(&mut two_days, id);
        (q.first_seen, q.trend) = (start.clone(), None);
    }
    assert_eq!(
        [
            said(estimate()),
            said(declared),
            said(undeclared),
            said(no_column),
            said(two_days),
            said(stopped(stopped(twice(estimate()), "7"), "70")),
        ]
        .concat(),
        [
            "A type mismatch on customers.email slows down a read from customers · Low · Medium\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[shop].[customers].[email],0)`). SQL Server warns that the conversion may affect its row estimates, which can lead it to a poorer plan; it doesn't say an index can't be used. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stopped a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 varchar(200)`.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. This often happens when an application sends strings as nvarchar (Unicode) to a varchar column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch slows down a read from customers · Low · Medium\n\
                The database has to convert a column before comparing it with a parameter of a different type. SQL Server warns that the conversion may affect its row estimates, which can lead it to a poorer plan; it doesn't say an index can't be used. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value with the column's own type, or change the column to the type the application sends if that's what it should hold. Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stops 2 queries from seeking an index · Low · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column. The same mismatch appears in 2 queries that together take 100% of database time.\n\
                impact: 100% of DB time\n\
                metric: Share of DB time\n\
                metric: Queries affected\n\
                metric: DB time per day\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email slowed down 2 queries · Low · Medium\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[shop].[customers].[email],0)`). SQL Server warns that the conversion may affect its row estimates, which can lead it to a poorer plan; it doesn't say an index can't be used. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column. The same mismatch appears in 2 queries that together take 100% of database time.\n\
                impact: 100% of DB time\n\
                metric: Share of DB time\n\
                metric: Queries affected\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
        ]
    );
    let spill = |name: &str| {
        let s: Snapshot = serde_json::from_str(
            &std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap(),
        )
        .unwrap();
        let id = busiest_with_plan(&s);
        said(warned(s, &id, PlanWarning::Spill))
    };
    assert_eq!(
        [
            said(warned(snapshot(), "7", PlanWarning::Spill)),
            said(warned(stopped(snapshot(), "7"), "7", PlanWarning::Spill)),
            spill("postgres-demo.json"),
            spill("mysql-demo.json"),
            spill("sqlite-demo.json"),
        ]
        .concat(),
        [
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers runs out of memory and spills to disk · Medium · High\n\
                A sort or hash in this plan needed more memory than it was granted and wrote the overflow to disk (tempdb or temp files). Spills usually come from a row estimate that was far too low, or a sort the index could have avoided.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Update statistics on the tables involved; if the estimate is still far off, add an index that delivers rows in the needed order so the sort disappears.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stopped a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers ran out of memory and spilled to disk · Medium · High\n\
                A sort or hash in this plan needed more memory than it was granted and wrote the overflow to disk (tempdb or temp files). Spills usually come from a row estimate that was far too low, or a sort the index could have avoided.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Update statistics on the tables involved; if the estimate is still far off, add an index that delivers rows in the needed order so the sort disappears.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from orders runs out of memory and spills to disk · Medium · High\n\
                A sort or hash in this plan needed more memory than it was granted and wrote the overflow to disk (tempdb or temp files). Spills usually come from a row estimate that was far too low, or a sort the index could have avoided.\n\
                impact: 45% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Raise work_mem for this workload (per role or per session), or add an index that delivers rows in the needed order so the sort disappears.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from orders runs out of memory and spills to disk · Low · High\n\
                A sort or hash in this plan needed more memory than it was granted and wrote the overflow to disk (tempdb or temp files). Spills usually come from a row estimate that was far too low, or a sort the index could have avoided.\n\
                impact: 77% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Raise tmp_table_size and max_heap_table_size for this workload so the temporary table stays in memory, or add an index that returns rows already ordered or grouped.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from orders runs out of memory and spills to disk · Medium · High\n\
                A sort or hash in this plan needed more memory than it was granted and wrote the overflow to disk (tempdb or temp files). Spills usually come from a row estimate that was far too low, or a sort the index could have avoided.\n\
                impact: 50% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Give the sort an index that returns rows in the order asked for; otherwise PRAGMA temp_store = MEMORY keeps temporary b-trees in memory, and a larger cache_size helps them fit.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
        ]
    );
    let join = |s: Snapshot, text: &str| {
        let mut s = warned(s, "7", PlanWarning::NoJoinPredicate);
        query(&mut s, "7").text = text.into();
        said(s)
    };
    assert_eq!(
        [
            join(snapshot(), "SELECT c.name, o.total FROM shop.customers c, shop.orders o WHERE c.email = @P1"),
            join(
                snapshot(),
                "SELECT c.name, o.total FROM shop.customers c JOIN shop.orders o ON o.customer_id = c.id WHERE c.id = @P1 AND o.customer_id = @P1"
            ),
            join(stopped(snapshot(), "7"), "SELECT c.name, s.code FROM shop.customers c CROSS JOIN shop.segments s"),
        ]
        .concat(),
        [
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers joins tables without a join condition · Medium · Medium\n\
                Part of this plan combines every row of one input with every row of another (a cross join), and the statement has no condition joining them: no ON clause and no comparison of one table's column with another's. A missing join condition multiplies the rows the query reads and returns.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: No join predicate\n\
                fix: Check the query's joins and add the missing condition.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers's plan joins two inputs without a join condition · Low · Medium\n\
                Part of this plan combines every row of one input with every row of another, though the statement does join them. SQL Server does this when both join columns are compared with the same value (a.x = @v AND a.x = b.x): it filters each side by the value and drops the join condition as redundant. That is harmless while both sides keep few rows.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: No join predicate\n\
                fix: Nothing to change while both inputs keep few rows. If the plan multiplies many rows, check the join columns and the filters on them in the plan.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A type mismatch on customers.email stopped a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers asked for a cross join · Low · Medium\n\
                Part of this plan combines every row of one input with every row of another, and the statement asks for it (CROSS JOIN or CROSS APPLY). That multiplies rows: fine for a small list of values, costly for two large tables.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: No join predicate\n\
                fix: Make sure the cross join is intended and both inputs stay small.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
        ]
    );
    // A statistics warning on query 10's seek on priority, and what the catalog says of it now.
    let statistics = |rows: Option<f64>, changes: Option<f64>, then: bool, now: bool| {
        let mut s = snapshot();
        let plan = query(&mut s, "10").plan.as_mut().expect("query 10's plan");
        walk(&mut plan.root, &mut |n| {
            if n.index.as_deref() == Some("ix_support_tickets_priority") {
                n.warnings.push(PlanWarning::MissingStatistics);
                n.props.insert("ColumnsWithNoStatistics".into(), "priority".into());
                n.facts_mut().columns_without_statistics = vec!["priority".into()];
            }
            if !then {
                n.table_rows = None;
            }
        });
        let stats = s.statistics.iter_mut().find(|st| st.name == "ix_support_tickets_priority").unwrap();
        (stats.last_updated, stats.rows, stats.modifications) = (Some("2026-08-28T08:58:07Z".into()), rows, changes);
        if !now {
            s.tables.iter_mut().filter(|t| t.name == "support_tickets").for_each(|t| t.rows = None);
        }
        said(s)
    };
    let mut no_insight = snapshot();
    query(&mut no_insight, "10").plan.as_mut().unwrap().warnings.push(PlanWarning::MissingStatistics);
    assert_eq!(
        [
            statistics(Some(1.0), Some(95.0), true, true),
            statistics(Some(5_000.0), Some(1.0), true, false),
            statistics(None, Some(0.0), false, true),
            statistics(Some(5_000.0), None, false, false),
            said(no_insight),
        ]
        .concat(),
        [
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, when the table had 1 row, and have counted 95 changes since. The table held 300K rows when this plan was compiled, and holds 300K now.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, when the table had 5.0K rows, and have counted 1 change since. The table held 300K rows when this plan was compiled.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, with no changes counted since. The table holds 300K rows now.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, when the table had 5.0K rows.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning · Low · Medium\n\
                SQL Server reported unavailable statistics at compilation time. The saved warning alone does not establish missing or stale statistics now, or a slow plan.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect current column statistics and automatic statistics settings, then compare a newly compiled plan before choosing a statistics change.\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
        ]
    );
    // The catalog's other readings: neither rows nor changes, changes only, rows and none since.
    assert_eq!(
        [
            statistics(None, None, true, true),
            statistics(None, Some(3.0), true, true),
            statistics(Some(5_000.0), Some(0.0), true, true),
        ]
        .concat(),
        [
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28. The table held 300K rows when this plan was compiled, and holds 300K now.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, and have counted 3 changes since. The table held 300K rows when this plan was compiled, and holds 300K now.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
            "A type mismatch on customers.email stops a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "The captured plan for a read from support_tickets reported a statistics warning on priority · Low · Medium\n\
                SQL Server reported no usable statistics on priority when this plan was compiled. These columns appear in seek keys of ix_support_tickets_priority. An index has statistics, but its histogram describes only the first key; the seek alone does not establish that each named column has a histogram or that the statistics are out of date. Query Store can retain the original warning after statistics change. Current catalog: The statistics of ix_support_tickets_priority were last updated on Aug 28, when the table had 5.0K rows, with no changes counted since. The table held 300K rows when this plan was compiled, and holds 300K now.\n\
                impact: 14% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Missing statistics\n\
                fix: Inspect the current column and index statistics and AUTO_CREATE_STATISTICS before choosing a change. An index histogram describes only its first key, so a column appearing in a seek does not prove that it has a usable histogram. Refresh or create statistics only where current evidence supports it; compare actual rows with estimates if the query is costly.\n\
                sql: SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();\n\
                verify: Capture a newly compiled plan after any justified statistics change and compare its warning and estimates with representative runs. Query Store may retain the original XML, so a re-scan of that saved plan alone cannot verify the change.",
        ]
    );
}
