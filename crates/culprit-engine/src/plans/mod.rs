//! Plans from outside a connection: pasted text or files saved by other tools. Reads SQL Server showplan XML
//! (.sqlplan), Postgres EXPLAIN as JSON or text, MySQL EXPLAIN FORMAT=JSON (versions 1 and 2) and MySQL's tree output
//! (EXPLAIN ANALYZE, FORMAT=TREE), SQLite's EXPLAIN QUERY PLAN as the sqlite3 shell prints it, plus Culprit's own plan
//! JSON. psql and mysql client decorations are stripped first.

#[cfg(any(feature = "postgres", feature = "mysql", feature = "yugabyte"))]
use culprit_core::PlanKind;
use culprit_core::{Error, Plan, PlanNode, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

mod pg_text;

/// The largest SQL Server plans run to a few megabytes; anything far beyond that is not a plan.
pub const MAX_PLAN_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanFormat {
    ShowplanXml,
    PostgresJson,
    YugabyteJson,
    PostgresText,
    MysqlJson,
    MysqlTree,
    SqliteText,
    Culprit,
}

impl PlanFormat {
    pub fn label(self) -> &'static str {
        match self {
            PlanFormat::ShowplanXml => "SQL Server showplan XML",
            PlanFormat::PostgresJson => "PostgreSQL EXPLAIN (FORMAT JSON)",
            PlanFormat::YugabyteJson => "YugabyteDB EXPLAIN (DIST, FORMAT JSON)",
            PlanFormat::PostgresText => "PostgreSQL EXPLAIN",
            PlanFormat::MysqlJson => "MySQL EXPLAIN FORMAT=JSON",
            PlanFormat::MysqlTree => "MySQL EXPLAIN ANALYZE / FORMAT=TREE",
            PlanFormat::SqliteText => "SQLite EXPLAIN QUERY PLAN",
            PlanFormat::Culprit => "Culprit plan JSON",
        }
    }
}

/// Decode a plan file: UTF-8 (with or without a byte-order mark) or UTF-16 with a byte-order mark, which is how SSMS
/// saves .sqlplan files. A UTF-16 file's XML declaration is rewritten to say UTF-8, which the text now is.
pub fn decode(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_PLAN_BYTES * 2 {
        return Err(too_large(bytes.len()));
    }
    let utf16 = |units: Vec<u16>| {
        String::from_utf16(&units)
            .map(|s| {
                s.replacen("encoding=\"utf-16\"", "encoding=\"utf-8\"", 1).replacen(
                    "encoding='utf-16'",
                    "encoding='utf-8'",
                    1,
                )
            })
            .map_err(|_| Error::invalid("The file says it is UTF-16 but isn't valid UTF-16"))
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()),
        [0xFE, 0xFF, rest @ ..] => utf16(rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect()),
        [0xEF, 0xBB, 0xBF, rest @ ..] => text(rest),
        _ => text(bytes),
    }
}

fn text(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        Error::invalid("This file isn't text")
            .with_hint("Open a .sqlplan, an EXPLAIN (FORMAT JSON) file, or EXPLAIN text output.")
    })
}

/// Which plan format the text is in, if Culprit reads it.
pub fn detect(text: &str) -> Option<PlanFormat> {
    let text = clean(text);
    detect_clean(text.trim())
}

/// Read a plan from text in any supported format. The plan keeps the text it came from in `raw` so it can be saved again.
pub fn parse_plan(input: &str) -> Result<Plan> {
    if input.len() > MAX_PLAN_BYTES {
        return Err(too_large(input.len()));
    }
    let cleaned = clean(input);
    // Keep the first line's indentation: text plans are read by column, and psql indents every line by one space.
    let block = cleaned.trim_end().trim_start_matches(['\n', '\r']);
    let block = match block.find(|c: char| !c.is_whitespace()) {
        Some(first) => &block[block[..first].rfind('\n').map_or(0, |n| n + 1)..],
        None => "",
    };
    let text = block.trim_start();
    if text.is_empty() {
        return Err(Error::invalid("There is no plan to read"));
    }
    let format = detect_clean(text).ok_or_else(|| unrecognized(text))?;
    let unreadable = |e: Error| {
        Error::invalid(format!("This looks like {} but Culprit couldn't read it: {}", format.label(), e.message))
    };
    let mut plan = match format {
        PlanFormat::ShowplanXml => showplan(text),
        PlanFormat::PostgresJson => postgres_json(text),
        PlanFormat::YugabyteJson => yugabyte_json(text),
        PlanFormat::PostgresText => postgres_text(block),
        PlanFormat::MysqlJson => mysql_json(text),
        PlanFormat::MysqlTree => mysql_tree(text),
        PlanFormat::SqliteText => sqlite_text(text),
        PlanFormat::Culprit => culprit_json(text),
    }
    .map_err(unreadable)?;
    // Before anything walks the tree: a deeper one would make the scan or file it's saved in unreadable.
    if plan.depth() > culprit_core::plan::MAX_PLAN_DEPTH {
        return Err(unreadable(culprit_core::plan::too_deep()));
    }
    if format != PlanFormat::Culprit {
        plan.raw = Some(block.into());
    }
    culprit_core::insights::annotate(&mut plan);
    culprit_checks::present_plan(&mut plan, None);
    // A number past what a double holds (1e309) reads as infinity, which a saved plan writes as nothing: the scan or
    // file it's saved in would never read back.
    let kept = serde_json::to_value(&plan).ok().is_some_and(|saved| Plan::deserialize(&saved).is_ok());
    if !kept {
        return Err(unreadable(Error::invalid("a number in it is too large to keep")));
    }
    Ok(plan)
}

fn detect_clean(text: &str) -> Option<PlanFormat> {
    if text.starts_with('<') {
        return text.contains("ShowPlanXML").then_some(PlanFormat::ShowplanXml);
    }
    if text.starts_with('{') || text.starts_with('[') {
        let value: Value = serde_json::from_str(text).ok()?;
        return json_format(unwrap_json(value)?.0.as_ref()?);
    }
    let first = text.lines().next()?.trim();
    if is_sqlite_text(text) {
        return Some(PlanFormat::SqliteText);
    }
    if first.starts_with("-> ") {
        return Some(PlanFormat::MysqlTree);
    }
    let pg_marks =
        |line: &str| line.contains("(cost=") || line.contains("(actual ") || line.contains("(never executed)");
    if pg_marks(first) || text.lines().any(|l| l.trim_start().starts_with("->  ")) {
        return Some(PlanFormat::PostgresText);
    }
    None
}

/// The plan object inside tool-specific wrappers: `[{"QUERY PLAN": [...]}]` (pgAdmin, psql -A), `{"EXPLAIN": "…"}`.
/// Returns the value and the statement text when a wrapper carried it.
fn unwrap_json(value: Value) -> Option<(Option<Value>, Option<String>)> {
    let mut value = value;
    let mut statement = None;
    for _ in 0..3 {
        let inner = match &value {
            Value::Array(items) if items.len() == 1 => items[0].as_object().and_then(wrapper_value),
            Value::Object(o) => wrapper_value(o),
            _ => None,
        };
        let Some(inner) = inner else { break };
        value = match inner {
            Value::String(s) => serde_json::from_str(&s).ok()?,
            other => other,
        };
    }
    let top = match &value {
        Value::Array(items) => items.first().and_then(Value::as_object),
        Value::Object(o) => Some(o),
        _ => None,
    };
    if let Some(top) = top {
        statement =
            ["Query Text", "query"].iter().find_map(|k| top.get(*k).and_then(Value::as_str)).map(str::to_string);
    }
    Some((Some(value), statement))
}

fn wrapper_value(o: &Map<String, Value>) -> Option<Value> {
    if o.len() != 1 {
        return None;
    }
    ["QUERY PLAN", "EXPLAIN", "explain"].iter().find_map(|k| o.get(*k).cloned())
}

fn json_format(value: &Value) -> Option<PlanFormat> {
    let top = match value {
        Value::Array(items) => items.first()?,
        other => other,
    };
    let o = top.as_object()?;
    if o.contains_key("Plan") {
        Some(
            if ["Storage Read Requests", "Storage Table Read Requests", "Storage Filter", "Storage Index Filter"]
                .iter()
                .any(|key| contains_key(value, key))
            {
                PlanFormat::YugabyteJson
            } else {
                PlanFormat::PostgresJson
            },
        )
    } else if o.contains_key("query_block") || (o.contains_key("operation") && o.contains_key("access_type")) {
        Some(PlanFormat::MysqlJson)
    } else if o.contains_key("root") && o.contains_key("engine") && o.contains_key("total_cost") {
        Some(PlanFormat::Culprit)
    } else {
        None
    }
}

fn unrecognized(text: &str) -> Error {
    let hint = "Paste or open a SQL Server plan (.sqlplan), PostgreSQL EXPLAIN output (text or FORMAT JSON), MySQL \
                EXPLAIN FORMAT=JSON / EXPLAIN ANALYZE output, or SQLite EXPLAIN QUERY PLAN output.";
    let first = text.lines().next().unwrap_or("").trim();
    let message = if text.contains("<explain xmlns=\"http://www.postgresql.org") || first.starts_with("- Plan:") {
        "PostgreSQL plans in XML or YAML aren't supported; use EXPLAIN (FORMAT JSON) or the default text format"
    } else if text.starts_with('<') {
        "This XML isn't a SQL Server execution plan"
    } else if text.contains("|--") {
        "This is a SQL Server text plan (SHOWPLAN_TEXT); Culprit reads the XML plan (SET SHOWPLAN_XML ON or Save Execution Plan As in SSMS)"
    } else if text.starts_with('{') || text.starts_with('[') {
        "This JSON isn't a plan Culprit knows"
    } else {
        "Culprit doesn't recognize this as a query plan"
    };
    Error::invalid(message).with_hint(hint)
}

fn too_large(len: usize) -> Error {
    Error::invalid(format!("This is {} MB, far larger than any query plan", len / (1024 * 1024)))
}

/// Strip what database clients wrap around a plan: psql's `QUERY PLAN` header, dashes, `+` continuations and
/// `(26 rows)` footer; the mysql client's table borders and `*** 1. row ***` / `EXPLAIN:` labels; quoted grid copies.
fn clean(text: &str) -> String {
    let text = text.trim_start_matches('\u{feff}');
    // The sqlite3 shell heads its tree with the `QUERY PLAN` line psql prints above a plan, dropped below: keep it.
    if is_sqlite_text(text) {
        return text.trim_end().to_string();
    }
    let is_border = |t: &str| !t.is_empty() && t.chars().all(|c| matches!(c, '-' | '+'));
    let is_header = |t: &str| matches!(t.trim_matches('|').trim(), "QUERY PLAN" | "EXPLAIN");
    let is_footer = |t: &str| {
        t.strip_prefix('(')
            .and_then(|r| r.strip_suffix(" rows)").or_else(|| r.strip_suffix(" row)")))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    let is_banner = |t: &str| t.starts_with("***") && t.ends_with("***") && t.contains(". row");
    let decorated = text.lines().any(|l| is_header(l.trim())) && text.lines().any(|l| is_border(l.trim()));
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if is_border(t) || is_header(t) || is_footer(t) || is_banner(t) {
            continue;
        }
        let mut line = line.trim_end().to_string();
        if decorated && line.ends_with('+') {
            line = line.trim_end_matches('+').trim_end().to_string();
        }
        if let Some(rest) = line.trim_start().strip_prefix("EXPLAIN: ") {
            line = rest.to_string();
        }
        if let Some(rest) = line.strip_prefix("| ") {
            line = rest.to_string();
        }
        if line.ends_with(" |") || line.ends_with("\t|") {
            line = line[..line.len() - 1].trim_end().to_string();
        }
        lines.push(line);
    }
    let non_empty: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if !non_empty.is_empty()
        && non_empty.iter().all(|l| l.trim().len() >= 2 && l.trim().starts_with('"') && l.trim().ends_with('"'))
    {
        lines = lines
            .iter()
            .map(|l| {
                let t = l.trim();
                if t.len() >= 2 { t[1..t.len() - 1].replace("\"\"", "\"") } else { t.to_string() }
            })
            .collect();
    }
    lines.join("\n")
}

fn showplan(text: &str) -> Result<Plan> {
    #[cfg(feature = "mssql")]
    {
        // Text pasted from a UTF-16 file keeps its declaration; the string is UTF-8 by now.
        let head = text.floor_char_boundary(120);
        let fixed;
        let text = if text[..head].contains("encoding=\"utf-16\"") {
            fixed = text.replacen("encoding=\"utf-16\"", "encoding=\"utf-8\"", 1);
            fixed.as_str()
        } else if text[..head].contains("encoding='utf-16'") {
            fixed = text.replacen("encoding='utf-16'", "encoding='utf-8'", 1);
            fixed.as_str()
        } else {
            text
        };
        culprit_mssql::plan_xml::parse(text)
    }
    #[cfg(not(feature = "mssql"))]
    {
        let _ = text;
        Err(Error::invalid("This build of Culprit has no SQL Server support"))
    }
}

fn postgres_json(text: &str) -> Result<Plan> {
    let value: Value = serde_json::from_str(text).map_err(|e| Error::invalid(e.to_string()))?;
    let (value, statement) = unwrap_json(value).ok_or_else(|| Error::invalid("No plan inside the JSON"))?;
    postgres_value(&value.unwrap_or(Value::Null), statement.as_deref())
}

fn postgres_value(value: &Value, statement: Option<&str>) -> Result<Plan> {
    #[cfg(feature = "postgres")]
    {
        let kind = if contains_key(value, "Actual Loops") { PlanKind::Actual } else { PlanKind::Estimated };
        culprit_postgres::plan_json::from_value(value, kind, statement)
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = (value, statement);
        Err(Error::invalid("This build of Culprit has no PostgreSQL support"))
    }
}

fn yugabyte_json(text: &str) -> Result<Plan> {
    #[cfg(feature = "yugabyte")]
    {
        let value: Value = serde_json::from_str(text).map_err(|e| Error::invalid(e.to_string()))?;
        let (value, statement) = unwrap_json(value).ok_or_else(|| Error::invalid("No plan inside the JSON"))?;
        let value = value.unwrap_or(Value::Null);
        let kind = if contains_key(&value, "Actual Loops") { PlanKind::Actual } else { PlanKind::Estimated };
        culprit_yugabyte::plan::from_value(&value, kind, statement.as_deref())
    }
    #[cfg(not(feature = "yugabyte"))]
    {
        let _ = text;
        Err(Error::invalid("This build of Culprit has no YugabyteDB support"))
    }
}

fn postgres_text(text: &str) -> Result<Plan> {
    postgres_value(&pg_text::to_json(text)?, None)
}

fn mysql_json(text: &str) -> Result<Plan> {
    let value: Value = serde_json::from_str(text).map_err(|e| Error::invalid(e.to_string()))?;
    let (value, statement) = unwrap_json(value).ok_or_else(|| Error::invalid("No plan inside the JSON"))?;
    let value = value.unwrap_or(Value::Null);
    let value = match value {
        Value::Array(mut items) if !items.is_empty() => items.swap_remove(0),
        other => other,
    };
    #[cfg(feature = "mysql")]
    {
        if value.get("query_block").is_some() {
            let json = serde_json::to_string(&value).map_err(|e| Error::internal(e.to_string()))?;
            return culprit_mysql::plan_json::parse(&json, PlanKind::Estimated, statement.as_deref(), None);
        }
        // Version 2 nodes carry the same operation text as the tree format; draw them through the tree reader.
        let mut tree = String::new();
        mysql_v2::tree(&value, 0, &mut tree);
        let mut plan = culprit_mysql::plan_tree::parse(&tree, statement.as_deref(), None)?;
        plan.kind = if contains_key(&value, "actual_rows") { PlanKind::Actual } else { PlanKind::Estimated };
        mysql_v2::fill_objects(&value, &mut plan.root);
        Ok(plan)
    }
    #[cfg(not(feature = "mysql"))]
    {
        let _ = (value, statement);
        Err(Error::invalid("This build of Culprit has no MySQL support"))
    }
}

fn is_sqlite_text(text: &str) -> bool {
    #[cfg(feature = "sqlite")]
    return culprit_sqlite::plan::is_text_plan(text);
    #[cfg(not(feature = "sqlite"))]
    {
        let _ = text;
        false
    }
}

fn sqlite_text(text: &str) -> Result<Plan> {
    #[cfg(feature = "sqlite")]
    return culprit_sqlite::plan::parse_text(text);
    #[cfg(not(feature = "sqlite"))]
    {
        let _ = text;
        Err(Error::invalid("This build of Culprit has no SQLite support"))
    }
}

fn mysql_tree(text: &str) -> Result<Plan> {
    #[cfg(feature = "mysql")]
    {
        let mut plan = culprit_mysql::plan_tree::parse(text, None, None)?;
        if !text.contains("(actual ") && !text.contains("(never executed)") {
            plan.kind = PlanKind::Estimated;
        }
        Ok(plan)
    }
    #[cfg(not(feature = "mysql"))]
    {
        let _ = text;
        Err(Error::invalid("This build of Culprit has no MySQL support"))
    }
}

/// A plan Culprit exported earlier. Node ids are renumbered so a hand-edited file can't confuse the UI.
fn culprit_json(text: &str) -> Result<Plan> {
    let mut plan: Plan = serde_json::from_str(text).map_err(|e| Error::invalid(e.to_string()))?;
    fn renumber(node: &mut PlanNode, next: &mut u32) {
        node.id = *next;
        *next += 1;
        node.children.iter_mut().for_each(|c| renumber(c, next));
    }
    renumber(&mut plan.root, &mut 0);
    Ok(plan)
}

fn contains_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(o) => o.contains_key(key) || o.values().any(|v| contains_key(v, key)),
        Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
        _ => false,
    }
}

#[cfg(feature = "mysql")]
mod mysql_v2 {
    use culprit_core::PlanNode;
    use serde_json::Value;

    /// Render a version 2 node as an EXPLAIN tree line, then its inputs four spaces deeper.
    pub fn tree(node: &Value, depth: usize, out: &mut String) {
        let op = node.get("operation").and_then(Value::as_str).unwrap_or("Operation");
        out.push_str(&"    ".repeat(depth));
        out.push_str("-> ");
        out.push_str(op.lines().next().unwrap_or(op));
        let num = |k: &str| node.get(k).and_then(Value::as_f64);
        let mut stats = Vec::new();
        if let Some(cost) = num("estimated_total_cost") {
            stats.push(format!("cost={cost}"));
        }
        if let Some(rows) = num("estimated_rows") {
            stats.push(format!("rows={rows}"));
        }
        if !stats.is_empty() {
            out.push_str(&format!("  ({})", stats.join(" ")));
        }
        if let Some(rows) = num("actual_rows") {
            let first = num("actual_first_row_ms").unwrap_or(0.0);
            let last = num("actual_last_row_ms").unwrap_or(first);
            let loops = num("actual_loops").unwrap_or(1.0);
            out.push_str(&format!(" (actual time={first}..{last} rows={rows} loops={loops})"));
        }
        out.push('\n');
        for input in node.get("inputs").and_then(Value::as_array).into_iter().flatten() {
            tree(input, depth + 1, out);
        }
    }

    /// Version 2 names each table and its schema, which the tree text only gives as an alias. Walks both trees
    /// together and stops wherever their shapes differ.
    pub fn fill_objects(json: &Value, node: &mut PlanNode) {
        if let Some(table) = json.get("table_name").and_then(Value::as_str) {
            node.object = Some(match json.get("schema_name").and_then(Value::as_str) {
                Some(schema) => format!("{schema}.{table}"),
                None => table.to_string(),
            });
        }
        if let Some(index) = json.get("index_name").and_then(Value::as_str) {
            node.index = Some(index.to_string());
        }
        let inputs: &[Value] = json.get("inputs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        if inputs.len() == node.children.len() {
            for (input, child) in inputs.iter().zip(node.children.iter_mut()) {
                fill_objects(input, child);
            }
        }
    }
}
