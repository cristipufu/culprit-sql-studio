use super::{Engine, Token, readings};

const MAX_RELATIONAL_REFERENCES: usize = 64;
const MAX_RELATIONAL_SQL_BYTES: usize = 64 * 1024;

mod derived;
mod expressions;

/// An original SQL reference, retaining spelling and case. A named relation is not proof of its resolved object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MssqlRelationName {
    Parameter(String),
    Named(Vec<String>),
}

/// One relation in a closed top-level SELECT. Native callers must independently match every Object and its scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MssqlRelationReference {
    pub name: MssqlRelationName,
    pub alias: Option<String>,
}

/// Recognize a deliberately small SELECT/JOIN grammar, with agreement under every supported lexical mode.
/// Only bounded grouping, recognized built-ins and one closed parameter passthrough are admitted. Other nested
/// statements, unknown functions, CTEs, APPLY, implicit joins and table hints are not resolved.
/// Multiple relations require distinct explicit aliases. Nothing here infers variable kind or default schema.
pub fn mssql_relational_references(text: &str) -> Option<Vec<MssqlRelationReference>> {
    if text.len() > MAX_RELATIONAL_SQL_BYTES {
        return None;
    }
    let readings = readings(text, Engine::Mssql).ok()?;
    let mut candidates = readings.iter().map(|tokens| references(tokens));
    let first = candidates.next()??;
    candidates.all(|candidate| candidate.as_ref() == Some(&first)).then_some(first)
}

fn references(tokens: &[Token<'_>]) -> Option<Vec<MssqlRelationReference>> {
    let start = tokens.iter().take_while(|token| matches!(token, Token::Semicolon(_))).count();
    let end = tokens.iter().rposition(|token| !matches!(token, Token::Semicolon(_)))? + 1;
    let tokens = &tokens[start..end];
    if !word(tokens.first(), "SELECT") {
        return None;
    }
    let at = tokens.iter().position(|token| word(Some(token), "FROM"))?;
    let mut at = at + 1;
    let mut derived = None;
    let mut relations = vec![input(tokens, &mut at, &mut derived)?];
    while at < tokens.len() && !tail_clause(&tokens[at]) {
        let predicate = join(tokens, &mut at)?;
        if relations.len() == MAX_RELATIONAL_REFERENCES {
            return None;
        }
        relations.push(input(tokens, &mut at, &mut derived)?);
        if predicate {
            if !word(tokens.get(at), "ON") {
                return None;
            }
            at += 1;
            let start = at;
            at = predicate_end(tokens, at)?;
            if at == start {
                return None;
            }
        }
    }
    if tokens[at..].iter().any(|token| join_start(token) || word(Some(token), "ON")) {
        return None;
    }
    if !closed_scope(tokens, derived.as_ref()) || !distinct_aliases(&relations, derived.as_ref()) {
        return None;
    }
    Some(relations)
}

fn input(
    tokens: &[Token<'_>],
    at: &mut usize,
    derived: &mut Option<derived::DerivedRelation>,
) -> Option<MssqlRelationReference> {
    if tokens.get(*at) != Some(&Token::Punct('(')) {
        return relation(tokens, at);
    }
    if derived.is_some() {
        return None;
    }
    let (reference, scope) = derived::DerivedRelation::read(tokens, at)?;
    *derived = Some(scope);
    Some(reference)
}

fn closed_scope(tokens: &[Token<'_>], derived: Option<&derived::DerivedRelation>) -> bool {
    // Only the completely parsed relation may be excluded. Arbitrary nested SELECTs still fail the outer grammar.
    let outer;
    let tokens = if let Some(derived) = derived {
        outer = tokens
            .iter()
            .enumerate()
            .filter(|(at, _)| !derived.contains(*at))
            .map(|(_, token)| *token)
            .collect::<Vec<_>>();
        outer.as_slice()
    } else {
        tokens
    };
    !tokens.iter().skip(1).any(unsupported)
        && expressions::closed(tokens)
        && tokens.iter().filter(|token| word(Some(token), "FROM")).count() == 1
}

fn distinct_aliases(relations: &[MssqlRelationReference], derived: Option<&derived::DerivedRelation>) -> bool {
    if relations.len() == 1 && derived.is_none() {
        return true;
    }
    let mut aliases: Vec<&str> = Vec::new();
    for alias in
        relations.iter().map(|relation| relation.alias.as_deref()).chain(derived.map(|scope| Some(scope.alias())))
    {
        let Some(alias) = alias else { return false };
        if aliases.iter().any(|previous| alias.eq_ignore_ascii_case(previous)) {
            return false;
        }
        aliases.push(alias);
    }
    true
}

fn relation(tokens: &[Token<'_>], at: &mut usize) -> Option<MssqlRelationReference> {
    let name = match tokens.get(*at)? {
        Token::Word(name) if name.starts_with('@') && !name.starts_with("@@") && name.len() > 1 => {
            *at += 1;
            MssqlRelationName::Parameter((*name).into())
        }
        _ => {
            let mut parts = vec![identifier(tokens.get(*at)?)?];
            *at += 1;
            while tokens.get(*at) == Some(&Token::Punct('.')) {
                *at += 1;
                parts.push(identifier(tokens.get(*at)?)?);
                *at += 1;
                if parts.len() > 3 {
                    return None;
                }
            }
            MssqlRelationName::Named(parts)
        }
    };
    let alias = if word(tokens.get(*at), "AS") {
        *at += 1;
        let alias = identifier(tokens.get(*at)?)?;
        *at += 1;
        Some(alias)
    } else if let Some(alias) = tokens.get(*at).and_then(identifier) {
        *at += 1;
        Some(alias)
    } else {
        None
    };
    Some(MssqlRelationReference { name, alias })
}

fn identifier(token: &Token<'_>) -> Option<String> {
    match token {
        Token::Word(name) if !name.starts_with('@') && !reserved(name) => Some((*name).into()),
        Token::Quoted(name, delimiter) => {
            let close = char::from(*delimiter);
            let open = if *delimiter == b']' { '[' } else { close };
            Some(super::super::lexer::clean_identifier(&format!("{open}{name}{close}"))).filter(|name| !name.is_empty())
        }
        _ => None,
    }
}

fn join(tokens: &[Token<'_>], at: &mut usize) -> Option<bool> {
    let predicate = !word(tokens.get(*at), "CROSS");
    if ["INNER", "LEFT", "RIGHT", "FULL", "CROSS"].iter().any(|value| word(tokens.get(*at), value)) {
        let outer = ["LEFT", "RIGHT", "FULL"].iter().any(|value| word(tokens.get(*at), value));
        *at += 1;
        if outer && word(tokens.get(*at), "OUTER") {
            *at += 1;
        }
    }
    if !word(tokens.get(*at), "JOIN") {
        return None;
    }
    *at += 1;
    Some(predicate)
}

fn unsupported(token: &Token<'_>) -> bool {
    matches!(token, Token::Semicolon(_))
        || [
            "SELECT",
            "WITH",
            "APPLY",
            "UNION",
            "EXCEPT",
            "INTERSECT",
            "INTO",
            "FOR",
            "PIVOT",
            "UNPIVOT",
            "TABLESAMPLE",
            "OPTION",
        ]
        .iter()
        .any(|value| word(Some(token), value))
}

fn predicate_end(tokens: &[Token<'_>], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(start) {
        if depth == 0 {
            if join_start(token) || tail_clause(token) {
                return Some(at);
            }
            if *token == Token::Punct(',') || word(Some(token), "ON") {
                return None;
            }
        }
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    (depth == 0).then_some(tokens.len())
}

fn reserved(name: &str) -> bool {
    [
        "FROM", "AS", "ON", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER", "WHERE", "GROUP", "HAVING",
        "ORDER", "OFFSET", "FETCH",
    ]
    .iter()
    .any(|value| name.eq_ignore_ascii_case(value))
}

fn join_start(token: &Token<'_>) -> bool {
    ["JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS"].iter().any(|value| word(Some(token), value))
}

fn tail_clause(token: &Token<'_>) -> bool {
    ["WHERE", "GROUP", "HAVING", "ORDER", "OFFSET", "FETCH"].iter().any(|value| word(Some(token), value))
}

fn word(token: Option<&Token<'_>>, expected: &str) -> bool {
    matches!(token, Some(Token::Word(actual)) if actual.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests;
