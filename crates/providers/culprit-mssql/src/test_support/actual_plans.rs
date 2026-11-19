//! Multiple-result actual-plan capture belongs only to owned fixtures, never the production metadata reader.
use super::batch;
use crate::{
    conn::MsClient,
    read::{Parameter, value_bytes},
    value::{Rec, Val},
};
use futures_util::TryStreamExt;
use std::time::Duration;
use tiberius::QueryItem;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(40);
const ATTENTION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CAPTURE_ROWS: usize = 1024;
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CAPTURE_RESULT_SETS: usize = 32;
const MAX_CAPTURE_PLANS: usize = 16;

/// The actor must be owned inside the fixture's unwind/cleanup boundary. Failure panics: no subsequent SQL or
/// reuse is permitted on that actor, even when the driver's attention call returns Ok (not settlement proof).
pub(crate) async fn actual_plans(client: &mut MsClient, sql: &str, parameters: Vec<Parameter>) -> Vec<String> {
    actual_plan_capture(client, sql, parameters).await.plans
}

/// Result rows and native XML belong to the same completely drained owned execution. Neither a second execution
/// nor a later runtime-counter read is needed to verify its result semantics.
pub(crate) struct ActualPlanCapture {
    pub plans: Vec<String>,
    pub results: Vec<Rec>,
}

pub(crate) async fn actual_plan_capture(
    client: &mut MsClient,
    sql: &str,
    parameters: Vec<Parameter>,
) -> ActualPlanCapture {
    capture_response(client, sql, parameters)
        .await
        .finish()
        .expect("owned actual-plan response must retain native XML within its explicit capture bounds")
}

/// Some native metadata statements return no showplan. The response still has to drain within all retention
/// limits; absent XML establishes neither a zero execution grant nor production cost qualification.
pub(super) async fn metadata_plans(
    client: &mut MsClient,
    sql: &str,
    parameters: Vec<Parameter>,
) -> Option<Vec<String>> {
    capture_response(client, sql, parameters)
        .await
        .finish_metadata()
        .expect("owned metadata response exceeded its explicit capture bounds")
}

async fn capture_response(client: &mut MsClient, sql: &str, parameters: Vec<Parameter>) -> Capture {
    batch(client, "SET STATISTICS XML ON").await;
    let response = tokio::time::timeout(CAPTURE_TIMEOUT, async {
        let mut query = tiberius::Query::new(sql);
        for parameter in &parameters {
            match parameter {
                Parameter::Integer(value) => {
                    query.bind(*value);
                }
                Parameter::Text(value) => {
                    query.bind(value.as_str());
                }
            }
        }
        let mut stream = query.query(&mut *client).await?;
        let mut capture = Capture::default();
        while let Some(item) = stream.try_next().await? {
            match item {
                QueryItem::Metadata(_) => capture.metadata(),
                QueryItem::Row(row) => {
                    if capture.limit.is_none() {
                        capture.row(&Rec::from_row(&row));
                    }
                }
            }
        }
        Ok::<_, tiberius::error::Error>(capture)
    })
    .await;
    let capture = match response {
        Ok(Ok(capture)) => capture,
        _ => {
            // Reuse the driver's bounded attention primitive; its result never promotes this actor to settled.
            let _ = tokio::time::timeout(ATTENTION_TIMEOUT, client.cancel_query()).await;
            panic!("owned actual-plan capture failed; retire its actor before the admitted cleanup");
        }
    };
    // Even on a retention limit, the original response was fully drained above. Never start cleanup SQL while
    // dropping a partly consumed response, and never return the retained prefix as a successful capture.
    batch(client, "SET STATISTICS XML OFF").await;
    capture
}

#[derive(Default)]
struct Capture {
    rows: usize,
    bytes: usize,
    sets: usize,
    plans: Vec<String>,
    results: Vec<Rec>,
    limit: Option<&'static str>,
}

impl Capture {
    fn metadata(&mut self) {
        self.sets = self.sets.saturating_add(1);
        if self.sets > MAX_CAPTURE_RESULT_SETS {
            self.limit = Some("result sets");
        }
    }
    fn row(&mut self, row: &Rec) {
        self.rows = self.rows.saturating_add(1);
        self.bytes = self.bytes.saturating_add(value_bytes(row));
        if self.rows > MAX_CAPTURE_ROWS || self.bytes > MAX_CAPTURE_BYTES {
            self.limit = Some("rows or decoded bytes");
            return;
        }
        let mut plan_row = false;
        for (_, value) in row.columns() {
            if let Val::Text(xml) = value
                && xml.contains("<ShowPlanXML")
            {
                plan_row = true;
                if self.plans.len() == MAX_CAPTURE_PLANS {
                    self.limit = Some("plan count");
                    return;
                }
                self.plans.push(xml.clone());
            }
        }
        if !plan_row {
            self.results.push(row.clone());
        }
    }
    fn finish(self) -> Result<ActualPlanCapture, &'static str> {
        let capture = self.finish_response()?;
        if capture.plans.is_empty() {
            return Err("no actual XML plan returned");
        }
        Ok(capture)
    }
    fn finish_metadata(self) -> Result<Option<Vec<String>>, &'static str> {
        let capture = self.finish_response()?;
        Ok((!capture.plans.is_empty()).then_some(capture.plans))
    }
    fn finish_response(self) -> Result<ActualPlanCapture, &'static str> {
        if let Some(limit) = self.limit {
            return Err(limit);
        }
        Ok(ActualPlanCapture { plans: self.plans, results: self.results })
    }
}

#[test]
fn a_response_limit_never_turns_a_retained_plan_prefix_into_a_success() {
    let mut capture = Capture::default();
    capture.row(&Rec::of(&[("xml", Val::Text("<ShowPlanXML />".into()))]));
    for _ in 0..MAX_CAPTURE_ROWS {
        capture.row(&Rec::of(&[("count", Val::Int(1))]));
    }
    assert!(capture.finish().is_err());
    let mut capture = Capture::default();
    capture.row(&Rec::of(&[("xml", Val::Text("<ShowPlanXML />".into()))]));
    capture.row(&Rec::of(&[("native", Val::Text("x".repeat(MAX_CAPTURE_BYTES)))]));
    assert!(capture.finish().is_err());
}

#[test]
fn result_semantics_and_plan_xml_share_one_capture_and_one_existing_retention_budget() {
    let mut capture = Capture::default();
    capture.row(&Rec::of(&[("native_result", Val::Int(9007199254740993))]));
    capture.row(&Rec::of(&[("xml", Val::Text("<ShowPlanXML />".into()))]));
    let captured = capture.finish().unwrap();
    assert_eq!(captured.plans, vec!["<ShowPlanXML />"]);
    assert_eq!(crate::read::signed_integer(&captured.results[0], "native_result").unwrap(), 9007199254740993);
    assert_eq!(captured.results.len(), 1, "plan output is not an application result row");
}

#[test]
fn missing_metadata_showplan_stays_unknown_without_relaxing_actual_workload_capture() {
    let native_response = || {
        let mut capture = Capture::default();
        capture.row(&Rec::of(&[("native_metadata", Val::Int(16))]));
        capture
    };
    assert!(native_response().finish_metadata().unwrap().is_none());
    assert_eq!(native_response().finish().err(), Some("no actual XML plan returned"));
}

#[test]
fn optional_metadata_plans_preserve_available_xml_and_refuse_bounded_response_failures() {
    let xml = "<ShowPlanXML><QueryPlan /></ShowPlanXML>";
    let mut capture = Capture::default();
    capture.row(&Rec::of(&[("xml", Val::Text(xml.into()))]));
    assert_eq!(capture.finish_metadata().unwrap(), Some(vec![xml.into()]));
    for include_plan in [false, true] {
        let mut capture = Capture::default();
        if include_plan {
            capture.row(&Rec::of(&[("xml", Val::Text(xml.into()))]));
        }
        for _ in 0..=MAX_CAPTURE_ROWS {
            capture.row(&Rec::of(&[("native_metadata", Val::Int(1))]));
        }
        assert_eq!(capture.finish_metadata().unwrap_err(), "rows or decoded bytes");
    }
}
