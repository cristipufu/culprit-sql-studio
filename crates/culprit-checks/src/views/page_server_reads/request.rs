//! One source-qualified request counter is not an execution average or current statement attribution.
use super::*;

pub fn explain_request(pages: &str) -> Result<PageServerReadExplanation> {
    if PageServerReadStatistics::native_count(pages).is_none() {
        return Err(invalid());
    }
    let pages = format::digits(pages.to_owned());
    Ok(PageServerReadExplanation {
        availability: PageServerReadAvailability::Available,
        title: t!("view.page_server_reads.request.title"),
        summary: t!("view.page_server_reads.request.summary", pages = pages.clone()),
        metrics: vec![Metric::said_text(key!("view.page_server_reads.request.count"), pages)],
        limitations: vec![
            t!("view.page_server_reads.request.limit.lifetime"),
            t!("view.page_server_reads.request.limit.measurement"),
            t!("view.page_server_reads.request.limit.point"),
        ],
        investigate: t!("view.page_server_reads.request.investigate"),
        verify: t!("view.page_server_reads.request.verify"),
    })
}

#[cfg(test)]
mod tests;
