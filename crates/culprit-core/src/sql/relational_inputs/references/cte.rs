//! One or two closed, single-use parameter passthroughs. The native relation keeps its original inner alias.
use super::{MssqlRelationName, MssqlRelationReference, Token, derived, identifier, unsupported, word};

pub(super) struct CteRelation {
    name: String,
    source: MssqlRelationReference,
    predecessor: Option<CtePredecessor>,
    alias: Option<String>,
}

struct CtePredecessor {
    name: String,
    alias: String,
}

impl CteRelation {
    pub(super) fn read(tokens: &[Token<'_>]) -> Option<(Self, usize)> {
        if !word(tokens.first(), "WITH") {
            return None;
        }
        let mut at = 1;
        let (name, source) = definition(tokens, &mut at)?;
        if !matches!(&source.name, MssqlRelationName::Parameter(_)) {
            return None;
        }
        let mut scope = Self { name, source, predecessor: None, alias: None };
        // Exactly one optional successor; another definition, recursion or nested scope remains unknown.
        if tokens.get(at) == Some(&Token::Punct(',')) {
            at += 1;
            let (name, reference) = definition(tokens, &mut at)?;
            scope = scope.follow(name, reference)?;
        }
        word(tokens.get(at), "SELECT").then_some((scope, at))
    }

    fn follow(self, name: String, reference: MssqlRelationReference) -> Option<Self> {
        let MssqlRelationName::Named(parts) = reference.name else { return None };
        let [predecessor] = parts.as_slice() else { return None };
        if predecessor != &self.name || name.eq_ignore_ascii_case(&self.name) {
            return None;
        }
        Some(Self {
            name,
            source: self.source,
            predecessor: Some(CtePredecessor { name: self.name, alias: reference.alias? }),
            alias: None,
        })
    }

    pub(super) fn resolve(
        &mut self,
        reference: MssqlRelationReference,
        explicit_alias: bool,
    ) -> Option<MssqlRelationReference> {
        let MssqlRelationName::Named(parts) = &reference.name else { return Some(reference) };
        let [name] = parts.as_slice() else { return Some(reference) };
        if self.predecessor.as_ref().is_some_and(|predecessor| name.eq_ignore_ascii_case(&predecessor.name)) {
            return None;
        }
        if !name.eq_ignore_ascii_case(&self.name) {
            return Some(reference);
        }
        if name != &self.name || self.alias.is_some() || !explicit_alias {
            return None;
        }
        self.alias = Some(reference.alias?);
        Some(self.source.clone())
    }

    pub(super) fn aliases(&self) -> impl Iterator<Item = &str> {
        self.predecessor.iter().map(|predecessor| predecessor.alias.as_str()).chain(self.alias.as_deref())
    }

    pub(super) fn is_bound(&self, relations: &[MssqlRelationReference]) -> bool {
        let conflicts_with_name = |alias: &str| {
            alias.eq_ignore_ascii_case(&self.name)
                || self.predecessor.as_ref().is_some_and(|predecessor| alias.eq_ignore_ascii_case(&predecessor.name))
        };
        self.alias.is_some()
            && self.aliases().all(|alias| !conflicts_with_name(alias))
            && relations
                .iter()
                .all(|relation| relation.alias.as_deref().is_some_and(|alias| !conflicts_with_name(alias)))
    }
}

fn definition(tokens: &[Token<'_>], at: &mut usize) -> Option<(String, MssqlRelationReference)> {
    if unsupported(tokens.get(*at)?) || !word(tokens.get(*at + 1), "AS") {
        return None;
    }
    let name = identifier(tokens.get(*at)?)?;
    *at += 2;
    Some((name, derived::read_body(tokens, at)?))
}
