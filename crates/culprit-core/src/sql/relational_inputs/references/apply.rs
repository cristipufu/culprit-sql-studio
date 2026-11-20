//! One single-row, direct-column APPLY projection. It adds an alias, never another native relation.
use super::{MssqlRelationName, MssqlRelationReference, Token, identifier, projection, unsupported, word};
use std::ops::Range;

pub(super) struct AppliedProjection {
    alias: String,
    range: Range<usize>,
}

pub(super) fn starts(tokens: &[Token<'_>], at: usize) -> bool {
    word(tokens.get(at), "CROSS") && word(tokens.get(at + 1), "APPLY")
}

impl AppliedProjection {
    pub(super) fn read(tokens: &[Token<'_>], at: &mut usize, source: &MssqlRelationReference) -> Option<Self> {
        let start = *at;
        if !starts(tokens, start)
            || !matches!(&source.name, MssqlRelationName::Parameter(_))
            || tokens.get(start + 2) != Some(&Token::Punct('('))
            || !word(tokens.get(start + 3), "SELECT")
        {
            return None;
        }
        let projection_start = start + 4;
        let end = tokens.iter().enumerate().skip(projection_start).find(|(_, token)| **token == Token::Punct(')'))?.0;
        if !projection::is_direct_columns(&tokens[projection_start..end], source.alias.as_deref()?)
            || !word(tokens.get(end + 1), "AS")
            || unsupported(tokens.get(end + 2)?)
        {
            return None;
        }
        let alias = identifier(tokens.get(end + 2)?)?;
        *at = end + 3;
        Some(Self { alias, range: start..*at })
    }

    pub(super) fn alias(&self) -> &str {
        &self.alias
    }

    pub(super) fn contains(&self, at: usize) -> bool {
        self.range.contains(&at)
    }
}
