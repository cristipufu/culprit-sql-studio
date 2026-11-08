use crate::finding::{CheckInfo, Finding};
use crate::model::Snapshot;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Whether the inputs let a check assess its subject. Missing evidence is never a clean bill of health.
#[derive(Clone, Debug)]
pub enum CheckEvidence {
    Available,
    Unavailable(String),
    Insufficient(String),
    /// These statements were assessed, while other statements lack the required measurements.
    Queries {
        assessed: Vec<String>,
        reason: Option<String>,
    },
    NotApplicable(String),
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CheckStatus {
    Findings,
    Assessed,
    Unavailable,
    InsufficientEvidence,
    NotApplicable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CheckAssessment {
    pub check: String,
    pub title: String,
    pub status: CheckStatus,
    pub reason: Option<String>,
    // Statements with complete evidence when the check's overall coverage is incomplete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub assessed_queries: Vec<String>,
}

/// A check is a pure function from a snapshot to findings. It never touches a live connection. Every built-in check is a
/// [`Diagnosis`], which is a `Check`; an embedder can implement either.
pub trait Check: Send + Sync {
    fn info(&self) -> CheckInfo;
    fn run(&self, snapshot: &Snapshot) -> Vec<Finding>;
    /// The extension's version, when its author records one. It never inherits Culprit's own package version.
    fn version(&self) -> Option<&str> {
        None
    }
    /// Embedders can declare positive evidence too. Existing plugins still run, without claiming a negative result.
    fn evidence(&self, _snapshot: &Snapshot) -> CheckEvidence {
        CheckEvidence::Insufficient(crate::t!("evidence.undeclared"))
    }
}

/// A check in two halves, the shape every built-in check has. [`Diagnosis::decide`] establishes what the evidence
/// supports, as typed cases that hold facts (names, numbers, which advice applies) and no prose; [`Diagnosis::render`]
/// words one case as a finding, in the current language ([`crate::i18n`]). Language lives only in `render`, so a finding
/// can be said in another language, and its wording changed, without touching what the check concludes.
///
/// Every `Diagnosis` is a [`Check`]: its findings are its cases, each rendered.
pub trait Diagnosis: Send + Sync {
    /// What the check concludes about one subject. It may borrow from the snapshot it was decided on.
    type Case<'s>;
    fn info(&self) -> CheckInfo;
    /// The cases the snapshot establishes, in the order their findings are listed.
    fn decide<'s>(&self, snapshot: &'s Snapshot) -> Vec<Self::Case<'s>>;
    /// One case, worded as a finding.
    fn render<'s>(&self, snapshot: &'s Snapshot, case: Self::Case<'s>) -> Finding;
    /// See [`Check::version`].
    fn version(&self) -> Option<&str> {
        None
    }
    /// See [`Check::evidence`].
    fn evidence(&self, _snapshot: &Snapshot) -> CheckEvidence {
        CheckEvidence::Insufficient(crate::t!("evidence.undeclared"))
    }
}

impl<D: Diagnosis> Check for D {
    fn info(&self) -> CheckInfo {
        Diagnosis::info(self)
    }
    fn run(&self, snapshot: &Snapshot) -> Vec<Finding> {
        self.decide(snapshot).into_iter().map(|case| self.render(snapshot, case)).collect()
    }
    fn version(&self) -> Option<&str> {
        Diagnosis::version(self)
    }
    fn evidence(&self, snapshot: &Snapshot) -> CheckEvidence {
        Diagnosis::evidence(self, snapshot)
    }
}
