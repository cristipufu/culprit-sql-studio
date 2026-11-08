//! Catalog references retained with observations. Store arguments before formatting, then render for the reader.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::Arg;

pub(crate) fn render_text(text: &mut String, message: Option<&Message>) {
    if let Some(message) = message {
        *text = message.render();
    }
}

pub(crate) fn render_notes(notes: &mut Vec<String>, messages: Option<&[Message]>) {
    if let Some(messages) = messages {
        *notes = messages.iter().map(Message::render).collect();
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum Message {
    /// Database text, identifiers, SQL, or user-authored text.
    Text {
        text: String,
    },
    Catalog {
        key: String,
        arguments: BTreeMap<String, MessageValue>,
    },
    /// Whole messages forming a paragraph, or a list of definitions separated by punctuation.
    Sequence {
        messages: Vec<Message>,
        separator: String,
    },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum MessageValue {
    Text(String),
    Number(f64),
    Count(f64),
    Duration(f64),
    Bytes(f64),
    Percent(f64),
    /// A decimal wire representation whose precision is retained, without grouping (format::digits).
    Digits(String),
    DateSpan {
        from: String,
        to: String,
    },
    Message(Box<Message>),
}

impl Message {
    pub fn is_translatable(&self) -> bool {
        match self {
            Self::Text { .. } => false,
            Self::Catalog { .. } => true,
            Self::Sequence { messages, .. } => messages.iter().any(Self::is_translatable),
        }
    }

    pub fn catalog(key: &str, arguments: &[(&str, MessageValue)]) -> Self {
        Self::Catalog {
            key: key.into(),
            arguments: arguments.iter().map(|(key, value)| (key.to_string(), value.clone())).collect(),
        }
    }

    pub fn sequence(messages: Vec<Message>, separator: &str) -> Self {
        Self::Sequence { messages, separator: separator.into() }
    }

    pub fn paragraph(messages: Vec<Message>) -> Self {
        Self::sequence(messages, " ")
    }

    pub fn list(messages: Vec<Message>) -> Self {
        Self::joined_list(&messages, crate::key!("common.list.pair"))
    }

    pub(crate) fn joined_list(messages: &[Message], pair: &str) -> Self {
        let Some((last, rest)) = messages.split_last() else { return Self::from("") };
        let Some((before, rest)) = rest.split_last() else { return last.clone() };
        let tail = Self::catalog(pair, &[("first", before.into()), ("last", last.into())]);
        rest.iter().rev().fold(tail, |tail, first| crate::message!("common.list.more", first = first, rest = tail))
    }

    pub fn render(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::Catalog { key, arguments } => {
                let arguments: Vec<_> = arguments.iter().map(|(key, value)| (key.as_str(), value.render())).collect();
                super::t(key, &arguments)
            }
            Self::Sequence { messages, separator } => {
                messages.iter().map(Self::render).collect::<Vec<_>>().join(separator)
            }
        }
    }
}

impl MessageValue {
    fn render(&self) -> Arg {
        match self {
            Self::Text(value) => value.into(),
            Self::Number(value) => (*value).into(),
            Self::Count(value) => crate::format::count(*value).into(),
            Self::Duration(value) => crate::format::duration(*value).into(),
            Self::Bytes(value) => crate::format::bytes(*value).into(),
            Self::Percent(value) => crate::format::percent(*value).into(),
            Self::Digits(value) => crate::format::digits(value.clone()).into(),
            Self::DateSpan { from, to } => crate::span_when(from, to).into(),
            Self::Message(message) => message.render().into(),
        }
    }
}

impl From<String> for Message {
    fn from(text: String) -> Self {
        Self::Text { text }
    }
}
impl From<&str> for Message {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}
impl From<&String> for Message {
    fn from(text: &String) -> Self {
        text.clone().into()
    }
}
impl From<&Message> for Message {
    fn from(message: &Message) -> Self {
        message.clone()
    }
}
impl From<String> for MessageValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for MessageValue {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
impl From<&String> for MessageValue {
    fn from(value: &String) -> Self {
        Self::Text(value.clone())
    }
}
impl From<Message> for MessageValue {
    fn from(value: Message) -> Self {
        Self::Message(Box::new(value))
    }
}
impl From<&Message> for MessageValue {
    fn from(value: &Message) -> Self {
        value.clone().into()
    }
}
macro_rules! number_value {
    ($($ty:ty),*) => { $(impl From<$ty> for MessageValue {
        fn from(value: $ty) -> Self { Self::Number(value as f64) }
    })* };
}
number_value!(f64, f32, u8, u16, u32, u64, usize, i32, i64);

/// Record a catalog key and its unformatted arguments instead of recording the current language's words.
#[macro_export]
macro_rules! message {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::Message::catalog($key, &[$((stringify!($name), ($value).into())),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_messages_format_nested_arguments_for_each_reader() {
        let message = crate::message!("engine.query_log.statements", n = 1234, count = MessageValue::Count(1234.0));
        let stored = super::super::scope("en", || serde_json::to_string(&message).unwrap());
        assert!(!stored.contains("statements read"));
        let read: Message = serde_json::from_str(&stored).unwrap();
        let en = super::super::scope("en", || read.render());
        let fr = super::super::scope("fr", || read.render());
        assert!(en.contains("1.2K"), "{en}");
        assert!(fr.contains("1,2K"), "{fr}");
        assert_ne!(en, fr);
    }
}
