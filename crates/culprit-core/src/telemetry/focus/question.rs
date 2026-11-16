//! A finite set of implemented native questions over one exact authenticated original request.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticFocus {
    Blocking { focus: Box<ActivityRequestFocus> },
    PageWait { focus: Box<ActivityWaitFocus> },
    PageWaitWriters { focus: Box<PageWaitWriterFocus> },
    RequestMemory { focus: Box<ActivityRequestFocus> },
    CompilationAdmission { focus: Box<ActivityRequestFocus> },
    CurrentRequestWork { focus: Box<ActivityRequestFocus> },
}

impl DiagnosticFocus {
    pub fn participant(&self) -> &ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &focus.participant,
            Self::PageWaitWriters { focus } => &focus.waiter.participant,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus } => focus,
        }
    }
    pub fn participant_mut(&mut self) -> &mut ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &mut focus.participant,
            Self::PageWaitWriters { focus } => &mut focus.waiter.participant,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus } => focus,
        }
    }
    pub fn page_wait(&self) -> Option<&ActivityWaitFocus> {
        match self {
            Self::PageWait { focus } => Some(focus),
            Self::PageWaitWriters { focus } => Some(&focus.waiter),
            _ => None,
        }
    }
    pub fn page_wait_mut(&mut self) -> Option<&mut ActivityWaitFocus> {
        match self {
            Self::PageWait { focus } => Some(focus),
            Self::PageWaitWriters { focus } => Some(&mut focus.waiter),
            _ => None,
        }
    }
    pub fn selector(&self) -> RecipeSelector {
        match self {
            Self::Blocking { .. } => RecipeSelector::FocusedBlocking,
            Self::PageWait { .. } => RecipeSelector::FocusedPageWait,
            Self::PageWaitWriters { .. } => RecipeSelector::FocusedPageWaitWriters,
            Self::RequestMemory { .. } => RecipeSelector::FocusedRequestMemory,
            Self::CompilationAdmission { .. } => RecipeSelector::FocusedCompilationAdmission,
            Self::CurrentRequestWork { .. } => RecipeSelector::CurrentRequestWork,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Blocking { focus } => focus.is_blocking_focus(),
            Self::PageWait { focus } => focus.is_valid(),
            Self::PageWaitWriters { focus } => focus.is_valid(),
            Self::RequestMemory { focus } => focus.is_request_memory_focus(),
            Self::CompilationAdmission { focus } => focus.is_compilation_admission_focus(),
            Self::CurrentRequestWork { focus } => focus.is_current_request_work_focus(),
        }
    }
    pub fn from_selection(selection: &CollectionSelection) -> Option<Self> {
        match selection {
            CollectionSelection::FocusedBlocking { focus } => Some(Self::Blocking { focus: focus.clone() }),
            CollectionSelection::FocusedPageWait { focus } | CollectionSelection::SharedPageWaitFocus { focus, .. } => {
                Some(Self::PageWait { focus: focus.clone() })
            }
            CollectionSelection::PageWaitWriters { focus } => Some(Self::PageWaitWriters { focus: focus.clone() }),
            CollectionSelection::FocusedRequestMemory { focus } => Some(Self::RequestMemory { focus: focus.clone() }),
            CollectionSelection::CurrentRequestWork { focus } => {
                Some(Self::CurrentRequestWork { focus: focus.clone() })
            }
            CollectionSelection::FocusedCompilationAdmission { focus }
            | CollectionSelection::SharedCompilationAdmissionFocus { focus, .. } => {
                Some(Self::CompilationAdmission { focus: focus.clone() })
            }
            _ => None,
        }
    }
    pub fn matches_parent(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> bool {
        self.is_valid() && self.participant().matches_parent(source, original, reference)
    }

    /// Immutable prerequisites are authenticated and pinned before admission, including every original writer
    /// candidate. A request can occupy more than one role while retaining just one exact input receipt.
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        let (page, candidates) = match self {
            Self::PageWaitWriters { focus } => (Some(&focus.page), focus.candidates.as_slice()),
            _ => (None, &[][..]),
        };
        let mut seen = Vec::new();
        std::iter::once(&self.participant().parent)
            .chain(page)
            .chain(candidates.iter().map(|candidate| &candidate.parent))
            .filter(move |reference| {
                if seen.contains(reference) {
                    false
                } else {
                    seen.push(*reference);
                    true
                }
            })
    }

    /// Shapes an unadmitted database question. Authentication, cost qualification and native guards remain mandatory.
    pub fn database_selection(&self) -> CollectionSelection {
        match self {
            Self::Blocking { focus } => CollectionSelection::FocusedBlocking { focus: focus.clone() },
            Self::PageWait { focus } => CollectionSelection::FocusedPageWait { focus: focus.clone() },
            Self::PageWaitWriters { focus } => CollectionSelection::PageWaitWriters { focus: focus.clone() },
            Self::RequestMemory { focus } => CollectionSelection::FocusedRequestMemory { focus: focus.clone() },
            Self::CompilationAdmission { focus } => {
                CollectionSelection::FocusedCompilationAdmission { focus: focus.clone() }
            }
            Self::CurrentRequestWork { focus } => CollectionSelection::CurrentRequestWork { focus: focus.clone() },
        }
    }
}

impl From<ActivityWaitFocus> for DiagnosticFocus {
    fn from(focus: ActivityWaitFocus) -> Self {
        Self::PageWait { focus: Box::new(focus) }
    }
}
