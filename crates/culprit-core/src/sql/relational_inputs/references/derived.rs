//! One fully consumed passthrough relation. It contributes the original physical alias, never the outer alias.
use super::{MssqlRelationName, MssqlRelationReference, Token, identifier, projection, relation, unsupported, word};
use std::ops::Range;

pub(super) struct DerivedRelation {
    alias: String,
    range: Range<usize>,
}

impl DerivedRelation {
    pub(super) fn read(tokens: &[Token<'_>], at: &mut usize) -> Option<(MssqlRelationReference, Self)> {
        let start = *at;
        let source = read_body(tokens, at)?;
        if !matches!(&source.name, MssqlRelationName::Parameter(_))
            || !word(tokens.get(*at), "AS")
            || unsupported(tokens.get(*at + 1)?)
        {
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

/// The complete passthrough body has one explicitly aliased source and no hidden relation or operation.
/// Its caller qualifies that source as an original parameter or an exact already-declared CTE.
pub(super) fn read_body(tokens: &[Token<'_>], at: &mut usize) -> Option<MssqlRelationReference> {
    let start = *at;
    if tokens.get(start) != Some(&Token::Punct('(')) || !word(tokens.get(start + 1), "SELECT") {
        return None;
    }
    let projection_start = start + 2;
    let from = tokens.iter().enumerate().skip(projection_start).find(|(_, token)| word(Some(token), "FROM"))?.0;
    if !word(tokens.get(from + 2), "AS") {
        return None;
    }
    let mut inner = from + 1;
    let source = relation(tokens, &mut inner)?;
    if inner != from + 4
        || unsupported(tokens.get(inner - 1)?)
        || tokens.get(inner) != Some(&Token::Punct(')'))
        || !projection::is_direct(&tokens[projection_start..from], source.alias.as_deref()?)
    {
        return None;
    }
    *at = inner + 1;
    Some(source)
}
