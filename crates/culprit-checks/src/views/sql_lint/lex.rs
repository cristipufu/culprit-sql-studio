//! Statement splitting, tokenization, and comment-preserving rewrites for DDL linting,
//! built on [`culprit_core::sql`] lexical primitives.

use culprit_core::sql::{block_comment_end, dollar_quoted_end, dollar_tag, line_end};

use super::unquote_ident;

pub(super) fn compact_sql(stmt: &str) -> String {
    stmt.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn contains_identifier(sql_text: &str, ident: &str) -> bool {
    let lower_sql = sql_text.to_ascii_lowercase();
    let lower_ident = ident.to_ascii_lowercase();
    if lower_ident.is_empty() {
        return false;
    }
    lower_sql.match_indices(&lower_ident).any(|(at, _)| {
        let is_word = |ch: Option<char>| ch.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let before = lower_sql[..at].chars().last();
        let after = lower_sql[at + lower_ident.len()..].chars().next();
        !is_word(before) && !is_word(after)
    })
}

pub(super) fn split_statements(sql: &str) -> Vec<String> {
    split_statement_spans(sql).into_iter().map(|(_, _, s)| s).collect()
}

fn parse_dollar_tag_str(sql: &str) -> Option<&str> {
    let tag = dollar_tag(sql.as_bytes(), 0)?;
    std::str::from_utf8(tag).ok()
}

fn skip_nested_block_comment(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    chars.next();
    let mut depth = 1usize;
    while let Some((_, ch)) = chars.next() {
        if ch == '/' && chars.peek().map(|(_, next_ch)| *next_ch) == Some('*') {
            chars.next();
            depth += 1;
        } else if ch == '*' && chars.peek().map(|(_, next_ch)| *next_ch) == Some('/') {
            chars.next();
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
    }
}

pub(super) fn split_statement_spans(sql: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut stmt_start: Option<usize> = None;
    let mut stmt_end: usize = 0;
    let mut chars = sql.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut in_bracket = false;
    let mut dollar_tag: Option<String> = None;
    while let Some((idx, c)) = chars.next() {
        if let Some(ref tag) = dollar_tag {
            current.push(c);
            stmt_end = idx + c.len_utf8();
            if c == '$' && sql[idx..].starts_with(tag.as_str()) {
                for _ in 0..tag.chars().count().saturating_sub(1) {
                    if let Some((end_i, ch)) = chars.next() {
                        current.push(ch);
                        stmt_end = end_i + ch.len_utf8();
                    }
                }
                dollar_tag = None;
            }
            continue;
        }
        if in_single && c == '\\' {
            current.push(c);
            if let Some((end_i, next_c)) = chars.next() {
                current.push(next_c);
                stmt_end = end_i + next_c.len_utf8();
            }
            continue;
        }
        if !in_single && !in_double && !in_backtick && !in_bracket {
            if c == '$'
                && let Some(tag) = parse_dollar_tag_str(&sql[idx..])
            {
                if stmt_start.is_none() {
                    stmt_start = Some(idx);
                }
                current.push(c);
                stmt_end = idx + c.len_utf8();
                for _ in 0..tag.chars().count().saturating_sub(1) {
                    if let Some((end_i, ch)) = chars.next() {
                        current.push(ch);
                        stmt_end = end_i + ch.len_utf8();
                    }
                }
                dollar_tag = Some(tag.to_string());
                continue;
            }
            if c == '-' && chars.peek().map(|(_, ch)| *ch) == Some('-') {
                for (_, ch) in chars.by_ref() {
                    if matches!(ch, '\n' | '\r') {
                        current.push(ch);
                        break;
                    }
                }
                continue;
            }
            if c == '/' && chars.peek().map(|(_, ch)| *ch) == Some('*') {
                skip_nested_block_comment(&mut chars);
                current.push(' ');
                continue;
            }
            if c == ';' {
                let trimmed = current.trim();
                if !trimmed.is_empty()
                    && let Some(start) = stmt_start.take()
                {
                    out.push((start, idx + 1, trimmed.to_string()));
                }
                current.clear();
                stmt_start = None;
                continue;
            }
        }
        if c == '\'' && !in_double && !in_backtick && !in_bracket {
            in_single = !in_single;
        } else if c == '"' && !in_single && !in_backtick && !in_bracket {
            in_double = !in_double;
        } else if c == '`' && !in_single && !in_double && !in_bracket {
            in_backtick = !in_backtick;
        } else if c == '[' && !in_single && !in_double && !in_backtick && !in_bracket {
            in_bracket = true;
        } else if c == ']' && in_bracket {
            if chars.peek().map(|(_, next_c)| *next_c) == Some(']') {
                current.push(c);
                if let Some((end_i, next_c)) = chars.next() {
                    current.push(next_c);
                    stmt_end = end_i + next_c.len_utf8();
                }
                continue;
            }
            in_bracket = false;
        }
        if !c.is_whitespace() {
            if stmt_start.is_none() {
                stmt_start = Some(idx);
            }
            stmt_end = idx + c.len_utf8();
        }
        current.push(c);
    }
    let trimmed = current.trim();
    if !trimmed.is_empty()
        && let Some(start) = stmt_start.take()
    {
        out.push((start, stmt_end, trimmed.to_string()));
    }
    out
}

pub(super) fn tokenize(stmt: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut close_delim: Option<char> = None;
    let mut dollar_tag: Option<String> = None;
    let mut chars = stmt.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if let Some(ref tag) = dollar_tag {
            current.push(c);
            if c == '$' && stmt[idx..].starts_with(tag.as_str()) {
                for _ in 0..tag.chars().count().saturating_sub(1) {
                    if let Some((_, ch)) = chars.next() {
                        current.push(ch);
                    }
                }
                dollar_tag = None;
            }
            continue;
        }
        if let Some(q) = close_delim {
            current.push(c);
            if q == '\'' && c == '\\' {
                if let Some((_, next_c)) = chars.next() {
                    current.push(next_c);
                }
                continue;
            }
            if c == q {
                if chars.peek().map(|(_, next_c)| *next_c) == Some(q) {
                    if let Some((_, next_c)) = chars.next() {
                        current.push(next_c);
                    }
                    continue;
                }
                close_delim = None;
            }
            continue;
        }
        if c == '$'
            && let Some(tag) = parse_dollar_tag_str(&stmt[idx..])
        {
            current.push(c);
            for _ in 0..tag.chars().count().saturating_sub(1) {
                if let Some((_, ch)) = chars.next() {
                    current.push(ch);
                }
            }
            dollar_tag = Some(tag.to_string());
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            close_delim = Some(c);
            current.push(c);
            continue;
        }
        if c == '[' {
            close_delim = Some(']');
            current.push(c);
            continue;
        }
        if c == '-' && chars.peek().map(|(_, ch)| *ch) == Some('-') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            for (_, ch) in chars.by_ref() {
                if matches!(ch, '\n' | '\r') {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek().map(|(_, ch)| *ch) == Some('*') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            skip_nested_block_comment(&mut chars);
            continue;
        }
        if c.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else if matches!(c, '(' | ')' | ',' | '=') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(c.to_string());
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(super) fn parse_paren_list(tokens: &[String], start: usize) -> Option<(Vec<String>, usize)> {
    if tokens.get(start).map(String::as_str) != Some("(") {
        return None;
    }
    let mut cols = Vec::new();
    let mut current: Option<String> = None;
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(start) {
        match tok.as_str() {
            "(" => {
                depth += 1;
            }
            ")" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(col) = current.take() {
                        cols.push(col);
                    }
                    return Some((cols, idx + 1));
                }
            }
            "," if depth == 1 => {
                if let Some(col) = current.take() {
                    cols.push(col);
                }
            }
            other if depth == 1 && current.is_none() => {
                let u = other.to_ascii_uppercase();
                if !matches!(u.as_str(), "ASC" | "DESC" | "NULLS" | "FIRST" | "LAST") {
                    current = Some(unquote_ident(other));
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn split_top_level_commas<'a>(
    tokens: &'a [String],
    upper: &'a [String],
) -> Vec<(&'a [String], &'a [String])> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate() {
        match tok.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                if start < idx {
                    out.push((&tokens[start..idx], &upper[start..idx]));
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    if start < tokens.len() {
        out.push((&tokens[start..], &upper[start..]));
    }
    out
}

fn scan_outside_quotes_and_comments(
    sql: &str,
    mut on_unquoted_byte: impl FnMut(&[u8], usize) -> bool,
    mut on_line_comment: impl FnMut(&[u8], usize, usize) -> bool,
) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut in_bracket = false;
    while i < bytes.len() {
        if in_single && bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if !in_single && !in_double && !in_backtick && !in_bracket {
            if bytes[i] == b'$'
                && let Some(tag) = dollar_tag(bytes, i)
            {
                i = dollar_quoted_end(bytes, i, tag).unwrap_or(bytes.len());
                continue;
            }
            if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
                let end = line_end(bytes, i);
                if on_line_comment(bytes, i, end) {
                    return Some(i);
                }
                i = end;
                continue;
            }
            if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                i = block_comment_end(bytes, i, true).unwrap_or(bytes.len());
                continue;
            }
            if on_unquoted_byte(bytes, i) {
                return Some(i);
            }
        }
        if bytes[i] == b'\'' && !in_double && !in_backtick && !in_bracket {
            if in_single && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            in_single = !in_single;
        } else if bytes[i] == b'"' && !in_single && !in_backtick && !in_bracket {
            if in_double && bytes.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            in_double = !in_double;
        } else if bytes[i] == b'`' && !in_single && !in_double && !in_bracket {
            if in_backtick && bytes.get(i + 1) == Some(&b'`') {
                i += 2;
                continue;
            }
            in_backtick = !in_backtick;
        } else if bytes[i] == b'[' && !in_single && !in_double && !in_backtick && !in_bracket {
            in_bracket = true;
        } else if bytes[i] == b']' && in_bracket {
            if bytes.get(i + 1) == Some(&b']') {
                i += 2;
                continue;
            }
            in_bracket = false;
        }
        i += 1;
    }
    None
}

pub(super) fn find_keyword_outside_comments(sql: &str, keyword: &str) -> Option<usize> {
    let kw = keyword.as_bytes();
    if kw.is_empty() {
        return None;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    scan_outside_quotes_and_comments(
        sql,
        |bytes, i| {
            i + kw.len() <= bytes.len()
                && bytes[i..i + kw.len()].eq_ignore_ascii_case(kw)
                && (i == 0 || !is_word_byte(bytes[i - 1]))
                && (i + kw.len() == bytes.len() || !is_word_byte(bytes[i + kw.len()]))
        },
        |_, _, _| false,
    )
}

pub(super) fn ends_with_line_comment(sql: &str) -> bool {
    scan_outside_quotes_and_comments(sql, |_, _| false, |bytes, _, end| end == bytes.len()).is_some()
}

pub(super) fn append_sql_clause(stmt: &str, clause: &str) -> String {
    let body = stmt.trim().strip_suffix(';').unwrap_or(stmt).trim_end();
    if ends_with_line_comment(body) { format!("{body}\n{}", clause.trim_start()) } else { format!("{body}{clause}") }
}

pub(super) fn rewrite_preserving_comments(orig_span: &str, rep_sql: &str) -> String {
    if !orig_span.contains("--") && !orig_span.contains("/*") {
        return rep_sql.to_string();
    }
    let rep_upper = rep_sql.to_ascii_uppercase();
    // 1. CREATE [UNIQUE] INDEX -> insert CONCURRENTLY right after INDEX in orig_span
    if rep_upper.starts_with("CREATE ")
        && rep_upper.contains(" INDEX CONCURRENTLY ")
        && let Some(pos) = find_keyword_outside_comments(orig_span, "INDEX")
    {
        let after_idx = pos + "INDEX".len();
        let mut rewritten = String::with_capacity(orig_span.len() + 16);
        rewritten.push_str(&orig_span[..after_idx]);
        rewritten.push_str(" CONCURRENTLY");
        rewritten.push_str(&orig_span[after_idx..]);
        if !rewritten.trim_end().ends_with(';') && rep_sql.ends_with(';') {
            if ends_with_line_comment(rewritten.trim_end()) {
                rewritten.push('\n');
            }
            rewritten.push(';');
        }
        return rewritten;
    }
    // 2. Suffix additions (WITH (ONLINE = ON); or ALGORITHM=INPLACE[ ,] LOCK=NONE;)
    for suffix in [" WITH (ONLINE = ON);", " ALGORITHM=INPLACE LOCK=NONE;", ", ALGORITHM=INPLACE, LOCK=NONE;"] {
        if rep_sql.ends_with(suffix.trim_start()) {
            return append_sql_clause(orig_span.trim_end(), suffix);
        }
    }
    // 3. Preserve leading comments before a rewritten statement (DROP INDEX CONCURRENTLY, REINDEX CONCURRENTLY, VACUUM)
    if let Some(first_kw) =
        ["DROP", "REINDEX", "VACUUM"].into_iter().find_map(|kw| find_keyword_outside_comments(orig_span, kw))
        && first_kw > 0
    {
        return format!("{}{rep_sql}", &orig_span[..first_kw]);
    }
    rep_sql.to_string()
}
