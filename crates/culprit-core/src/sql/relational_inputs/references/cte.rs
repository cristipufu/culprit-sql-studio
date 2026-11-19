//! One closed, single-use parameter passthrough. The native relation keeps its original inner alias.
use super::{MssqlRelationName, MssqlRelationReference, Token, derived, identifier, unsupported, word};

pub(super) struct CteRelation {
    name: String,
    source: MssqlRelationReference,
    alias: Option<String>,
}

impl CteRelation {
    pub(super) fn read(tokens: &[Token<'_>]) -> Option<(Self, usize)> {
        if !word(tokens.first(), "WITH") || !word(tokens.get(2), "AS") || unsupported(tokens.get(1)?) {
            return None;
        }
        let name = identifier(tokens.get(1)?)?;
        let mut at = 3;
        let source = derived::read_body(tokens, &mut at)?;
        if !word(tokens.get(at), "SELECT")
            || source.alias.as_deref().is_some_and(|alias| alias.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        Some((Self { name, source, alias: None }, at))
    }

    pub(super) fn resolve(
        &mut self,
        reference: MssqlRelationReference,
        explicit_alias: bool,
    ) -> Option<MssqlRelationReference> {
        let MssqlRelationName::Named(parts) = &reference.name else { return Some(reference) };
        let [name] = parts.as_slice() else { return Some(reference) };
        if !name.eq_ignore_ascii_case(&self.name) {
            return Some(reference);
        }
        if name != &self.name || self.alias.is_some() || !explicit_alias {
            return None;
        }
        self.alias = Some(reference.alias?);
        Some(self.source.clone())
    }

    pub(super) fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    pub(super) fn is_bound(&self, relations: &[MssqlRelationReference]) -> bool {
        self.alias.as_deref().is_some_and(|alias| !alias.eq_ignore_ascii_case(&self.name))
            && relations
                .iter()
                .all(|relation| relation.alias.as_deref().is_some_and(|alias| !alias.eq_ignore_ascii_case(&self.name)))
    }
}
