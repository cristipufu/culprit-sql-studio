//! Sharing preserves valid counter relationships; it never repairs malformed native evidence.
use super::{opaque, redact_read_context, redact_resource_identity};
use crate::{Result, telemetry::*};

pub(super) fn redact(mut read: Box<StatementCounterSnapshot>) -> Result<Box<StatementCounterSnapshot>> {
    if !read.is_valid_read(false) {
        return Err(CollectionStop::InvalidPage.error());
    }
    if let Some(context) = &mut read.context {
        redact_read_context(context);
    }
    for row in &mut read.observations {
        let OperationalFact::StatementCounters(sample) = &mut row.fact else {
            return Err(CollectionStop::InvalidPage.error());
        };
        if let Some(member) = &mut sample.member {
            redact_resource_identity(member);
        }
        sample.identity.query_id = opaque(&sample.identity.query_id);
        for value in sample.identity.qualifiers.values_mut() {
            *value = opaque(value);
        }
        row.key = sample.key()?;
    }
    read.is_valid_read(false).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}

#[cfg(test)]
mod tests;
