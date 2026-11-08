//! What Culprit says, in the reader's language. Every message lives in a language catalog, `locales/<code>.json` at the
//! repository root: one file per language, compiled into the library by `build.rs` (a new file is a new language at the
//! next build; nothing is read from disk at run time). English (`en.json`) is the source every other catalog translates;
//! a key a language hasn't translated reads in English.
//!
//! Code asks for a message by key with [`t!`]: `t!("check.heavy_query.title", handle = handle, share = share)`. The
//! language is the one [`scope`] set for the work on this thread (analysis runs on one thread), English otherwise.
//! Messages use a subset of ICU MessageFormat, the same as the app's (`app/src/lib/message-format.ts`), checked by the
//! shared cases in `locales/.tests/messages.json`: `{name}`, `{n, plural, =0 {..} one {# ..} other {..}}` and
//! `{v, select, .. other {..}}`.

mod message;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

#[cfg(test)]
pub(crate) use message::parse;
pub use message::{Arg, PluralRule, argument_kinds, format_message, format_number};

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

/// A language Culprit can speak: its catalog's messages and how it writes numbers.
#[derive(Debug)]
pub struct Lang {
    pub code: String,
    /// Its own name for itself ("Español").
    pub name: String,
    /// Its name in English ("Spanish").
    pub english_name: String,
    pub plural: PluralRule,
    pub decimal: String,
    pub group: String,
    messages: HashMap<String, String>,
}

impl Lang {
    /// The message for `key` in this language, if it has one.
    pub fn message(&self, key: &str) -> Option<&str> {
        self.messages.get(key).map(String::as_str)
    }

    /// Every key this language has a message for.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.messages.keys().map(String::as_str)
    }

    fn parse(code: &str, text: &str) -> Result<Lang, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("{code}.json: {e}"))?;
        let object = value.as_object().ok_or_else(|| format!("{code}.json: not an object"))?;
        let meta = object.get("_meta").and_then(|m| m.as_object()).ok_or_else(|| format!("{code}.json: no _meta"))?;
        let field = |name: &str| {
            meta.get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("{code}.json: _meta.{name}"))
        };
        let plural = match field("plural")?.as_str() {
            "one_is_1" => PluralRule::OneIs1,
            "one_below_2" => PluralRule::OneBelow2,
            other => return Err(format!("{code}.json: unknown plural rule {other}")),
        };
        let mut messages = HashMap::new();
        for (key, message) in object.iter().filter(|(key, _)| key.as_str() != "_meta") {
            let message = message.as_str().ok_or_else(|| format!("{code}.json: {key} is not text"))?;
            messages.insert(key.clone(), message.to_string());
        }
        Ok(Lang {
            code: code.to_string(),
            name: field("name")?,
            english_name: field("english_name")?,
            plural,
            decimal: field("decimal")?,
            group: field("group")?,
            messages,
        })
    }
}

/// Every language the build compiled in, by code. A catalog that doesn't read is left out (its test fails).
fn all() -> &'static HashMap<String, Arc<Lang>> {
    static ALL: OnceLock<HashMap<String, Arc<Lang>>> = OnceLock::new();
    ALL.get_or_init(|| {
        CATALOGS
            .iter()
            .filter_map(|(code, text)| Lang::parse(code, text).ok().map(|lang| (code.to_string(), Arc::new(lang))))
            .collect()
    })
}

/// English: the source of every message, and what any other language falls back to.
pub fn english() -> Arc<Lang> {
    all().get("en").cloned().expect("locales/en.json is compiled in")
}

/// The language with `code`, if Culprit has its catalog.
pub fn language(code: &str) -> Option<Arc<Lang>> {
    all().get(code).cloned()
}

/// Every language Culprit has, English first, then by their own names.
pub fn languages() -> Vec<Arc<Lang>> {
    let mut out: Vec<Arc<Lang>> = all().values().cloned().collect();
    out.sort_by(|a, b| (a.code != "en").cmp(&(b.code != "en")).then_with(|| a.name.cmp(&b.name)));
    out
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Lang>>> = const { RefCell::new(None) };
}

/// The language this process speaks where no [`scope`] says otherwise.
static PROCESS: RwLock<Option<Arc<Lang>>> = RwLock::new(None);

/// Speak `code` in this process from now on, wherever no [`scope`] says otherwise: the desktop app's choice, the CLI's
/// `--lang`. Whether Culprit has that language (it keeps the one before when it doesn't). Output for coding agents
/// (the brief, digests, MCP tools) stays in English by its own scope.
pub fn set_language(code: &str) -> bool {
    let Some(lang) = language(code) else { return false };
    *PROCESS.write().unwrap_or_else(|e| e.into_inner()) = Some(lang);
    true
}

/// The language messages are said in on this thread: the one [`scope`] set, else the process's ([`set_language`]),
/// else English.
pub fn current() -> Arc<Lang> {
    CURRENT
        .with(|c| c.borrow().clone())
        .or_else(|| PROCESS.read().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_else(english)
}

/// Run `f` with its messages in `code` (English for a language Culprit doesn't have), restoring the language before.
pub fn scope<R>(code: &str, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<Lang>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|c| *c.borrow_mut() = previous);
        }
    }
    let lang = language(code).unwrap_or_else(english);
    let _restore = Restore(CURRENT.with(|c| c.borrow_mut().replace(lang)));
    f()
}

/// The message `key` in the current language, with `args` in it. A key it hasn't translated reads in English; one
/// English doesn't have reads as the key itself (the catalogs' tests make sure that never ships).
pub fn t(key: &str, args: &[(&str, Arg)]) -> String {
    let lang = current();
    match lang.message(key) {
        Some(message) => format_message(message, args, &lang),
        None => {
            let english = english();
            match english.message(key) {
                Some(message) => format_message(message, args, &english),
                None => key.to_string(),
            }
        }
    }
}

/// `key!("common.noun.time")`: a message key handed on rather than said here (to [`t`] later, by the code it is given
/// to). It is the key itself; the macro marks it as one, so the catalogs' tests know it is used.
#[macro_export]
macro_rules! key {
    ($key:literal) => {
        $key
    };
}

/// `t!("key", name = value, ...)`: the message `key` in the current language, with each value as its named argument
/// (text as it is; a number for `plural` or in the language's digits).
#[macro_export]
macro_rules! t {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::t($key, &[$((stringify!($name), $crate::i18n::Arg::from($value))),*])
    };
}

#[cfg(test)]
mod tests;
