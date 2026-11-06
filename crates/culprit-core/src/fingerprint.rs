//! Statement fingerprints: a statement with its literal values taken out, so runs of the same statement with different
//! values group together. One implementation for every source that sees statements as written rather than normalized
//! by the engine: MySQL's slow query log (matched to performance_schema digests), and query logs an application's
//! framework writes (`culprit-querylog`).
//!
//! The engine decides what a quote means: MySQL's double quotes are strings and its `#` starts a comment; elsewhere a
//! double-quoted word is a name, and `#` is part of a name (SQL Server's temporary tables). Placeholders (`?`, `?1`,
//! `$1`, `:name`, `@name`) are values, whatever the driver wrote.

use crate::SqlSyntax;
use crate::model::Engine;

/// A statement's fingerprint (canonical tokens, for grouping and matching) and its display text (the statement as
/// written, with every literal replaced by `?`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub key: String,
    pub display: String,
}

struct Rules {
    backslash_escapes: bool,
    double_quote_is_string: bool,
    hash_comments: bool,
    backticks: bool,
    brackets: bool,
    /// `:name`, `@name` and `$name` are placeholders (SQLite; SQL Server's `@name`).
    named_parameters: &'static [char],
}

fn rules(engine: SqlSyntax) -> Rules {
    match engine {
        SqlSyntax::Mysql => Rules {
            backslash_escapes: true,
            double_quote_is_string: true,
            hash_comments: true,
            backticks: true,
            brackets: false,
            named_parameters: &[],
        },
        SqlSyntax::Postgres => Rules {
            backslash_escapes: false,
            double_quote_is_string: false,
            hash_comments: false,
            backticks: false,
            brackets: false,
            named_parameters: &[],
        },
        SqlSyntax::Mssql => Rules {
            backslash_escapes: false,
            double_quote_is_string: false,
            hash_comments: false,
            backticks: false,
            brackets: true,
            named_parameters: &['@'],
        },
        SqlSyntax::Sqlite => Rules {
            backslash_escapes: false,
            double_quote_is_string: false,
            hash_comments: false,
            backticks: true,
            brackets: true,
            named_parameters: &[':', '@', '$'],
        },
    }
}

pub fn fingerprint(sql: &str, engine: Engine) -> Fingerprint {
    let engine = engine.sql_syntax();
    let r = rules(engine);
    let mut tokens: Vec<String> = Vec::new();
    let mut display = String::with_capacity(sql.len());
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let space = |display: &mut String| {
        if !display.is_empty() && !display.ends_with(' ') {
            display.push(' ');
        }
    };
    let value = |tokens: &mut Vec<String>, display: &mut String| {
        tokens.push("?".into());
        display.push('?');
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Comments, including MySQL's executable `/*! … */` ones: the digest drops them too.
        if (r.hash_comments && c == '#')
            || (c == '-' && next == Some('-') && chars.get(i + 2).is_none_or(|c| c.is_whitespace() || !r.hash_comments))
        {
            while i < chars.len() && !matches!(chars[i], '\n' | '\r') {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            let nests = engine != SqlSyntax::Mysql;
            let mut depth = 1usize;
            i += 2;
            while i + 1 < chars.len() && depth > 0 {
                if nests && chars[i] == '/' && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                i = chars.len();
            }
            continue;
        }
        if c.is_whitespace() {
            space(&mut display);
            i += 1;
            continue;
        }
        // Strings (and hex/bit literals written x'..', b'..', N'..'); in MySQL, double-quoted ones too.
        let prefixed = (matches!(c, 'x' | 'X' | 'b' | 'B' | 'n' | 'N')
            || (engine == SqlSyntax::Postgres && matches!(c, 'e' | 'E')))
            && next == Some('\'')
            && !word_before(&chars, i);
        if c == '\'' || (c == '"' && r.double_quote_is_string) || prefixed {
            let quote = if prefixed { '\'' } else { c };
            let force_escapes = prefixed && matches!(c, 'e' | 'E');
            let quote_start = if prefixed { i + 1 } else { i };
            i = crate::redact::skip_quoted_smart(&chars, quote_start, quote, r.backslash_escapes, force_escapes, false)
                .0;
            value(&mut tokens, &mut display);
            continue;
        }
        // Quoted names: kept as written for display, lower-cased for the key.
        let close = match c {
            '`' if r.backticks => Some('`'),
            '[' if r.brackets => Some(']'),
            '"' if !r.double_quote_is_string => Some('"'),
            _ => None,
        };
        if let Some(close) = close {
            let start = i + 1;
            i += 1;
            while i < chars.len() {
                if chars[i] == close {
                    if chars.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            let name: String = chars[start..i.min(chars.len())].iter().collect();
            i += 1;
            tokens.push(name.to_lowercase());
            display.push(c);
            display.push_str(&name);
            display.push(close);
            continue;
        }
        // Postgres dollar-quoted string literals (`$$…$$` or `$tag$…$tag$`).
        if c == '$'
            && engine == SqlSyntax::Postgres
            && let Some(tag_end) = dollar_tag(&chars, i)
        {
            let tag = &chars[i..=tag_end];
            let body_start = tag_end + 1;
            i = chars
                .get(body_start..)
                .and_then(|rest| rest.windows(tag.len()).position(|w| w == tag).map(|p| body_start + p + tag.len()))
                .unwrap_or(chars.len());
            value(&mut tokens, &mut display);
            continue;
        }
        // Placeholders: `?` and `?1`, `$1`, and named ones where the engine has them.
        if c == '?' {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            value(&mut tokens, &mut display);
            continue;
        }
        if c == '$' && engine != SqlSyntax::Mysql && next.is_some_and(|n| n.is_ascii_digit()) {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            value(&mut tokens, &mut display);
            continue;
        }
        if r.named_parameters.contains(&c)
            && next.is_some_and(|n| n.is_alphabetic() || n == '_')
            && !(c == ':' && i > 0 && chars[i - 1] == ':')
        {
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            value(&mut tokens, &mut display);
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
            // 12, 1.5, 1e3, 0x1F
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '.'
                    || ((chars[i] == '-' || chars[i] == '+') && matches!(chars[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            value(&mut tokens, &mut display);
            continue;
        }
        let word_char =
            |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '@' || (ch == '#' && !r.hash_comments);
        if word_char(c) {
            let start = i;
            while i < chars.len()
                && word_char(chars[i])
                && !(engine == SqlSyntax::Postgres && chars[i] == '$' && dollar_tag(&chars, i).is_some())
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let lower = word.to_lowercase();
            let canonical = match lower.as_str() {
                "true" | "false" => "?".to_string(),
                "character" => "char".to_string(),
                // The MySQL digest names interval units by their internal names (SQL_TSI_DAY).
                w if engine == SqlSyntax::Mysql && w.starts_with("sql_tsi_") => {
                    w.trim_start_matches("sql_tsi_").to_string()
                }
                _ => lower,
            };
            if canonical == "?" {
                display.push('?');
            } else {
                display.push_str(&word);
            }
            tokens.push(canonical);
            continue;
        }
        // Operators: two-character ones stay together, `<>` is `!=`, and `...` is the digest's list marker.
        let pair: String = [Some(c), next].iter().flatten().collect();
        if c == '.' && next == Some('.') && chars.get(i + 2) == Some(&'.') {
            tokens.push("...".into());
            display.push_str("...");
            i += 3;
            continue;
        }
        if matches!(pair.as_str(), "<=" | ">=" | "<>" | "!=" | ":=" | "||" | "&&" | "<<" | ">>" | "::") {
            tokens.push(if pair == "<>" { "!=".into() } else { pair.clone() });
            display.push_str(&pair);
            i += 2;
            continue;
        }
        if c == ';' {
            i += 1;
            continue;
        }
        tokens.push(c.to_string());
        display.push(c);
        i += 1;
    }
    Fingerprint { key: collapse(tokens).join(" "), display: display.trim().to_string() }
}

fn word_before(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

fn dollar_tag(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    while i < chars.len() {
        match chars[i] {
            '$' => return Some(i),
            c if c.is_alphanumeric() || c == '_' => i += 1,
            _ => return None,
        }
    }
    None
}

/// Lists of values become `...` whatever their length (`IN (1, 2, 3)`, `IN (...)`, `?, ...`), and the MySQL digest's
/// `(unknown)` placeholder is a value.
fn collapse(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "("
            && tokens.get(i + 1).is_some_and(|t| t == "unknown")
            && tokens.get(i + 2).is_some_and(|t| t == ")")
        {
            out.push("?".into());
            i += 3;
            continue;
        }
        if tokens[i] == "?" || tokens[i] == "..." {
            let mut j = i + 1;
            let mut values = 1;
            while j + 1 < tokens.len() && tokens[j] == "," && (tokens[j + 1] == "?" || tokens[j + 1] == "...") {
                values += 1;
                j += 2;
            }
            if values > 1 || tokens[i] == "..." {
                out.push("...".into());
            } else {
                out.push("?".into());
            }
            i = j;
            continue;
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// A short stable id for a fingerprint (FNV-1a, 64 bits, hex).
pub fn id(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_quoted_word_is_a_name_outside_mysql() {
        let a = fingerprint(
            r#"SELECT "products".* FROM "products" WHERE "products"."category_id" = 3 LIMIT 20"#,
            Engine::Sqlite,
        );
        let b = fingerprint(
            r#"SELECT "products".* FROM "products" WHERE "products"."category_id" = 9 LIMIT 20"#,
            Engine::Sqlite,
        );
        assert_eq!(a.key, b.key);
        assert_eq!(a.display, r#"SELECT "products".* FROM "products" WHERE "products"."category_id" = ? LIMIT ?"#);
        // In MySQL the same quotes are strings.
        assert_eq!(fingerprint(r#"SELECT "a""#, Engine::Mysql).key, "select ?");
    }

    #[test]
    fn placeholders_are_values() {
        let key = |s: &str, e| fingerprint(s, e).key;
        assert_eq!(
            key("SELECT * FROM t WHERE a = ? AND b = ?2", Engine::Sqlite),
            key("SELECT * FROM t WHERE a = 1 AND b = 'x'", Engine::Sqlite)
        );
        assert_eq!(
            key("SELECT * FROM t WHERE a = :id", Engine::Sqlite),
            key("SELECT * FROM t WHERE a = $id", Engine::Sqlite)
        );
        assert_eq!(
            key("SELECT * FROM t WHERE a = $1", Engine::Postgres),
            key("SELECT * FROM t WHERE a = 7", Engine::Postgres)
        );
        // A cast is not a placeholder.
        assert!(key("SELECT a::text FROM t", Engine::Postgres).contains(":: text"));
        assert_eq!(
            key("SELECT * FROM [t] WHERE id = @p0", Engine::Mssql),
            key("SELECT * FROM [t] WHERE id = 5", Engine::Mssql)
        );
        // SQL Server's temporary tables keep their #.
        assert!(key("SELECT * FROM #work", Engine::Mssql).contains("#work"));
    }

    #[test]
    fn lists_of_any_length_are_one_shape() {
        let key = |s: &str| fingerprint(s, Engine::Sqlite).key;
        assert_eq!(key("SELECT * FROM t WHERE id IN (1, 2, 3)"), key("SELECT * FROM t WHERE id IN (?, ?)"));
    }

    #[test]
    fn dollar_quoted_strings_nested_comments_and_escaped_identifiers_are_normalized() {
        for engine in [Engine::Postgres, Engine::Yugabyte] {
            let a = fingerprint("SELECT $$secret_alpha$$, $tag$secret beta$tag$ FROM \"my\"\"table\"", engine);
            let b = fingerprint("SELECT 'other', $tag$different$tag$ FROM \"my\"\"table\"", engine);
            assert_eq!(a.key, b.key);
            assert!(!a.key.contains("secret"));
            assert!(!a.display.contains("secret"));
            assert!(a.display.contains("\"my\"\"table\""));

            let nested = fingerprint("SELECT /* outer /* inner */ still_comment_canary */ a FROM t", engine);
            assert!(!nested.key.contains("still_comment_canary"));
            assert!(!nested.display.contains("still_comment_canary"));
        }
        let cr = fingerprint("SELECT a FROM t -- comment\rWHERE id = 42", Engine::Postgres);
        assert_eq!(cr.key, "select a from t where id = ?");
    }

    #[test]
    fn fuzz_fingerprint_never_panics_or_leaks_planted_literals() {
        const TOKENS: &[&str] = &[
            "SELECT",
            "FROM",
            "WHERE",
            "orders",
            "AND",
            "IN",
            "(",
            ")",
            ",",
            " ",
            "\n",
            "\r",
            "'PLANTED_STR_CANARY'",
            "N'PLANTED_NSTR_CANARY'",
            "'escaped '' PLANTED_ESC_CANARY'",
            "-- PLANTED_COMMENT_CANARY\n",
            "-- PLANTED_CR_COMMENT_CANARY\r",
            "/* PLANTED_BLOCK_CANARY */",
            "987654321",
            "0xdeadbeef",
            "\"quoted\"\"ident\"",
            "[bracket]]ident]",
            "`backtick``ident`",
            "café",
            "日本語",
        ];
        let mut state = 0x243f_6a88_85a3_08d3_u64;
        let mut next_u64 = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..300 {
            let len = (next_u64() as usize % 14) + 2;
            let mut sql = String::new();
            for _ in 0..len {
                let idx = next_u64() as usize % TOKENS.len();
                sql.push_str(TOKENS[idx]);
                sql.push(' ');
            }
            for engine in Engine::ALL {
                let fp = fingerprint(&sql, engine);
                for canary in [
                    "PLANTED_STR_CANARY",
                    "PLANTED_NSTR_CANARY",
                    "PLANTED_ESC_CANARY",
                    "PLANTED_COMMENT_CANARY",
                    "PLANTED_CR_COMMENT_CANARY",
                    "PLANTED_BLOCK_CANARY",
                    "987654321",
                    "deadbeef",
                ] {
                    assert!(!fp.key.contains(&canary.to_lowercase()), "{engine:?} leaked {canary} in key: {sql:?}");
                    assert!(!fp.display.contains(canary), "{engine:?} leaked {canary} in display: {sql:?}");
                }
            }
            for pg_engine in [Engine::Postgres, Engine::Yugabyte] {
                let pg_sql = format!(
                    "{sql} WHERE payload = $tag$PLANTED_DOLLAR_CANARY$tag$ AND body =$$PLANTED_DOLLAR2$$ AND x = 'it\\'s' AND y = 'PLANTED_DOLLAR3'"
                );
                let fp = fingerprint(&pg_sql, pg_engine);
                assert!(!fp.key.contains("planted_dollar"));
                assert!(!fp.display.contains("PLANTED_DOLLAR"));
            }
            let my_fp = fingerprint("SELECT 'C:\\path\\', ' PLANTED_MYSQL_NO_BS'", Engine::Mysql);
            assert!(!my_fp.key.contains("planted_mysql_no_bs"));
            assert!(!my_fp.display.contains("PLANTED_MYSQL_NO_BS"));
        }
    }
}
