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
// only affects estimates, one whose parameters aren't known, withheld flags with no conversion evidence, and the
// same mismatch in two statements (with a
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
    assert!(said(no_column).is_empty(), "warning flags alone do not establish a current column conversion");
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
            "A read from customers: Nested Loops spilled to tempdb · Medium · High\n\
                Nested Loops used tempdb for part of this execution’s work. The spill alone does not establish its elapsed cost or underlying cause.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Compare input rows and width with estimates, then inspect requested, granted and used memory, applicable limits and concurrent demand. Reduce unnecessary input work where possible; check native memory grant feedback eligibility and status before relying on it.\n\
                verify: Compare representative runs under equivalent load: spill warnings or volume, elapsed time, CPU and grant waiting. Fewer spills alone do not establish a better result; a larger grant can increase concurrency pressure.",
            "A type mismatch on customers.email stopped a read from customers from seeking an index · Medium · High\n\
                The database has to convert customers.email before comparing it with a parameter of a different type (`CONVERT_IMPLICIT(nvarchar(200),[culprit_demo].[shop].[customers].[email],0)=[@P1]`). SQL Server warns that the conversion keeps it from searching an index, so it reads every row instead. The application declares its parameters as `@P1 nvarchar(4000)`: Unicode strings compared with a non-Unicode column.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                metric: Column: customers.email\n\
                note: Implicit conversion\n\
                fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.\n\
                verify: Re-scan after the change; the warning should be gone from this query's plan.",
            "A read from customers: Nested Loops spilled to tempdb · Medium · High\n\
                Nested Loops used tempdb for part of this execution’s work. The spill alone does not establish its elapsed cost or underlying cause.\n\
                impact: 54% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Compare input rows and width with estimates, then inspect requested, granted and used memory, applicable limits and concurrent demand. Reduce unnecessary input work where possible; check native memory grant feedback eligibility and status before relying on it.\n\
                verify: Compare representative runs under equivalent load: spill warnings or volume, elapsed time, CPU and grant waiting. Fewer spills alone do not establish a better result; a larger grant can increase concurrency pressure.",
            "A read from orders: Gather Merge recorded temporary-file work · Medium · High\n\
                This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value.\n\
                impact: 45% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value.\n\
                verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.",
            "A read from orders: Query block reported a spill · Low · High\n\
                The operator reported a spill without a recorded merge-pass count. The warning alone does not establish temporary I/O volume, elapsed cost or the responsible memory setting.\n\
                impact: 77% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Identify whether the native spill came from filesort or an internal temporary table. Compare input size and the applicable sort_buffer_size or temporary-table limits before changing session memory, then test equivalent work under concurrency.\n\
                verify: Compare equivalent input and concurrency: merge passes or temporary-file work, latency and CPU. Measure the effect of any session-memory change; a zero pass count is not required for a better result.",
            "A read from orders has a recorded spill warning · Medium · High\n\
                The captured plan reports a spill. This warning alone does not establish its size, elapsed cost or cause.\n\
                impact: 50% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: Spills to disk\n\
                fix: Identify the spilling operator and compare actual input rows, row width, memory use and limits for representative executions. Establish the mechanism before changing statistics, indexes or memory settings.\n\
                verify: Obtain a measured plan in an owned test environment and compare equivalent inputs and concurrency: spill volume, latency, CPU and waiting. A spill need not disappear for the overall result to improve.",
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

fn tvp_conversion_snapshot(input: Option<culprit_core::plan_facts::RelationalInput>) -> Snapshot {
    let mut snapshot = snapshot();
    let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| query.id == "7").unwrap();
    query.text = "SELECT r.email FROM @Rows r WHERE r.email=@needle".into();
    query.parameters = None;
    let plan = query.plan.as_mut().unwrap();
    let expression = "CONVERT_IMPLICIT(nvarchar(64),[r].[email],0)=[@needle]";
    plan.statement = Some(query.text.clone());
    plan.raw = None;
    plan.kind = culprit_core::PlanKind::Actual;
    plan.root.id = 0;
    plan.root.kind = culprit_core::NodeKind::Scan;
    plan.root.op = "Table Scan".into();
    plan.root.object = Some("@Rows".into());
    plan.root.index = None;
    plan.root.children.clear();
    plan.root.seek_predicate = None;
    plan.root.predicate = Some(expression.into());
    plan.root.est_rows = 20_000.0;
    plan.root.act_rows = Some(20_000.0);
    plan.root.act_executions = Some(1.0);
    plan.root.rows_read = Some(20_000.0);
    plan.root.props.clear();
    plan.root.facts = culprit_core::PlanNodeFacts::default();
    plan.root.facts_mut().relational_input = input;
    plan.root.facts_mut().table_alias = Some("r".into());
    plan.root.facts_mut().access_rows = culprit_core::plan_facts::PlanAccessRows::new(Some(20_000.0), Some(20_000.0));
    plan.root.warnings = vec![culprit_core::PlanWarning::ImplicitConversion];
    plan.facts = culprit_core::PlanFacts::default();
    plan.facts_mut().conversions = vec![culprit_core::PlanConversion {
        issue: culprit_core::PlanConversionIssue::CardinalityEstimate,
        expression: expression.into(),
    }];
    snapshot
}

#[test]
fn a_native_tvp_conversion_finding_reuses_the_plans_type_contract_advice_and_verification() {
    use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};
    let snapshot = tvp_conversion_snapshot(Some(RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    }));
    let plan = snapshot
        .workload
        .as_ref()
        .unwrap()
        .queries
        .iter()
        .find(|query| query.id == "7")
        .unwrap()
        .plan
        .as_ref()
        .unwrap();
    let insight = culprit_core::insights::insights(plan)
        .into_iter()
        .find(|insight| insight.rule == "implicit-conversion")
        .unwrap();
    let report = culprit_checks::analyze(snapshot);
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.id.starts_with("plan-warnings:implicit-conversion:tvp:"))
        .expect("the query-owned TVP conversion finding");
    let fix = finding.fix.as_ref().unwrap();
    assert_eq!(fix.summary, insight.fix);
    assert!(fix.sql.is_none());
    assert_eq!(finding.verify, vec![insight.verify]);
    assert!(finding.summary.contains(&culprit_core::t!(
        "insight.implicit_conversion.summary_tvp",
        input = "@Rows",
        column = "r.email",
        target = "nvarchar(64)"
    )));
    assert!(finding.queries.iter().any(|query| query.id == "7"));
}

#[test]
fn a_local_or_unresolved_input_conversion_keeps_existing_scalar_conversion_english() {
    use culprit_core::plan_facts::{
        RelationalInput, RelationalInputKind as Kind, RelationalInputProvenance as Provenance,
    };
    for input in [
        None,
        Some(RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration }),
        Some(RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved }),
        Some(RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativePlan }),
    ] {
        let report = culprit_checks::analyze(tvp_conversion_snapshot(input));
        let finding =
            report.findings.iter().find(|finding| finding.id == "plan-warnings:implicit-conversion:r.email").unwrap();
        assert_eq!(
            finding.fix.as_ref().unwrap().summary,
            culprit_core::insights::mismatch_fix(Some("r.email"), Some("nvarchar(64)"))
        );
        assert_eq!(finding.verify, vec![culprit_core::t!("check.plan_warnings.verify.rescan")]);
    }
}

#[test]
fn tvp_conversion_findings_do_not_merge_different_query_or_input_contracts() {
    use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};
    let tvp = RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    };
    for other_is_tvp in [true, false] {
        let mut snapshot = tvp_conversion_snapshot(Some(tvp));
        let mut other =
            snapshot.workload.as_ref().unwrap().queries.iter().find(|query| query.id == "7").unwrap().clone();
        other.id = "conversion-peer".into();
        let object = if other_is_tvp { "@Other" } else { "shop.customers" };
        other.text = format!("SELECT r.email FROM {object} r WHERE r.email=@needle");
        let plan = other.plan.as_mut().unwrap();
        plan.statement = Some(other.text.clone());
        plan.root.object = Some(object.into());
        plan.root.facts_mut().relational_input = other_is_tvp.then_some(tvp);
        plan.facts_mut().conversions[0].issue = culprit_core::PlanConversionIssue::SeekPlan;
        snapshot.workload.as_mut().unwrap().queries.push(other);
        let report = culprit_checks::analyze(snapshot);
        let findings: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.id.starts_with("plan-warnings:implicit-conversion:"))
            .collect();
        assert_eq!(findings.len(), 2, "native contracts sharing an alias remain independent");
        assert_ne!(findings[0].id, findings[1].id);
        for finding in findings {
            assert_eq!(finding.queries.len(), 1, "a contract cannot borrow another query's impact or SQL reference");
            let query = &finding.queries[0].id;
            let input = match query.as_str() {
                "7" => Some("@Rows"),
                "conversion-peer" if other_is_tvp => Some("@Other"),
                "conversion-peer" => None,
                _ => panic!("unexpected query"),
            };
            let fix = &finding.fix.as_ref().unwrap().summary;
            if let Some(input) = input {
                assert!(finding.id.starts_with("plan-warnings:implicit-conversion:tvp:"));
                assert_eq!(
                    fix,
                    &culprit_core::t!("insight.implicit_conversion.fix_tvp", input = input, column = "r.email")
                );
                assert!(finding.summary.contains(&culprit_core::t!(
                    "insight.implicit_conversion.summary_tvp",
                    input = input,
                    column = "r.email",
                    target = "nvarchar(64)"
                )));
            } else {
                assert_eq!(finding.id, "plan-warnings:implicit-conversion:r.email");
                assert_eq!(fix, &culprit_core::insights::mismatch_fix(Some("r.email"), Some("nvarchar(64)")));
            }
        }
    }
}

#[test]
fn conversion_findings_share_the_native_warning_verdict_and_exact_operator_with_plan_insights() {
    use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};
    let tvp = RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    };
    for case in [
        "node-only",
        "unrelated-seek",
        "same-owner-seek",
        "ambiguous-owner",
        "duplicate-node",
        "scalar-only",
        "scalar-with-unrelated-column",
        "other-issue",
    ] {
        let mut snapshot = tvp_conversion_snapshot(Some(tvp));
        let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| query.id == "7").unwrap();
        let plan = query.plan.as_mut().unwrap();
        plan.root.id = 1;
        plan.facts_mut().conversions[0].issue = culprit_core::PlanConversionIssue::SeekPlan;
        let warning = plan.facts().conversions[0].clone();
        match case {
            "node-only" => {
                plan.facts_mut().conversions.clear();
                plan.root.facts_mut().conversions.push(warning);
                plan.root.predicate = None;
                plan.root.warnings.clear();
                plan.warnings.clear();
            }
            "same-owner-seek" => {
                plan.root.kind = culprit_core::NodeKind::Seek;
                plan.root.op = "Index Seek".into();
            }
            "unrelated-seek" | "ambiguous-owner" | "duplicate-node" => {
                let input = plan.root.clone();
                let mut peer = input.clone();
                peer.id = if case == "duplicate-node" { 1 } else { 2 };
                peer.kind = culprit_core::NodeKind::Seek;
                peer.op = "Index Seek".into();
                peer.self_cost = 100.0;
                if case != "ambiguous-owner" {
                    peer.object = Some("shop.customers".into());
                    peer.facts_mut().table_alias = Some("x".into());
                    peer.facts_mut().relational_input = None;
                    let expression = "CONVERT_IMPLICIT(nvarchar(128),[x].[email],0)=[@peer]";
                    peer.predicate = Some(expression.into());
                    peer.facts_mut().conversions = vec![culprit_core::PlanConversion {
                        issue: culprit_core::PlanConversionIssue::CardinalityEstimate,
                        expression: expression.into(),
                    }];
                }
                let mut root = input.clone();
                root.id = 0;
                root.kind = culprit_core::NodeKind::Join;
                root.op = "Hash Match".into();
                root.object = None;
                root.predicate = None;
                root.facts = culprit_core::PlanNodeFacts::default();
                root.warnings.clear();
                root.children = vec![input, peer];
                plan.root = root;
            }
            "scalar-only" => {
                let expression = "CONVERT_IMPLICIT(nvarchar(64),[@needle],0)";
                plan.facts_mut().conversions[0].expression = expression.into();
                plan.root.predicate = Some(expression.into());
            }
            "scalar-with-unrelated-column" => {
                plan.facts_mut().conversions[0].expression = "CONVERT_IMPLICIT(nvarchar(64),[@needle],0)".into();
            }
            "other-issue" => {
                plan.facts_mut().conversions[0].issue = culprit_core::PlanConversionIssue::Other("Future Issue".into())
            }
            _ => unreachable!(),
        }
        let evidence = culprit_core::insights::conversion_evidence(plan);
        let expected = evidence
            .as_ref()
            .filter(|evidence| evidence.has_native_warning())
            .map(|evidence| (evidence.node().map(|node| node.id), evidence.blocks_seek()));
        let report = culprit_checks::analyze(snapshot);
        let findings: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.id.starts_with("plan-warnings:implicit-conversion:"))
            .collect();
        if let Some((node, blocks_seek)) = expected {
            assert_eq!(findings.len(), 1, "{case}");
            let finding = findings[0];
            assert_eq!(finding.queries.len(), 1);
            assert_eq!(finding.queries[0].id, "7");
            assert_eq!(finding.queries[0].plan_node, node, "{case}");
            if matches!(case, "same-owner-seek" | "other-issue") {
                assert_eq!(finding.actionability, culprit_core::Actionability::Information);
                assert!(finding.summary.contains(&if case == "same-owner-seek" {
                    culprit_core::t!("insight.implicit_conversion.consequence.seek_observed")
                } else {
                    culprit_core::t!("insight.implicit_conversion.consequence.other_warning")
                }));
            }
            assert_eq!(
                finding.confidence,
                if blocks_seek { culprit_core::Confidence::High } else { culprit_core::Confidence::Medium },
                "{case}"
            );
        } else {
            assert!(findings.is_empty(), "{case}");
        }
    }
}

#[test]
fn merging_neutral_and_lost_seek_warnings_keeps_one_consistent_representative_in_both_orders() {
    for neutral_is_other in [false, true] {
        for strong_first in [false, true] {
            let mut snapshot = tvp_conversion_snapshot(None);
            let workload = snapshot.workload.as_mut().unwrap();
            let mut neutral = workload.queries.iter().find(|query| query.id == "7").unwrap().clone();
            neutral.text = "SELECT r.email FROM shop.customers r WHERE r.email=@needle".into();
            let plan = neutral.plan.as_mut().unwrap();
            plan.statement = Some(neutral.text.clone());
            plan.root.object = Some("shop.customers".into());
            plan.facts_mut().conversions[0].issue = if neutral_is_other {
                culprit_core::PlanConversionIssue::Other("Future Issue".into())
            } else {
                plan.root.kind = culprit_core::NodeKind::Seek;
                plan.root.op = "Index Seek".into();
                culprit_core::PlanConversionIssue::SeekPlan
            };
            let mut strong = neutral.clone();
            strong.id = "lost-seek".into();
            let plan = strong.plan.as_mut().unwrap();
            plan.root.kind = culprit_core::NodeKind::Scan;
            plan.root.op = "Index Scan".into();
            let expression = "CONVERT_IMPLICIT(nvarchar(128),[r].[email],0)=[@needle]";
            plan.root.predicate = Some(expression.into());
            plan.facts_mut().conversions[0] = culprit_core::PlanConversion {
                issue: culprit_core::PlanConversionIssue::SeekPlan,
                expression: expression.into(),
            };
            workload.queries = if strong_first { vec![strong, neutral] } else { vec![neutral, strong] };
            let report = culprit_checks::analyze(snapshot);
            let findings: Vec<_> = report
                .findings
                .iter()
                .filter(|finding| finding.id == "plan-warnings:implicit-conversion:r.email")
                .collect();
            assert_eq!(findings.len(), 1);
            let finding = findings[0];
            assert_eq!(finding.actionability, culprit_core::Actionability::NeedsAttention);
            assert_eq!(finding.confidence, culprit_core::Confidence::High);
            assert!(finding.summary.contains(expression));
            assert!(finding.summary.contains(&culprit_core::t!("check.plan_warnings.summary.blocks_seek")));
            assert!(
                !finding.summary.contains(&culprit_core::t!("insight.implicit_conversion.consequence.seek_observed"))
            );
            assert!(
                !finding.summary.contains(&culprit_core::t!("insight.implicit_conversion.consequence.other_warning"))
            );
            assert_eq!(
                finding.fix.as_ref().unwrap().summary,
                culprit_core::insights::mismatch_fix(Some("r.email"), Some("nvarchar(128)"))
            );
            let mut queries: Vec<_> = finding.queries.iter().map(|query| query.id.as_str()).collect();
            queries.sort_unstable();
            assert_eq!(queries, vec!["7", "lost-seek"]);
        }
    }
}
