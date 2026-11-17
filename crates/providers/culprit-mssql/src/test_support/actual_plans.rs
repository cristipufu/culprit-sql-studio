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
    capture.finish().expect("owned actual-plan response exceeded its explicit capture bounds")
}

#[derive(Default)]
struct Capture {
    rows: usize,
    bytes: usize,
    sets: usize,
    plans: Vec<String>,
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
        for (_, value) in row.columns() {
            if let Val::Text(xml) = value
                && xml.contains("<ShowPlanXML")
            {
                if self.plans.len() == MAX_CAPTURE_PLANS {
                    self.limit = Some("plan count");
                    return;
                }
                self.plans.push(xml.clone());
            }
        }
    }
    fn finish(self) -> Result<Vec<String>, &'static str> {
        if let Some(limit) = self.limit {
            return Err(limit);
        }
        if self.plans.is_empty() {
            return Err("no actual XML plan returned");
        }
        Ok(self.plans)
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
