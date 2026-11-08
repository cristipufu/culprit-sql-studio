//! The message syntax, as the app implements it too (`app/src/lib/message-format.ts`): a subset of ICU MessageFormat.

use std::collections::BTreeMap;

use super::Lang;

/// An argument a message is given: text as it is, or a number (for `plural`, or written in the language's digits).
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Text(String),
    Number(f64),
}

/// A catalog owns the complete label in either position, including its spelling and word order.
#[derive(Clone, Copy)]
pub enum LabelPosition {
    Inline,
    Initial,
}

impl From<LabelPosition> for Arg {
    fn from(position: LabelPosition) -> Self {
        match position {
            LabelPosition::Inline => "inline".into(),
            LabelPosition::Initial => "initial".into(),
        }
    }
}

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Arg::Text(value.to_string())
    }
}
impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::Text(value)
    }
}
impl From<&String> for Arg {
    fn from(value: &String) -> Self {
        Arg::Text(value.clone())
    }
}
impl From<std::borrow::Cow<'_, str>> for Arg {
    fn from(value: std::borrow::Cow<'_, str>) -> Self {
        Arg::Text(value.into_owned())
    }
}
macro_rules! number_arg {
    ($($ty:ty),*) => { $(impl From<$ty> for Arg { fn from(value: $ty) -> Self { Arg::Number(value as f64) } })* };
}
number_arg!(f64, f32, u8, u16, u32, u64, usize, i32, i64);

#[derive(Debug, PartialEq)]
pub(crate) enum Node {
    Text(String),
    Arg(String),
    Pound,
    Plural(String, BTreeMap<String, Vec<Node>>),
    Select(String, BTreeMap<String, Vec<Node>>),
}

/// `message` with `args` in it, written for `lang`. An argument it names but isn't given stays as `{name}`.
pub fn format_message(message: &str, args: &[(&str, Arg)], lang: &Lang) -> String {
    match parse(message) {
        Ok(nodes) => {
            let mut out = String::new();
            render(&nodes, args, lang, None, &mut out);
            out
        }
        // A malformed message can't ship (the catalogs' tests read every one); as a last resort, its raw text.
        Err(_) => message.to_string(),
    }
}

/// A number in `lang`'s digits: grouped thousands, up to two decimals, no trailing zeros.
pub fn format_number(n: f64, lang: &Lang) -> String {
    format_number_with(n, &lang.decimal, &lang.group)
}

pub(crate) fn format_number_with(n: f64, decimal: &str, group: &str) -> String {
    if !n.is_finite() {
        return n.to_string();
    }
    let fixed = format!("{:.2}", n.abs());
    let trimmed = fixed.trim_end_matches('0').trim_end_matches('.');
    let (whole, fraction) = trimmed.split_once('.').unwrap_or((trimmed, ""));
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push_str(group);
        }
        grouped.push(c);
    }
    let sign = if n < 0.0 && trimmed != "0" { "-" } else { "" };
    if fraction.is_empty() { format!("{sign}{grouped}") } else { format!("{sign}{grouped}{decimal}{fraction}") }
}

fn render(nodes: &[Node], args: &[(&str, Arg)], lang: &Lang, count: Option<f64>, out: &mut String) {
    let arg = |name: &str| args.iter().find(|(n, _)| *n == name).map(|(_, a)| a);
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Pound => match count {
                Some(n) => out.push_str(&format_number(n, lang)),
                None => out.push('#'),
            },
            Node::Arg(name) => match arg(name) {
                Some(Arg::Text(text)) => out.push_str(text),
                Some(Arg::Number(n)) => out.push_str(&format_number(*n, lang)),
                None => out.push_str(&format!("{{{name}}}")),
            },
            Node::Plural(name, cases) => {
                let n = match arg(name) {
                    Some(Arg::Number(n)) => *n,
                    Some(Arg::Text(text)) => text.parse().unwrap_or(f64::NAN),
                    None => f64::NAN,
                };
                let exact = cases.get(&format!("={n}"));
                let chosen = exact.or_else(|| cases.get(lang.plural.category(n))).or_else(|| cases.get("other"));
                if let Some(chosen) = chosen {
                    render(chosen, args, lang, Some(n), out);
                }
            }
            Node::Select(name, cases) => {
                let value = match arg(name) {
                    Some(Arg::Text(text)) => text.clone(),
                    Some(Arg::Number(n)) => format_number_with(*n, ".", ""),
                    None => String::new(),
                };
                if let Some(chosen) = cases.get(&value).or_else(|| cases.get("other")) {
                    render(chosen, args, lang, count, out);
                }
            }
        }
    }
}

/// What `message` asks for: each argument's name and how it's used (`text`, `plural` or `select`).
pub fn argument_kinds(message: &str) -> Result<BTreeMap<String, &'static str>, String> {
    fn walk(nodes: &[Node], out: &mut BTreeMap<String, &'static str>) {
        for node in nodes {
            match node {
                Node::Arg(name) => {
                    out.entry(name.clone()).or_insert("text");
                }
                Node::Plural(name, cases) => {
                    out.insert(name.clone(), "plural");
                    cases.values().for_each(|c| walk(c, out));
                }
                Node::Select(name, cases) => {
                    out.insert(name.clone(), "select");
                    cases.values().for_each(|c| walk(c, out));
                }
                Node::Text(_) | Node::Pound => {}
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&parse(message)?, &mut out);
    Ok(out)
}

/// Parse a message, or say where it is malformed.
pub(crate) fn parse(message: &str) -> Result<Vec<Node>, String> {
    let chars: Vec<char> = message.chars().collect();
    let mut i = 0;
    let nodes = parse_nodes(&chars, &mut i, false, false, message)?;
    if i < chars.len() {
        return Err(format!("unexpected }} at {i} in: {message}"));
    }
    Ok(nodes)
}

fn parse_nodes(
    chars: &[char],
    i: &mut usize,
    in_plural: bool,
    nested: bool,
    message: &str,
) -> Result<Vec<Node>, String> {
    let fail = |why: &str, at: usize| Err(format!("{why} at {at} in: {message}"));
    let mut out = Vec::new();
    let mut text = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == '}' {
            if !nested {
                return fail("unexpected }", *i);
            }
            break;
        }
        if c == '#' && in_plural {
            if !text.is_empty() {
                out.push(Node::Text(std::mem::take(&mut text)));
            }
            out.push(Node::Pound);
            *i += 1;
            continue;
        }
        if c != '{' {
            text.push(c);
            *i += 1;
            continue;
        }
        if !text.is_empty() {
            out.push(Node::Text(std::mem::take(&mut text)));
        }
        *i += 1;
        let name = word(chars, i, message)?;
        skip_space(chars, i);
        match chars.get(*i) {
            Some('}') => {
                *i += 1;
                out.push(Node::Arg(name));
                continue;
            }
            Some(',') => *i += 1,
            _ => return fail("expected , or }", *i),
        }
        let kind = word(chars, i, message)?;
        if kind != "plural" && kind != "select" {
            return fail(&format!("unknown argument type {kind}"), *i);
        }
        skip_space(chars, i);
        if chars.get(*i) != Some(&',') {
            return fail("expected ,", *i);
        }
        *i += 1;
        let mut cases = BTreeMap::new();
        loop {
            skip_space(chars, i);
            match chars.get(*i) {
                Some('}') => {
                    *i += 1;
                    break;
                }
                None => return fail("unclosed argument", *i),
                _ => {}
            }
            let key = word(chars, i, message)?;
            skip_space(chars, i);
            if chars.get(*i) != Some(&'{') {
                return fail("expected { after a case", *i);
            }
            *i += 1;
            let case = parse_nodes(chars, i, kind == "plural", true, message)?;
            if chars.get(*i) != Some(&'}') {
                return fail("unclosed case", *i);
            }
            *i += 1;
            cases.insert(key, case);
        }
        if !cases.contains_key("other") {
            return fail(&format!("{kind} needs an other case"), *i);
        }
        out.push(if kind == "plural" { Node::Plural(name, cases) } else { Node::Select(name, cases) });
    }
    if !text.is_empty() {
        out.push(Node::Text(text));
    }
    Ok(out)
}

fn skip_space(chars: &[char], i: &mut usize) {
    while chars.get(*i).is_some_and(|c| c.is_whitespace()) {
        *i += 1;
    }
}

fn word(chars: &[char], i: &mut usize, message: &str) -> Result<String, String> {
    skip_space(chars, i);
    let start = *i;
    while chars.get(*i).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '=' | '.' | '-')) {
        *i += 1;
    }
    if start == *i {
        return Err(format!("expected a name at {start} in: {message}"));
    }
    Ok(chars[start..*i].iter().collect())
}
