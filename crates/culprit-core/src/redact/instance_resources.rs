use super::*;
use crate::telemetry::{InstanceResourceRead, ObservationReadContext};

pub(super) fn redact(mut read: Box<InstanceResourceRead>) -> crate::Result<Box<InstanceResourceRead>> {
    if !read.is_valid() {
        return Err(crate::telemetry::CollectionStop::InvalidPage.error());
    }
    if let ObservationReadContext::Native { context } = &mut read.context {
        redact_read_context(context);
    }
    read.is_valid().then_some(read).ok_or_else(|| crate::telemetry::CollectionStop::InvalidPage.error())
}
