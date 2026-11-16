//! Native EOF counters belong to the exact full TableScan operator and its original worker population.
use super::children;
use culprit_core::plan_facts::{MAX_PLAN_SCAN_THREADS, PlanScanCompletion, PlanScanThread};
use roxmltree::Node;

pub(super) fn read(relop: Node<'_, '_>) -> Option<PlanScanCompletion> {
    let parallel = match relop.attribute("Parallel") {
        Some("0" | "false") => Some(false),
        Some("1" | "true") => Some(true),
        None => None,
        _ => return None,
    };
    let mut populations = children(relop, "RunTimeInformation");
    let population = populations.next()?;
    if populations.next().is_some() {
        return None;
    }
    let mut threads = Vec::new();
    for native in children(population, "RunTimeCountersPerThread") {
        if threads.len() == MAX_PLAN_SCAN_THREADS {
            return None;
        }
        let thread = native.attribute("Thread")?.parse::<u32>().ok()?;
        threads.push(PlanScanThread {
            thread,
            executions: counter(native, "ActualExecutions")?,
            end_of_scans: counter(native, "ActualEndOfScans")?,
            rebinds: counter(native, "ActualRebinds")?,
            rewinds: counter(native, "ActualRewinds")?,
        });
    }
    let completion = PlanScanCompletion { parallel, threads };
    completion.is_valid().then_some(completion)
}

fn counter(node: Node<'_, '_>, name: &str) -> Option<Option<String>> {
    match node.attribute(name) {
        Some(value) => value.parse::<u64>().ok().map(|count| Some(count.to_string())),
        None => Some(None),
    }
}

#[cfg(test)]
mod tests;
