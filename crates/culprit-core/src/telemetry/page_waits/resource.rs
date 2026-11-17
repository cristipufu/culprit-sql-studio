//! SQL Server's page coordinates remain independent of its mutable diagnostic latch fields.
use super::*;

const MAX_DECIMAL_DIGITS: usize = 10;
const MAX_DIAGNOSTIC_COUNTER_DIGITS: usize = 20;
const POINTER_HEX_DIGITS: usize = 16;

impl PageWaitAddress {
    /// Only the coordinates identify the physical page within its separately qualified native source.
    /// Diagnostic task addresses and flags are syntax, never holder, generation or absence evidence.
    pub fn parse_sql_server_resource(native: &str) -> Option<Self> {
        if !valid_key(native) || !native.is_ascii() {
            return None;
        }
        let coordinates = match native.split_once(" (LATCH ") {
            Some((coordinates, diagnostic)) => {
                if !valid_latch_diagnostic(diagnostic) {
                    return None;
                }
                coordinates
            }
            None => native,
        };
        let mut parts = coordinates.split(':');
        let database_id = decimal(parts.next()?, i16::MAX as u32).filter(|value| *value > 0)?;
        let file_id = decimal(parts.next()?, i32::MAX as u32).filter(|value| *value > 0)?;
        let page_id = decimal(parts.next()?, i32::MAX as u32)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { database_id, file_id, page_id })
    }
}

impl ObservedWaitResource {
    /// Physical coordinates only. Native qualifier equality preserves provenance; callers still own
    /// source/member/epoch, request and point-clock qualification. Unsupported text never falls back to equality.
    pub fn same_sql_server_page(&self, other: &Self) -> bool {
        self.is_valid()
            && other.is_valid()
            && matches!(self.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo)
            && self.kind == other.kind
            && self.native.qualifiers == other.native.qualifiers
            && PageWaitAddress::parse_sql_server_resource(&self.native.id)
                .zip(PageWaitAddress::parse_sql_server_resource(&other.native.id))
                .is_some_and(|(left, right)| left == right)
    }
}

fn decimal(value: &str, maximum: u32) -> Option<u32> {
    if value.is_empty() || value.len() > MAX_DECIMAL_DIGITS || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|value| *value <= maximum)
}

fn valid_latch_diagnostic(value: &str) -> bool {
    let Some(value) = value.strip_suffix(')') else { return false };
    let Some((address, fields)) = value.split_once(": Class: BUFFER ") else { return false };
    if !valid_pointer(address) {
        return false;
    }
    let mut fields = fields.split(' ');
    for label in ["KP:", "SH:", "UP:", "EX:", "DT:", "Sublatch:", "HasWaiters:"] {
        if fields.next() != Some(label) || !fields.next().is_some_and(valid_diagnostic_counter) {
            return false;
        }
    }
    fields.next() == Some("Task:")
        && fields.next().is_some_and(valid_pointer)
        && fields.next() == Some("AnyReleasor:")
        && fields.next().is_some_and(valid_diagnostic_counter)
        && fields.next().is_none()
}

fn valid_pointer(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|value| value.len() == POINTER_HEX_DIGITS && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn valid_diagnostic_counter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DIAGNOSTIC_COUNTER_DIGITS
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

#[cfg(test)]
mod tests;
