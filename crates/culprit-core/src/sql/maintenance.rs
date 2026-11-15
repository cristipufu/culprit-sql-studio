//! Classify the leading operation of one complete statement, never a keyword inside text or another command.
use super::lexer::{Token, readings};
use crate::{Engine, Error, Result, SqlSyntax, maintenance::MaintenanceKind};

pub fn maintenance_statement(text: &str, engine: Engine) -> Result<Option<MaintenanceKind>> {
    let statement = super::single_statement(text, engine)?;
    let readings = readings(statement, engine)?;
    let mut classifications = readings.iter().map(|tokens| classify(tokens, engine.sql_syntax()));
    let first = classifications.next().ok_or_else(invalid)??;
    for classification in classifications {
        if classification? != first {
            return Err(invalid());
        }
    }
    Ok(first)
}

fn classify(tokens: &[Token<'_>], syntax: SqlSyntax) -> Result<Option<MaintenanceKind>> {
    if !balanced(tokens) {
        return Err(invalid());
    }
    if word(tokens, 0, "CREATE") {
        return create_index(tokens, syntax);
    }
    match syntax {
        SqlSyntax::Mssql => mssql(tokens),
        SqlSyntax::Postgres => postgres(tokens),
        SqlSyntax::Mysql => mysql(tokens),
        SqlSyntax::Sqlite => Ok(if word(tokens, 0, "REINDEX") {
            Some(MaintenanceKind::IndexReindex)
        } else if word(tokens, 0, "VACUUM") {
            Some(MaintenanceKind::Vacuum)
        } else if word(tokens, 0, "ANALYZE") {
            Some(MaintenanceKind::Statistics)
        } else {
            None
        }),
    }
}

fn create_index(tokens: &[Token<'_>], syntax: SqlSyntax) -> Result<Option<MaintenanceKind>> {
    let mut at = 1;
    let replacement = word(tokens, at, "OR") && word(tokens, at + 1, "REPLACE");
    if replacement {
        if syntax != SqlSyntax::Mysql {
            return Ok(None);
        }
        at += 2;
    }
    while ["UNIQUE", "CLUSTERED", "NONCLUSTERED", "COLUMNSTORE", "SPATIAL", "FULLTEXT", "BITMAP"]
        .iter()
        .any(|modifier| word(tokens, at, modifier))
    {
        let allowed = match syntax {
            SqlSyntax::Mssql => ["UNIQUE", "CLUSTERED", "NONCLUSTERED", "COLUMNSTORE", "SPATIAL"]
                .iter()
                .any(|modifier| word(tokens, at, modifier)),
            SqlSyntax::Postgres | SqlSyntax::Sqlite => word(tokens, at, "UNIQUE"),
            SqlSyntax::Mysql => ["UNIQUE", "SPATIAL", "FULLTEXT"].iter().any(|modifier| word(tokens, at, modifier)),
        };
        if !allowed {
            return Err(invalid());
        }
        at += 1;
    }
    if !word(tokens, at, "INDEX") {
        return Ok(None);
    }
    at += 1;
    if word(tokens, at, "CONCURRENTLY") {
        if syntax != SqlSyntax::Postgres {
            return Err(invalid());
        }
        at += 1;
    }
    if word(tokens, at, "IF") && word(tokens, at + 1, "NOT") && word(tokens, at + 2, "EXISTS") {
        if syntax == SqlSyntax::Mssql {
            return Err(invalid());
        }
        at += 3;
    }
    // PostgreSQL permits an omitted index name, taking the next keyword ON directly.
    if word(tokens, at, "ON") {
        if syntax != SqlSyntax::Postgres {
            return Err(invalid());
        }
    } else {
        at = name_end(tokens, at).ok_or_else(invalid)?;
    }
    if syntax == SqlSyntax::Mysql && word(tokens, at, "USING") {
        at = name_end(tokens, at + 1).ok_or_else(invalid)?;
    }
    if !word(tokens, at, "ON") {
        return Err(invalid());
    }
    at += 1;
    if word(tokens, at, "ONLY") {
        if syntax != SqlSyntax::Postgres {
            return Err(invalid());
        }
        at += 1;
    }
    name_end(tokens, at).ok_or_else(invalid)?;
    Ok(Some(if replacement { MaintenanceKind::IndexReplace } else { MaintenanceKind::IndexCreate }))
}

fn mssql(tokens: &[Token<'_>]) -> Result<Option<MaintenanceKind>> {
    if word(tokens, 0, "ALTER") && word(tokens, 1, "INDEX") {
        let at = name_end(tokens, 2).ok_or_else(invalid)?;
        if !word(tokens, at, "ON") {
            return Err(invalid());
        }
        let at = name_end(tokens, at + 1).ok_or_else(invalid)?;
        return Ok(if word(tokens, at, "REBUILD") {
            Some(MaintenanceKind::IndexRebuild)
        } else if word(tokens, at, "REORGANIZE") {
            Some(MaintenanceKind::IndexReorganize)
        } else {
            None
        });
    }
    if word(tokens, 0, "UPDATE") && word(tokens, 1, "STATISTICS") {
        name_end(tokens, 2).ok_or_else(invalid)?;
        return Ok(Some(MaintenanceKind::Statistics));
    }
    Ok(None)
}

fn postgres(tokens: &[Token<'_>]) -> Result<Option<MaintenanceKind>> {
    if word(tokens, 0, "VACUUM") {
        return Ok(Some(MaintenanceKind::Vacuum));
    }
    if word(tokens, 0, "ANALYZE") {
        return Ok(Some(MaintenanceKind::Statistics));
    }
    if word(tokens, 0, "CLUSTER") {
        return cluster(tokens);
    }
    if !word(tokens, 0, "REINDEX") {
        return Ok(None);
    }
    let at = if tokens.get(1) == Some(&Token::Punct('(')) { group_end(tokens, 1).ok_or_else(invalid)? } else { 1 };
    if !["INDEX", "TABLE", "SCHEMA", "DATABASE", "SYSTEM"].iter().any(|kind| word(tokens, at, kind)) {
        return Err(invalid());
    }
    let at = at + 1 + usize::from(word(tokens, at + 1, "CONCURRENTLY"));
    name_end(tokens, at).ok_or_else(invalid)?;
    Ok(Some(MaintenanceKind::IndexReindex))
}

fn cluster(tokens: &[Token<'_>]) -> Result<Option<MaintenanceKind>> {
    let at = if tokens.get(1) == Some(&Token::Punct('(')) {
        group_end(tokens, 1).ok_or_else(invalid)?
    } else {
        1 + usize::from(word(tokens, 1, "VERBOSE"))
    };
    if at != tokens.len() {
        let at = name_end(tokens, at).ok_or_else(invalid)?;
        if word(tokens, at, "USING") {
            name_end(tokens, at + 1).ok_or_else(invalid)?;
        }
    }
    Ok(Some(MaintenanceKind::Cluster))
}

fn mysql(tokens: &[Token<'_>]) -> Result<Option<MaintenanceKind>> {
    if word(tokens, 0, "ALTER") && word(tokens, 1, "TABLE") {
        return added_index(tokens);
    }
    let kind = if word(tokens, 0, "OPTIMIZE") {
        MaintenanceKind::TableOptimize
    } else if word(tokens, 0, "REPAIR") {
        MaintenanceKind::TableRepair
    } else if word(tokens, 0, "ANALYZE") {
        MaintenanceKind::Statistics
    } else {
        return Ok(None);
    };
    let at = 1 + usize::from(word(tokens, 1, "NO_WRITE_TO_BINLOG") || word(tokens, 1, "LOCAL"));
    if !word(tokens, at, "TABLE") {
        return Err(invalid());
    }
    name_end(tokens, at + 1).ok_or_else(invalid)?;
    Ok(Some(kind))
}

fn added_index(tokens: &[Token<'_>]) -> Result<Option<MaintenanceKind>> {
    let at = name_end(tokens, 2).ok_or_else(invalid)?;
    if !word(tokens, at, "ADD") {
        return Ok(None);
    }
    let at = at + 1;
    let at = at + usize::from(["UNIQUE", "FULLTEXT", "SPATIAL"].iter().any(|modifier| word(tokens, at, modifier)));
    if !word(tokens, at, "INDEX") && !word(tokens, at, "KEY") {
        return Ok(None);
    }
    let mut at = at + 1;
    if word(tokens, at, "IF") && word(tokens, at + 1, "NOT") && word(tokens, at + 2, "EXISTS") {
        at += 3;
    }
    if tokens.get(at) != Some(&Token::Punct('(')) {
        at = name_end(tokens, at).ok_or_else(invalid)?;
    }
    if word(tokens, at, "USING") {
        at = name_end(tokens, at + 1).ok_or_else(invalid)?;
    }
    group_end(tokens, at).ok_or_else(invalid)?;
    Ok(Some(MaintenanceKind::IndexCreate))
}

fn word(tokens: &[Token<'_>], at: usize, value: &str) -> bool {
    matches!(tokens.get(at), Some(Token::Word(word)) if word.eq_ignore_ascii_case(value))
}

fn name_end(tokens: &[Token<'_>], at: usize) -> Option<usize> {
    let identifier = |token: Option<&Token<'_>>| {
        matches!(token, Some(Token::Word(name)) if !name.starts_with('@')) || matches!(token, Some(Token::Quoted(_, _)))
    };
    if !identifier(tokens.get(at)) {
        return None;
    }
    let mut at = at + 1;
    while tokens.get(at) == Some(&Token::Punct('.')) {
        if !identifier(tokens.get(at + 1)) {
            return None;
        }
        at += 2;
    }
    Some(at)
}

fn group_end(tokens: &[Token<'_>], start: usize) -> Option<usize> {
    if tokens.get(start) != Some(&Token::Punct('(')) {
        return None;
    }
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn balanced(tokens: &[Token<'_>]) -> bool {
    let mut depth = 0usize;
    for token in tokens {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => {
                let Some(remaining) = depth.checked_sub(1) else {
                    return false;
                };
                depth = remaining;
            }
            _ => {}
        }
    }
    depth == 0
}

fn invalid() -> Error {
    Error::invalid(crate::t!("collection.invalid_source"))
}

#[cfg(test)]
mod tests;
