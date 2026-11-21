//! Sharing masks unrecognized native labels without changing exact measurements or source/read qualification.
use super::{opaque, redact_read_context};
use crate::{Engine, ResetMarker, Result, telemetry::*};

pub(super) fn redact(mut read: Box<WaitCounterSnapshot>, engine: Engine) -> Result<Box<WaitCounterSnapshot>> {
    if !read.is_valid_for(engine) {
        return Err(CollectionStop::InvalidPage.error());
    }
    if read.pool_current.is_some() {
        return Err(CollectionStop::InvalidPage.error());
    }
    if let Some(ObservationReadContext::Native { context }) = &mut read.context {
        redact_read_context(context);
    }
    for source in &mut read.sources {
        source.source = source_name(&source.source);
    }
    for row in &mut read.observations {
        row.source = source_name(&row.source);
        // Only a positively mapped native name establishes public schema semantics.
        if engine.spec().native_wait_mechanism(&row.wait_type).is_none() {
            row.wait_type = opaque(&row.wait_type);
        }
        if let ResetMarker::Generation(generation) = &mut row.reset {
            *generation = opaque(generation);
        }
    }
    read.is_valid_for(engine).then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}

pub(super) fn source_name(source: &str) -> String {
    if source == crate::source_id::WAITS { source.into() } else { opaque(source) }
}

#[cfg(test)]
mod tests;
