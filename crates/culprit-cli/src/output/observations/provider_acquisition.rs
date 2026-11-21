use culprit_core::{ProviderAcquisitionReport, ProviderSetupAccounting, format, t};
const READER_LIMIT: usize = 5;
pub(crate) fn provider_acquisition(report: &ProviderAcquisitionReport) -> String {
    let ProviderAcquisitionReport::Available {
        evidence,
        title,
        summary,
        settlement,
        coverage,
        total_admissions,
        readers,
        limitations,
    } = report
    else {
        let ProviderAcquisitionReport::Unavailable { reason } = report else { unreachable!() };
        return reason.clone();
    };
    let mut text = format!("{title}\n{summary}\n{settlement}\n{coverage}");
    text.push('\n');
    text.push_str(&t!(
        "view.source_overhead.cli.bounds",
        from = &evidence.client_started_at,
        until = &evidence.client_finished_at
    ));
    if let Some(total) = total_admissions {
        text.push('\n');
        text.push_str(&t!("view.provider_acquisition.total", count = format::digits(total.clone())));
    }
    for explained in readers.iter().take(READER_LIMIT) {
        let Some(reader) = evidence.readers.iter().find(|reader| reader.reader_id == explained.reader_id) else {
            continue;
        };
        if let Some(queries) = reader.queries {
            text.push_str("\n\n");
            text.push_str(&t!(
                "view.provider_acquisition.reader",
                reader = format::digits(reader.reader_id.to_string()),
                count = format::digits(queries.to_string())
            ));
        }
        text.push('\n');
        text.push_str(&explained.work.summary);
        super::source_overhead::append_work(
            &mut text,
            reader.work.as_deref(),
            &explained.work.template_order,
            explained.work.connection_work.as_ref(),
            explained.work.native_interval.as_ref(),
        );
        if reader.setup_accounting == ProviderSetupAccounting::Separate {
            if let Some(count) = reader.setup_queries {
                text.push('\n');
                text.push_str(&t!("view.provider_acquisition.setup", count = format::digits(count.to_string())));
            }
            if let Some(setup) = &explained.setup_work {
                super::source_overhead::append_work(
                    &mut text,
                    reader.setup_work.as_deref(),
                    &setup.template_order,
                    setup.connection_work.as_ref(),
                    setup.native_interval.as_ref(),
                );
            }
        }
    }
    if readers.len() > READER_LIMIT {
        text.push('\n');
        text.push_str(&t!(
            "view.provider_acquisition.presentation",
            shown = format::digits(READER_LIMIT.to_string()),
            recorded = format::digits(readers.len().to_string())
        ));
    }
    for limitation in limitations {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}

pub(crate) fn provider_acquisition_brief(report: &ProviderAcquisitionReport) -> String {
    match report {
        ProviderAcquisitionReport::Unavailable { reason } => reason.clone(),
        ProviderAcquisitionReport::Available { title, summary, settlement, coverage, limitations, .. } => {
            [title.as_str(), summary.as_str(), settlement.as_str(), coverage.as_str()]
                .into_iter()
                .chain(limitations.iter().take(1).map(String::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

#[cfg(test)]
mod tests;
