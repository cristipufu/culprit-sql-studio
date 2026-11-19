//! One fully consumed passthrough relation. It contributes the original physical alias, never the outer alias.
use super::{MssqlRelationName, MssqlRelationReference, Token, identifier, relation, unsupported, word};
use std::ops::Range;

pub(super) struct DerivedRelation {
    alias: String,
    range: Range<usize>,
}

impl DerivedRelation {
    pub(super) fn read(tokens: &[Token<'_>], at: &mut usize) -> Option<(MssqlRelationReference, Self)> {
        let start = *at;
        let source = read_body(tokens, at)?;
        if !word(tokens.get(*at), "AS") || unsupported(tokens.get(*at + 1)?) {
            return None;
        }
        let alias = identifier(tokens.get(*at + 1)?)?;
        *at += 2;
        Some((source, Self { alias, range: start..*at }))
    }

    pub(super) fn alias(&self) -> &str {
        &self.alias
    }

    pub(super) fn contains(&self, at: usize) -> bool {
        self.range.contains(&at)
    }
}

/// The complete body shared by a derived relation and a single CTE. It cannot hide another relation or operation.
pub(super) fn read_body(tokens: &[Token<'_>], at: &mut usize) -> Option<MssqlRelationReference> {
    let start = *at;
    if tokens.get(start) != Some(&Token::Punct('('))
        || !word(tokens.get(start + 1), "SELECT")
        || tokens.get(start + 2) != Some(&Token::Punct('*'))
        || !word(tokens.get(start + 3), "FROM")
        || !word(tokens.get(start + 5), "AS")
    {
        return None;
    }
    let mut inner = start + 4;
    let source = relation(tokens, &mut inner)?;
    if !matches!(&source.name, MssqlRelationName::Parameter(_))
        || inner != start + 7
        || source.alias.is_none()
        || unsupported(tokens.get(start + 6)?)
        || tokens.get(inner) != Some(&Token::Punct(')'))
    {
        return None;
    }
    *at = inner + 1;
    Some(source)
}
