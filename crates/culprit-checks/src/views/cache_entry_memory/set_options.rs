//! sys.dm_exec_plan_attributes flags describe compiled context; they are not @@OPTIONS or compilation counters.
use super::{MssqlCacheEntryMemory, format, known_keys, same_native_range, same_native_statement, t};

// Only the documented boolean SET flags may become ON/OFF advice. Parallel, internal, parameterization,
// date and language flags keep their native values without acquiring session-setting semantics.
const BOOLEAN_SET_OPTIONS: &[(u32, &str)] = &[
    (1, "ANSI_PADDING"),
    (4, "FORCEPLAN"),
    (8, "CONCAT_NULL_YIELDS_NULL"),
    (16, "ANSI_WARNINGS"),
    (32, "ANSI_NULLS"),
    (64, "QUOTED_IDENTIFIER"),
    (128, "ANSI_NULL_DFLT_ON"),
    (256, "ANSI_NULL_DFLT_OFF"),
    (4096, "ARITH_ABORT"),
    (8192, "NUMERIC_ROUNDABORT"),
];

struct ChangedOption {
    native_name: &'static str,
    left_enabled: bool,
}

pub(super) struct Difference<'a> {
    left: &'a MssqlCacheEntryMemory,
    right: &'a MssqlCacheEntryMemory,
    options: Vec<ChangedOption>,
}

pub(super) fn select<'a>(left: &'a MssqlCacheEntryMemory, right: &'a MssqlCacheEntryMemory) -> Option<Difference<'a>> {
    if !same_native_statement(left, right)
        || !same_original_statement(left, right)
        || left.cache_object_type != "Compiled Plan"
        || right.cache_object_type != "Compiled Plan"
        || left.object_type != right.object_type
        || !only_set_options_differ(left, right)
    {
        return None;
    }
    let left_mask = mask(left)?;
    let right_mask = mask(right)?;
    let changed = left_mask ^ right_mask;
    let supported = BOOLEAN_SET_OPTIONS.iter().fold(0, |mask, (flag, _)| mask | flag);
    if changed == 0 || changed & !supported != 0 {
        return None;
    }
    let options = BOOLEAN_SET_OPTIONS
        .iter()
        .filter(|(flag, _)| changed & flag != 0)
        .map(|(flag, name)| ChangedOption { native_name: name, left_enabled: left_mask & flag != 0 })
        .collect();
    Some(Difference { left, right, options })
}

fn same_original_statement(left: &MssqlCacheEntryMemory, right: &MssqlCacheEntryMemory) -> bool {
    left.statements.iter().any(|statement| {
        if !statement.statement.is_available()
            || left.statements.iter().filter(|other| same_native_range(statement, other)).count() != 1
        {
            return false;
        }
        let mut matches = right.statements.iter().filter(|other| same_native_range(statement, other));
        let Some(other) = matches.next() else { return false };
        matches.next().is_none()
            && statement.context.source_text_bytes == other.context.source_text_bytes
            && statement.statement == other.statement
    })
}

fn only_set_options_differ(left: &MssqlCacheEntryMemory, right: &MssqlCacheEntryMemory) -> bool {
    known_keys(left)
        && known_keys(right)
        && left.cache_keys.len() == right.cache_keys.len()
        && left.cache_keys.iter().all(|left| {
            right.cache_keys.iter().find(|right| left.name == right.name).is_some_and(|right| {
                left.native_type == right.native_type && (left.name == "set_options" || left.value == right.value)
            })
        })
}

fn mask(entry: &MssqlCacheEntryMemory) -> Option<u32> {
    let attribute = entry.cache_keys.iter().find(|attribute| attribute.name == "set_options")?;
    if attribute.native_type.as_deref() != Some("int") {
        return None;
    }
    u32::try_from(attribute.value.as_deref()?.parse::<i32>().ok()?).ok()
}

impl Difference<'_> {
    pub(super) fn render(&self) -> (String, String) {
        let options = self
            .options
            .iter()
            .map(|option| {
                if option.left_enabled {
                    t!("view.cache_entry_memory.context.set_option_on_off", option = option.native_name)
                } else {
                    t!("view.cache_entry_memory.context.set_option_off_on", option = option.native_name)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        (
            t!(
                "view.cache_entry_memory.context.set_options",
                options = options,
                left_handle = &self.left.plan_handle,
                right_handle = &self.right.plan_handle,
                left_bytes = format::digits(self.left.size_bytes.clone()),
                right_bytes = format::digits(self.right.size_bytes.clone())
            ),
            t!("view.cache_entry_memory.context.set_options_investigation"),
        )
    }
}
