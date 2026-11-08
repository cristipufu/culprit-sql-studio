//! Property tests over the parsers that read what people paste or point Culprit at: query plans in every format, SQL
//! (redaction, fingerprints, the read-only check, the DDL linter), connection strings and framework query logs.
//! Each property runs on every test run with a fixed case count; for a deeper search, raise it:
//! `PROPTEST_CASES=20000 cargo test --locked -p culprit-engine --test all fuzz::`.
//! A failing case is shrunk to a minimal input and saved under proptest-regressions/ to be replayed first.

use std::path::Path;

use culprit_core::Engine;
use proptest::prelude::*;

const ENGINES: [Engine; 5] = [Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite];

fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(cases),
        ..ProptestConfig::default()
    }
}

/// Every plan the parser tests read: SQL Server XML, PostgreSQL text and JSON, MySQL JSON, tree and EXPLAIN ANALYZE,
/// MariaDB, SQLite, TimescaleDB and YugabyteDB.
fn plan_corpus() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut out = Vec::new();
    for dir in ["plans", "insights"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("README") || name == "samples.json" {
                continue;
            }
            out.push(std::fs::read_to_string(&path).unwrap());
        }
    }
    out.sort();
    assert!(out.len() > 40, "the plan corpus moved: {} files", out.len());
    out
}

/// One edit to a sample, at a fraction of its length so it applies to any sample.
#[derive(Clone, Debug)]
enum Edit {
    Truncate(f64),
    Delete(f64, usize),
    Duplicate(f64, usize),
    Insert(f64, String),
    Splice(f64, usize, f64, usize),
}

fn edit() -> impl Strategy<Value = Edit> {
    let at = 0.0..1.0f64;
    let piece = prop_oneof![
        Just("<".to_string()),
        Just(">".to_string()),
        Just("\"".to_string()),
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[]".to_string()),
        Just("  ->  ".to_string()),
        Just("\n".to_string()),
        Just("NaN".to_string()),
        Just("-1e309".to_string()),
        Just("\u{0}".to_string()),
        Just("é😀".to_string()),
        "[ -~]{0,12}",
    ];
    prop_oneof![
        at.clone().prop_map(Edit::Truncate),
        (at.clone(), 1..400usize).prop_map(|(a, n)| Edit::Delete(a, n)),
        (at.clone(), 1..400usize).prop_map(|(a, n)| Edit::Duplicate(a, n)),
        (at.clone(), piece).prop_map(|(a, s)| Edit::Insert(a, s)),
        (at.clone(), any::<usize>(), at, 1..400usize).prop_map(|(a, other, from, n)| Edit::Splice(a, other, from, n)),
    ]
}

fn boundary(s: &str, at: f64) -> usize {
    let mut i = ((s.len() as f64) * at) as usize;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i.min(s.len())
}

fn span(s: &str, at: f64, n: usize) -> (usize, usize) {
    let start = boundary(s, at);
    let mut end = (start + n).min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (start, end.max(start))
}

fn mutate(corpus: &[String], sample: usize, edits: &[Edit]) -> String {
    let mut s = corpus[sample % corpus.len()].clone();
    for e in edits {
        match e {
            Edit::Truncate(at) => s.truncate(boundary(&s, *at)),
            Edit::Delete(at, n) => {
                let (a, b) = span(&s, *at, *n);
                s.replace_range(a..b, "");
            }
            Edit::Duplicate(at, n) => {
                let (a, b) = span(&s, *at, *n);
                let copy = s[a..b].to_string();
                s.insert_str(b, &copy);
            }
            Edit::Insert(at, piece) => s.insert_str(boundary(&s, *at), piece),
            Edit::Splice(at, other, from, n) => {
                let source = &corpus[other % corpus.len()];
                let (a, b) = span(source, *from, *n);
                s.insert_str(boundary(&s, *at), &source[a..b]);
            }
        }
    }
    s
}

proptest! {
    #![proptest_config(config(400))]

    /// A damaged plan in any format is read or refused, never a crash, and what is read survives being saved.
    #[test]
    fn a_damaged_plan_is_read_or_refused(sample in any::<usize>(), edits in prop::collection::vec(edit(), 1..6)) {
        let corpus = plan_corpus();
        let text = mutate(&corpus, sample, &edits);
        if let Ok(plan) = culprit_engine::parse_plan(&text) {
            let json = serde_json::to_string(&plan).expect("a parsed plan serializes");
            let back: culprit_core::Plan = serde_json::from_str(&json).expect("a saved plan reads back");
            prop_assert_eq!(back.nodes().len(), plan.nodes().len());
        }
    }

    /// Arbitrary text pasted as a plan is refused, never a crash.
    #[test]
    fn arbitrary_text_is_not_a_plan_crash(text in "\\PC{0,400}") {
        let _ = culprit_engine::parse_plan(&text);
    }
}

/// Plans nested far deeper than any engine produces, in each format, on a thread with the 2 MiB stack the app's
/// workers have: refused or read, never a stack overflow (which no caller could catch).
#[test]
fn a_deeply_nested_plan_is_refused_not_a_stack_overflow() {
    let depth = 20_000;
    let pg_json =
        format!("[{{\"Plan\": {}{}}}]", "{\"Node Type\": \"Hash\", \"Plans\": [".repeat(depth), "]}".repeat(depth));
    let pg_text = (0..depth)
        .map(|i| format!("{}->  Hash  (cost=0.00..1.00 rows=1 width=4)", " ".repeat(2 + i * 6)))
        .fold("Hash Join  (cost=0.00..1.00 rows=1 width=4)".to_string(), |s, l| s + "\n" + &l);
    let mssql = format!(
        "<ShowPlanXML xmlns=\"http://schemas.microsoft.com/sqlserver/2004/07/showplan\"><BatchSequence><Batch><Statements>\
         <StmtSimple StatementText=\"SELECT 1\"><QueryPlan>{}{}</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>",
        "<RelOp NodeId=\"1\" PhysicalOp=\"Nested Loops\" LogicalOp=\"Inner Join\" EstimateRows=\"1\"><NestedLoops>"
            .repeat(depth),
        "</NestedLoops></RelOp>".repeat(depth)
    );
    let mysql_tree = (0..depth)
        .map(|i| format!("{}-> Nested loop inner join  (cost=1.00 rows=1)", " ".repeat(4 * i)))
        .collect::<Vec<_>>()
        .join("\n");
    let mysql_json = format!(
        "{{\"query_block\": {{\"select_id\": 1, {}\"table\": {{\"table_name\": \"t\"}}{}}}}}",
        "\"nested_loop\": [{\"query_block\": {".repeat(depth),
        "}}]".repeat(depth)
    );
    for (format, text) in [
        ("pg json", pg_json),
        ("pg text", pg_text),
        ("mssql xml", mssql),
        ("mysql tree", mysql_tree),
        ("mysql json", mysql_json),
    ] {
        let outcome = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || culprit_engine::parse_plan(&text).map(|p| p.nodes().len()))
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("{format}: the parser panicked"));
        // Read only if it fits a saved scan; every format here nests far deeper, so each is refused.
        assert!(outcome.is_err(), "{format}: a plan {depth} steps deep was read");
    }
}

/// A SQL fragment that is complete on its own (no open quote or comment), to surround a literal with.
fn sql_context() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("SELECT a, b".to_string()),
            Just(" FROM app.orders o".to_string()),
            Just(" JOIN app.lines l ON l.order_id = o.id".to_string()),
            Just(" /* a comment */".to_string()),
            Just(" -- a line comment\n".to_string()),
            Just(" AND x IN (1, 2, 3)".to_string()),
            Just(" ORDER BY 1".to_string()),
            Just(" WHERE".to_string()),
            Just(" @p1".to_string()),
            Just(" $1".to_string()),
            Just(" ?".to_string()),
            Just("\n\t ".to_string()),
        ],
        0..6,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(config(600))]

    /// A value in a string or number literal never survives redaction, wherever the statement puts it.
    #[test]
    fn redaction_leaves_no_literal(
        before in sql_context(),
        after in sql_context(),
        secret in "[A-Za-z][A-Za-z0-9]{9,20}",
        number in 100_000_000u64..u64::MAX,
        engine in prop::sample::select(ENGINES.to_vec()),
    ) {
        let sql = format!("{before} WHERE name = '{secret}' AND id = {number}{after}");
        let redacted = culprit_core::redact::redact_sql(&sql, engine);
        prop_assert!(!redacted.contains(&secret), "{engine:?}: {redacted}");
        prop_assert!(!redacted.contains(&number.to_string()), "{engine:?}: {redacted}");
        prop_assert_eq!(culprit_core::redact::redact_sql(&redacted, engine), redacted.clone(), "redacting twice changes nothing");
    }

    /// Statements that differ only in their literal values, signs included, are one statement to the workload. (MySQL's
    /// fingerprint keeps a sign as the digest it is matched against does.)
    #[test]
    fn literals_do_not_change_a_fingerprint(
        before in sql_context(),
        a in "[a-z0-9 ]{0,12}",
        b in "[a-z0-9 ]{0,12}",
        x in any::<i64>(),
        y in any::<i64>(),
        engine in prop::sample::select(vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Sqlite]),
    ) {
        let one = culprit_core::fingerprint::fingerprint(&format!("{before} WHERE s = '{a}' AND n = {x}"), engine);
        let two = culprit_core::fingerprint::fingerprint(&format!("{before} WHERE s = '{b}' AND n = {y}"), engine);
        prop_assert_eq!(one.key, two.key);
    }

    /// A statement that changes data is never taken for a read, whatever comments, case and spacing come before it.
    #[test]
    fn a_write_is_never_read_only(
        lead in prop::collection::vec(prop_oneof![Just(" "), Just("\n"), Just("/* x */"), Just("-- x\n"), Just("\t")], 0..4),
        verb in prop::sample::select(vec!["DELETE FROM app.orders", "delete from app.orders", "UPDATE app.orders SET a = 1", "INSERT INTO app.orders VALUES (1)", "DROP TABLE app.orders", "TRUNCATE TABLE app.orders", "ALTER TABLE app.orders ADD c int", "MERGE INTO app.orders USING s ON 1 = 1 WHEN MATCHED THEN DELETE"]),
        tail in sql_context(),
        engine in prop::sample::select(ENGINES.to_vec()),
    ) {
        let sql = format!("{}{verb}{tail}", lead.concat());
        prop_assert!(!culprit_core::sql::reads_only(&sql, engine), "{engine:?} took a write for a read: {sql}");
    }

    /// Arbitrary text through the statement checks and the DDL linter: an answer, never a crash.
    #[test]
    fn arbitrary_sql_is_checked_not_a_crash(text in "\\PC{0,300}", engine in prop::sample::select(ENGINES.to_vec())) {
        let _ = culprit_core::sql::check(&text, engine);
        let _ = culprit_core::sql::reads_only(&text, engine);
        let _ = culprit_core::redact::redact_sql(&text, engine);
        let _ = culprit_core::fingerprint::fingerprint(&text, engine);
    }

    /// A password in a pasted connection string never comes back in what the form shows.
    #[test]
    fn a_pasted_password_is_never_shown(
        password in "[A-Za-z0-9!#%&*+_.~-]{10,24}",
        user in "[a-z]{3,10}",
        host in "[a-z]{3,10}\\.example",
        style in 0..4usize,
    ) {
        let input = match style {
            0 => format!("postgres://{user}:{password}@{host}:5432/shop"),
            1 => format!("mysql://{user}:{password}@{host}/shop"),
            2 => format!("Server=tcp:{host},1433;Database=shop;User ID={user};Password={password};Encrypt=True"),
            _ => format!("host={host} port=5432 dbname=shop user={user} password={password}"),
        };
        if let Ok(parsed) = culprit_engine::parse_connection(&input, None) {
            let shown = serde_json::to_string(&parsed).unwrap();
            prop_assert!(!shown.contains(&password), "the password came back: {shown}");
            prop_assert!(parsed.has_password, "{input}");
        }
    }

    /// Arbitrary text as a connection string: refused or read, never a crash.
    #[test]
    fn arbitrary_connection_text_is_not_a_crash(text in "\\PC{0,200}") {
        let _ = culprit_engine::parse_connection(&text, None);
        let _ = culprit_engine::parse_connection_with_ssh(&text, Some(Engine::Postgres));
    }

    /// A damaged framework log (Rails, Django, JSON lines) is read line by line, never a crash.
    #[test]
    fn a_damaged_query_log_is_read_not_a_crash(
        sample in 0..3usize,
        edits in prop::collection::vec(edit(), 1..6),
        engine in prop::sample::select(ENGINES.to_vec()),
    ) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../culprit-querylog/tests/data");
        let logs: Vec<String> = ["rails.log", "django.log", "statements.jsonl"]
            .iter()
            .map(|f| std::fs::read_to_string(root.join(f)).unwrap())
            .collect();
        let text = mutate(&logs, sample, &edits);
        let _ = culprit_querylog::read(text.lines(), engine);
    }
}

proptest! {
    #![proptest_config(config(150))]

    /// The DDL linter and its fixes on arbitrary SQL against a real catalog: findings or none, never a crash, and
    /// applying the fixes twice changes nothing more.
    #[test]
    fn the_ddl_linter_answers_arbitrary_sql(before in sql_context(), text in "\\PC{0,200}", after in sql_context()) {
        let snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        let sql = format!("{before}{text}{after}");
        let _ = culprit_engine::lint_sql(&snapshot, &sql);
        let (fixed, _) = culprit_engine::apply_sql_fixes(&snapshot, &sql);
        let (again, applied) = culprit_engine::apply_sql_fixes(&snapshot, &fixed);
        prop_assert_eq!(applied, 0, "fixed SQL still had fixes: {}", fixed);
        prop_assert_eq!(again, fixed);
    }
}

/// Found by the fuzzer: 1e309 reads as infinity, which a saved plan writes as `null`, so the plan was read but its
/// scan could never be read back. Such a plan is refused, in whichever format it comes.
#[test]
fn a_number_past_a_double_is_refused_not_saved_unreadable() {
    let plans = [
        "-> Table scan on t  (cost=1e309 rows=1e309)",
        "Seq Scan on t  (cost=0.00..1e309 rows=1e309 width=4)",
        "-> Table scan on t  (cost=0.25 rows=-1e309)",
    ];
    let mut refused = 0;
    for text in plans {
        match culprit_engine::parse_plan(text) {
            Ok(plan) => {
                let json = serde_json::to_string(&plan).unwrap();
                assert!(serde_json::from_str::<culprit_core::Plan>(&json).is_ok(), "{text}");
            }
            Err(e) => {
                assert!(e.message.contains("too large to keep"), "{text}: {}", e.message);
                refused += 1;
            }
        }
    }
    assert!(refused > 0, "no format read the number as infinity, so nothing here exercises the refusal");
}
