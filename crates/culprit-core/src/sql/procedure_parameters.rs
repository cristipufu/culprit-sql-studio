//! Bounded native procedure-header parsing. It supplies declared parameter kind, not input values or cardinality.

use super::lexer::{Token, readings};
use crate::Engine;
use std::collections::BTreeMap;

/// READONLY parameters from a complete, unambiguous native `CREATE/ALTER PROCEDURE` header. A caller must bind the
/// text to the exact observed compiled module; arbitrary SQL text or today's catalog cannot qualify an old plan.
/// Bodies, dynamic SQL, functions, procedure options and incomplete headers remain outside this narrow grammar.
pub fn mssql_procedure_table_parameters(prefix: &str) -> Option<BTreeMap<String, Vec<String>>> {
    for (at, letters) in prefix.as_bytes().windows(2).enumerate() {
        if !letters.eq_ignore_ascii_case(b"AS")
            || at > 0 && identifier_byte(prefix.as_bytes()[at - 1])
            || prefix.as_bytes().get(at + 2).is_some_and(|byte| identifier_byte(*byte))
        {
            continue;
        }
        let Ok(readings) = readings(&prefix[..at + 2], Engine::Mssql) else { continue };
        let candidates = readings.iter().map(|tokens| header(tokens)).collect::<Option<Vec<_>>>();
        let Some(candidates) = candidates else { continue };
        if let Some(first) = candidates.first()
            && candidates.iter().all(|candidate| candidate == first)
        {
            return Some(first.clone());
        }
    }
    None
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'@' | b'#' | b'$') || byte >= 0x80
}

fn header(tokens: &[Token<'_>]) -> Option<BTreeMap<String, Vec<String>>> {
    let mut at = tokens.iter().take_while(|token| matches!(token, Token::Semicolon(_))).count();
    if word(tokens, at, "CREATE") {
        at += 1;
        if word(tokens, at, "OR") {
            if !word(tokens, at + 1, "ALTER") {
                return None;
            }
            at += 2;
        }
    } else if word(tokens, at, "ALTER") {
        at += 1;
    } else {
        return None;
    }
    if !word(tokens, at, "PROC") && !word(tokens, at, "PROCEDURE") {
        return None;
    }
    at += 1;
    let module = identifier(tokens, &mut at)?;
    if module.len() > 2 || module.iter().any(|part| part.starts_with(['@', '#'])) {
        return None;
    }
    let end = tokens.len().checked_sub(1)?;
    if !word(tokens, end, "AS") {
        return None;
    }
    let mut parameters = BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    while at < end {
        let Token::Word(name) = tokens.get(at)? else { return None };
        if !name.starts_with('@') || name.starts_with("@@") || name.len() < 2 || !names.insert(*name) {
            return None;
        }
        at += 1;
        if word(tokens, at, "AS") {
            at += 1;
        }
        let data_type = identifier(tokens, &mut at)?;
        if data_type.len() > 2 || at > end {
            return None;
        }
        let tail = at;
        let mut depth = 0usize;
        while at < end {
            match tokens[at] {
                Token::Punct('(') => depth += 1,
                Token::Punct(')') => depth = depth.checked_sub(1)?,
                Token::Punct(',') if depth == 0 => break,
                Token::Semicolon(_) => return None,
                _ => {}
            }
            at += 1;
        }
        if depth != 0 {
            return None;
        }
        let readonly = tokens[tail..at]
            .iter()
            .any(|token| matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("READONLY")));
        if readonly {
            if at != tail + 1 || !word(tokens, tail, "READONLY") {
                return None;
            }
            parameters.insert((*name).into(), data_type);
        } else if tokens[tail..at]
            .iter()
            .any(|token| matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("WITH")))
        {
            return None;
        }
        if at < end {
            at += 1;
            if at == end {
                return None;
            }
        }
    }
    Some(parameters)
}

fn word(tokens: &[Token<'_>], at: usize, expected: &str) -> bool {
    matches!(tokens.get(at), Some(Token::Word(word)) if word.eq_ignore_ascii_case(expected))
}

fn identifier(tokens: &[Token<'_>], at: &mut usize) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    loop {
        let part = match tokens.get(*at)? {
            Token::Word(part) => (*part).to_string(),
            // The shared lexer records the closing delimiter for bracketed identifiers.
            Token::Quoted(part, b']') => part.replace("]]", "]"),
            Token::Quoted(part, b'"') => part.replace("\"\"", "\""),
            _ => return None,
        };
        if part.is_empty() || part.encode_utf16().count() > 128 {
            return None;
        }
        parts.push(part);
        *at += 1;
        if tokens.get(*at) != Some(&Token::Punct('.')) {
            return Some(parts);
        }
        *at += 1;
    }
}

#[cfg(test)]
mod tests;
