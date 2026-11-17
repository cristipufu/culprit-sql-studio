//! Check expression boundaries without interpreting values or broadening the relation grammar. Only known
//! unqualified built-ins may introduce argument parentheses; native Object/module binding remains independent.
//! SQL Server scalar UDF calls require a schema-qualified name, which this boundary deliberately refuses:
//! https://learn.microsoft.com/sql/relational-databases/user-defined-functions/execute-user-defined-functions
use super::{Token, word};

pub(super) const MAX_EXPRESSION_DEPTH: usize = 32;

pub(super) fn closed(tokens: &[Token<'_>]) -> bool {
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate() {
        match token {
            Token::Punct('(') => {
                if depth == MAX_EXPRESSION_DEPTH || !opening(tokens, at) {
                    return false;
                }
                depth += 1;
            }
            Token::Punct(')') => {
                let Some(remaining) = depth.checked_sub(1) else { return false };
                depth = remaining;
            }
            _ => {}
        }
    }
    depth == 0
}

fn opening(tokens: &[Token<'_>], at: usize) -> bool {
    let Some(previous) = at.checked_sub(1).and_then(|at| tokens.get(at)) else { return false };
    match previous {
        Token::Word(_) => {
            let grouping =
                ["SELECT", "DISTINCT", "ON", "WHERE", "AND", "OR", "NOT", "IN", "WHEN", "THEN", "ELSE", "BY"];
            let builtins = ["COUNT", "COUNT_BIG", "SUM", "AVG", "MIN", "MAX", "ABS", "ISNULL", "COALESCE", "NULLIF"];
            !at.checked_sub(2).is_some_and(|at| tokens[at] == Token::Punct('.'))
                && grouping.into_iter().chain(builtins).any(|name| word(Some(previous), name))
        }
        Token::Punct('(' | ',' | '=' | '<' | '>' | '!' | '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^') => true,
        _ => false,
    }
}
