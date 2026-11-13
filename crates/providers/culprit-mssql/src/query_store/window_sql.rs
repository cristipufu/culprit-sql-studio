//! One bounded native interval population supplies runtime, waits and displayed scan bounds.
use crate::{
    read::{Parameter, integer, invalid_data, optional_nonnegative_number, text},
    sql_time::utc_offset_precise_iso,
    value::Rec,
};
use culprit_core::{Result, telemetry::WaitCaptureState};
use std::collections::BTreeMap;

pub(crate) const MAX_INTERVALS: usize = 4096;
const MAX_INTERVAL_XML_BYTES: usize = MAX_INTERVALS * 256;
pub(crate) const DECLARATION: &str = "DECLARE /* culprit */ @culprit_intervals xml=CONVERT(xml,@P1);\n";
pub(crate) const MEMBERSHIP: &str = "EXISTS(SELECT 1 FROM @culprit_intervals.nodes('/intervals/interval') n(x) WHERE i.runtime_stats_interval_id=x.value('@id[1]','bigint'))";
const CAPPED_END: &str = "CASE WHEN b.ends>@at THEN @at ELSE b.ends END";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowVerification {
    Unchanged,
    WaitCaptureChanged,
    RuntimePopulationChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Interval {
    starts: String,
    ends: String,
}

#[derive(Clone)]
pub(crate) struct FrozenWindow {
    minutes: u32,
    clock: String,
    native_xml: String,
    intervals: BTreeMap<i64, Interval>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub hours: f64,
    pub capture: WaitCaptureState,
    waits_supported: bool,
    pub retained: bool,
}
impl FrozenWindow {
    pub(crate) fn decode(row: &Rec, minutes: u32, waits_supported: bool) -> Result<Self> {
        let clock = text(row, "window_clock")?;
        parse_time(&clock)?;
        let native_xml = text(row, "native_intervals")?;
        if native_xml.len() > MAX_INTERVAL_XML_BYTES {
            return Err(culprit_core::telemetry::CollectionStop::RowLimit.error());
        }
        let intervals = culprit_core::xml::read(&native_xml, |document| {
            let document = document.map_err(|_| invalid_data())?;
            if document.root_element().tag_name().name() != "intervals" {
                return Err(invalid_data());
            }
            let mut intervals = BTreeMap::new();
            for node in document.root_element().children().filter(|node| node.is_element()) {
                if node.tag_name().name() != "interval" {
                    return Err(invalid_data());
                }
                let id = node
                    .attribute("id")
                    .ok_or_else(invalid_data)?
                    .parse::<i64>()
                    .ok()
                    .filter(|id| *id >= 0)
                    .ok_or_else(invalid_data)?;
                let starts = node.attribute("starts").ok_or_else(invalid_data)?;
                let ends = node.attribute("ends").ok_or_else(invalid_data)?;
                if parse_time(starts)? >= parse_time(ends)? || parse_time(starts)? > parse_time(&clock)? {
                    return Err(invalid_data());
                }
                if intervals.insert(id, Interval { starts: starts.into(), ends: ends.into() }).is_some() {
                    return Err(invalid_data());
                }
                if intervals.len() > MAX_INTERVALS {
                    return Err(culprit_core::telemetry::CollectionStop::RowLimit.error());
                }
            }
            Ok(intervals)
        })?;
        let mut periods = intervals
            .values()
            .map(|interval| Ok((parse_time(&interval.starts)?, parse_time(&interval.ends)?)))
            .collect::<Result<Vec<_>>>()?;
        periods.sort_unstable();
        if periods.windows(2).any(|pair| pair[0].1 != pair[1].0) {
            return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
        }
        let start = row.str("window_start");
        let end = row.str("window_end");
        let hours = optional_nonnegative_number(row, "window_hours")?;
        match (&start, &end, hours) {
            (Some(start), Some(end), Some(hours)) if !intervals.is_empty() => {
                let expected_start = periods.first().ok_or_else(invalid_data)?.0;
                let expected_end = periods.last().ok_or_else(invalid_data)?.1.min(parse_time(&clock)?);
                if parse_time(start)? != expected_start || parse_time(end)? != expected_end {
                    return Err(invalid_data());
                }
                let elapsed = parse_time(end)?
                    .signed_duration_since(parse_time(start)?)
                    .num_nanoseconds()
                    .ok_or_else(invalid_data)?;
                if elapsed < 0 || (hours - elapsed as f64 / 3_600_000_000_000.0).abs() > 1e-10 {
                    return Err(invalid_data());
                }
            }
            (None, None, None) if intervals.is_empty() => (),
            _ => return Err(invalid_data()),
        }
        let capture = if waits_supported { super::waits::capture_state(row)? } else { WaitCaptureState::Unavailable };
        let retained = match integer(row, "intervals_retained")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        };
        Ok(Self {
            minutes,
            clock,
            native_xml,
            intervals,
            start,
            end,
            hours: hours.unwrap_or(0.0),
            capture,
            waits_supported,
            retained,
        })
    }
    pub(crate) fn matches_request(&self, minutes: u32) -> bool {
        self.minutes == minutes
    }
    pub(crate) fn parameter(&self) -> Parameter {
        Parameter::Text(self.native_xml.clone())
    }
    pub(crate) fn verify_query(&self) -> (String, Vec<Parameter>) {
        (metadata_sql(self.waits_supported, true), vec![self.parameter()])
    }
    pub(crate) fn verify(&mut self, row: &Rec) -> Result<WindowVerification> {
        let current = Self::decode(row, self.minutes, self.waits_supported)?;
        if self.intervals != current.intervals || parse_time(&current.clock)? < parse_time(&self.clock)? {
            self.invalidate_endpoint();
            return Ok(WindowVerification::RuntimePopulationChanged);
        }
        // Membership stays fixed while mutable open-interval counters can grow. The final native clock bounds
        // later-observed rows; it does not turn separately read runtime and waits into an atomic snapshot.
        let capture_changed = self.capture != current.capture;
        self.clock = current.clock;
        self.start = current.start;
        self.end = current.end;
        self.hours = current.hours;
        self.retained = current.retained;
        self.capture = current.capture;
        Ok(if !current.retained {
            self.invalidate_endpoint();
            WindowVerification::RuntimePopulationChanged
        } else if capture_changed {
            WindowVerification::WaitCaptureChanged
        } else {
            WindowVerification::Unchanged
        })
    }
    pub(crate) fn invalidate_endpoint(&mut self) {
        self.end = None;
        self.hours = 0.0;
        self.retained = false;
    }
    #[cfg(test)]
    pub(crate) fn interval_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.intervals.keys().copied()
    }
}

fn parse_time(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).map_err(|_| invalid_data())
}

pub(crate) fn metadata_sql(waits_supported: bool, verifying: bool) -> String {
    let (declaration, selection, bounds) = if verifying {
        (
            DECLARATION,
            "EXISTS(SELECT 1 FROM @culprit_intervals.nodes('/intervals/interval') n(x) WHERE i.runtime_stats_interval_id=x.value('@id[1]','bigint') AND i.start_time=CONVERT(datetimeoffset,x.value('@starts[1]','varchar(40)'),127) AND i.end_time=CONVERT(datetimeoffset,x.value('@ends[1]','varchar(40)'),127))".to_owned(),
            "@culprit_intervals",
        )
    } else {
        ("", "i.end_time>=DATEADD(minute,-@P1,@at) AND i.start_time<=@at".to_owned(), "@selected")
    };
    let capture = if waits_supported { "o.wait_stats_capture_mode_desc" } else { "CAST(NULL AS nvarchar(60))" };
    let start = utc_offset_precise_iso("b.starts");
    let end = utc_offset_precise_iso(CAPPED_END);
    let limit = MAX_INTERVALS + 1;
    format!(
        r#"{declaration}
DECLARE /* culprit */ @at datetimeoffset=SYSDATETIMEOFFSET();
DECLARE /* culprit */ @selected xml=(SELECT TOP({limit}) i.runtime_stats_interval_id AS id,
 CONVERT(varchar(40),i.start_time,127) AS starts,CONVERT(varchar(40),i.end_time,127) AS ends
 FROM sys.query_store_runtime_stats_interval i WHERE {selection}
 FOR XML RAW('interval'),ROOT('intervals'));
SELECT /* culprit */ CONVERT(varchar(40),@at,127) AS window_clock,
 CONVERT(nvarchar(max),{bounds}) AS native_intervals,
 CASE WHEN @selected.value('count(/intervals/interval)','int')={bounds}.value('count(/intervals/interval)','int') THEN 1 ELSE 0 END AS intervals_retained,
 {start} AS window_start,{end} AS window_end,
 DATEDIFF_BIG(nanosecond,b.starts,{CAPPED_END})/CAST(3600000000000 AS float) AS window_hours,
 {capture} AS wait_stats_capture_mode_desc
FROM sys.database_query_store_options o
CROSS JOIN (SELECT MIN(CONVERT(datetimeoffset,x.value('@starts[1]','varchar(40)'),127)) AS starts,
 MAX(CONVERT(datetimeoffset,x.value('@ends[1]','varchar(40)'),127)) AS ends
 FROM {bounds}.nodes('/intervals/interval') n(x)) b OPTION(MAXDOP 1,LOOP JOIN)"#
    )
}

pub(crate) fn waits_supported(row: &Rec) -> Result<bool> {
    match integer(row, "waits")? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_data()),
    }
}

#[cfg(test)]
pub(crate) mod tests;
