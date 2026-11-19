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
            || !word(tokens.get(inner + 1), "AS")
            || unsupported(tokens.get(inner + 2)?)
        {
            return None;
        }
        let alias = identifier(tokens.get(inner + 2)?)?;
        *at = inner + 3;
        Some((source, Self { alias, range: start..*at }))
    }

    pub(super) fn alias(&self) -> &str {
        &self.alias
    }

    pub(super) fn contains(&self, at: usize) -> bool {
        self.range.contains(&at)
    }
}
