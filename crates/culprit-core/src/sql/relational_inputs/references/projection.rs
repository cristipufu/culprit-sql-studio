//! Preserve names and rowsets only: a star or distinct direct columns of the one original source alias.
use super::{Token, identifier, unsupported};
use std::collections::BTreeSet;

pub(super) fn is_direct(tokens: &[Token<'_>], source_alias: &str) -> bool {
    if tokens == [Token::Punct('*')] {
        return true;
    }
    // The existing SQL-byte bound also bounds this list. No catalog/type or current schema is consulted.
    let mut columns = BTreeSet::new();
    for column in tokens.split(|token| *token == Token::Punct(',')) {
        let [alias, Token::Punct('.'), name] = column else { return false };
        if unsupported(alias) || unsupported(name) || identifier(alias).as_deref() != Some(source_alias) {
            return false;
        }
        let Some(name) = identifier(name) else { return false };
        // Database collation is not inferred; even a potentially valid case-sensitive duplicate stays unknown.
        if !columns.insert(name.to_ascii_lowercase()) {
            return false;
        }
    }
    !columns.is_empty()
}
