//! Preserve rowsets only: a star or distinct direct columns of the one original source alias.
use super::{Token, identifier, unsupported, word};
use std::collections::BTreeSet;

pub(super) fn is_direct(tokens: &[Token<'_>], source_alias: &str) -> bool {
    if tokens == [Token::Punct('*')] {
        return true;
    }
    // The existing SQL-byte bound also bounds this list. No catalog/type or current schema is consulted.
    let mut inputs = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for column in tokens.split(|token| *token == Token::Punct(',')) {
        let Some((input, output)) = direct_column(column, source_alias) else { return false };
        // Database collation is not inferred; potentially case-sensitive duplicates remain unknown.
        if !inputs.insert(input.to_ascii_lowercase()) || !outputs.insert(output.to_ascii_lowercase()) {
            return false;
        }
    }
    !inputs.is_empty()
}

fn direct_column(tokens: &[Token<'_>], source_alias: &str) -> Option<(String, String)> {
    let (alias, name, output) = match tokens {
        [alias, Token::Punct('.'), name] => (alias, name, name),
        [alias, Token::Punct('.'), name, keyword, output] if word(Some(keyword), "AS") => (alias, name, output),
        _ => return None,
    };
    if [alias, name, output].into_iter().any(unsupported) || identifier(alias).as_deref() != Some(source_alias) {
        return None;
    }
    Some((identifier(name)?, identifier(output)?))
}
