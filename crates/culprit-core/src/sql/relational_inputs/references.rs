use super::{Engine, Token, readings};

const MAX_RELATIONAL_REFERENCES: usize = 64;

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
/// Functions, nested statements, derived tables, CTEs, APPLY, implicit joins and table hints are not resolved.
/// Multiple relations require distinct explicit aliases. Nothing here infers variable kind or default schema.
pub fn mssql_relational_references(text: &str) -> Option<Vec<MssqlRelationReference>> {
    let readings = readings(text, Engine::Mssql).ok()?;
    let mut candidates = readings.iter().map(|tokens| references(tokens));
    let first = candidates.next()??;
    candidates.all(|candidate| candidate.as_ref() == Some(&first)).then_some(first)
}

fn references(tokens: &[Token<'_>]) -> Option<Vec<MssqlRelationReference>> {
    let start = tokens.iter().take_while(|token| matches!(token, Token::Semicolon(_))).count();
    let end = tokens.iter().rposition(|token| !matches!(token, Token::Semicolon(_)))? + 1;
    let tokens = &tokens[start..end];
    if !word(tokens.first(), "SELECT") || tokens.iter().skip(1).any(unsupported) {
        return None;
    }
    let mut from = tokens.iter().enumerate().filter(|(_, token)| word(Some(token), "FROM"));
    let (at, _) = from.next()?;
    if from.next().is_some() {
        return None;
    }
    let mut at = at + 1;
    let mut relations = vec![relation(tokens, &mut at)?];
    while at < tokens.len() && !tail_clause(&tokens[at]) {
        let predicate = join(tokens, &mut at)?;
        if relations.len() == MAX_RELATIONAL_REFERENCES {
            return None;
        }
        relations.push(relation(tokens, &mut at)?);
        if predicate {
            if !word(tokens.get(at), "ON") {
                return None;
            }
            at += 1;
            let start = at;
            while at < tokens.len() && !join_start(&tokens[at]) && !tail_clause(&tokens[at]) {
                if tokens[at] == Token::Punct(',') || word(tokens.get(at), "ON") {
                    return None;
                }
                at += 1;
            }
            if at == start {
                return None;
            }
        }
    }
    if tokens[at..].iter().any(|token| join_start(token) || word(Some(token), "ON")) {
        return None;
    }
    if relations.len() > 1
        && relations.iter().enumerate().any(|(at, relation)| {
            relation.alias.as_ref().is_none_or(|alias| {
                relations[..at]
                    .iter()
                    .any(|previous| previous.alias.as_ref().is_some_and(|other| alias.eq_ignore_ascii_case(other)))
            })
        })
    {
        return None;
    }
    Some(relations)
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
    matches!(token, Token::Semicolon(_) | Token::Punct('(' | ')'))
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
