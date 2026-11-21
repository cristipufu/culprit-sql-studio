//! Shared presentation shape; native CDC and change-feed facts and reports remain distinct.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ChangeCaptureEntryKind {
    ScanSession,
    Aggregate,
    Error,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ChangeCaptureField {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ChangeCaptureEntry {
    pub source: String,
    pub observation_key: String,
    pub kind: ChangeCaptureEntryKind,
    pub observed_at: String,
    pub title: String,
    pub explanation: String,
    pub fields: Vec<ChangeCaptureField>,
    pub investigation: String,
    pub verify: String,
}
