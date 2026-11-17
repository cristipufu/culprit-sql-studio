//! Preserve the four native page-server fields through the one existing per-thread runtime walk.
//! No request counters, native timestamps or later source context are substituted for this XML population.
use culprit_core::actual::page_server_io::{PlanPageServerIo, PlanPageServerIoBuilder, PlanPageServerThread};
use roxmltree::Node;

pub(super) fn observe(builder: &mut PlanPageServerIoBuilder, thread: Node<'_, '_>) {
    builder.observe(PlanPageServerThread {
        thread: thread.attribute("Thread"),
        reads: thread.attribute("ActualPageServerReads"),
        read_aheads: thread.attribute("ActualPageServerReadAheads"),
        lob_reads: thread.attribute("ActualLobPageServerReads"),
        lob_read_aheads: thread.attribute("ActualLobPageServerReadAheads"),
    });
}

pub(super) fn finish(mut builder: PlanPageServerIoBuilder, population: Node<'_, '_>) -> Option<Box<PlanPageServerIo>> {
    if population.parent().is_some_and(|relop| super::children(relop, "RunTimeInformation").take(2).count() != 1) {
        builder.mark_multiple_populations();
    }
    builder.finish().map(Box::new)
}

#[cfg(test)]
mod tests;
