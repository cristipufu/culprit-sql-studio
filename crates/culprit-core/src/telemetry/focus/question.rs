//! A finite set of implemented native questions over one exact authenticated original request.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticFocus {
    Blocking {
        focus: Box<ActivityRequestFocus>,
    },
    PageWait {
        focus: Box<ActivityWaitFocus>,
    },
    PageWaitWriters {
        focus: Box<PageWaitWriterFocus>,
    },
    PageTaskHolders {
        focus: Box<PageTaskHolderFocus>,
    },
    RequestMemory {
        focus: Box<ActivityRequestFocus>,
    },
    CompilationAdmission {
        focus: Box<ActivityRequestFocus>,
    },
    CurrentRequestWork {
        focus: Box<ActivityRequestFocus>,
    },
    /// The incident participant justifies a bounded population lookup. It never makes other task rows this
    /// request's exclusive population; every result still names the exact earlier Activity capture.
    PageWaitTasks {
        focus: Box<ActivityRequestFocus>,
        original_context: Box<NativeReadContext>,
        original_requests: Vec<ActivityRequestSelector>,
    },
}

impl DiagnosticFocus {
    pub fn participant(&self) -> &ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &focus.participant,
            Self::PageWaitWriters { focus } => &focus.waiter.participant,
            Self::PageTaskHolders { focus } => &focus.participant,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => focus,
        }
    }
    pub fn participant_mut(&mut self) -> &mut ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &mut focus.participant,
            Self::PageWaitWriters { focus } => &mut focus.waiter.participant,
            Self::PageTaskHolders { focus } => &mut focus.participant,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => focus,
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
            Self::PageTaskHolders { .. } => RecipeSelector::FocusedPageTaskHolders,
            Self::RequestMemory { .. } => RecipeSelector::FocusedRequestMemory,
            Self::CompilationAdmission { .. } => RecipeSelector::FocusedCompilationAdmission,
            Self::CurrentRequestWork { .. } => RecipeSelector::CurrentRequestWork,
            Self::PageWaitTasks { .. } => RecipeSelector::PageWaitTasks,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Blocking { focus } => focus.is_blocking_focus(),
            Self::PageWait { focus } => focus.is_valid(),
            Self::PageWaitWriters { focus } => focus.is_valid(),
            Self::PageTaskHolders { focus } => focus.is_valid(),
            Self::RequestMemory { focus } => focus.is_request_memory_focus(),
            Self::CompilationAdmission { focus } => focus.is_compilation_admission_focus(),
            Self::CurrentRequestWork { focus } => focus.is_current_request_work_focus(),
            Self::PageWaitTasks { focus, original_context, original_requests } => {
                focus.is_page_wait_tasks_focus(original_context)
                    && ActivityRequestSelector::is_canonical(original_requests)
                    && original_requests.iter().any(|request| {
                        request.key == focus.original.key && request.statement_identity == focus.statement_identity
                    })
            }
        }
    }
    pub fn from_selection(selection: &CollectionSelection) -> Option<Self> {
        match selection {
            CollectionSelection::FocusedBlocking { focus } => Some(Self::Blocking { focus: focus.clone() }),
            CollectionSelection::FocusedPageWait { focus } | CollectionSelection::SharedPageWaitFocus { focus, .. } => {
                Some(Self::PageWait { focus: focus.clone() })
            }
            CollectionSelection::PageWaitWriters { focus } => Some(Self::PageWaitWriters { focus: focus.clone() }),
            CollectionSelection::PageTaskHolders { focus } => Some(Self::PageTaskHolders { focus: focus.clone() }),
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
        self.is_valid()
            && self.participant().matches_parent(source, original, reference)
            && match self {
                Self::PageWaitTasks { original_context, .. } => {
                    NativeReadContext::from_activity(source).is_ok_and(|context| context == **original_context)
                }
                _ => true,
            }
    }

    /// Immutable prerequisites are authenticated and pinned before admission, including every original writer
    /// candidate. A request can occupy more than one role while retaining just one exact input receipt.
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        let (page, candidates) = match self {
            Self::PageWaitWriters { focus } => (Some(&focus.page), focus.candidates.as_slice()),
            Self::PageTaskHolders { focus } => (Some(&focus.page), &[][..]),
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
            Self::PageTaskHolders { focus } => CollectionSelection::PageTaskHolders { focus: focus.clone() },
            Self::RequestMemory { focus } => CollectionSelection::FocusedRequestMemory { focus: focus.clone() },
            Self::CompilationAdmission { focus } => {
                CollectionSelection::FocusedCompilationAdmission { focus: focus.clone() }
            }
            Self::CurrentRequestWork { focus } => CollectionSelection::CurrentRequestWork { focus: focus.clone() },
            Self::PageWaitTasks { focus, original_context, original_requests } => CollectionSelection::PageWaitTasks {
                activity: focus.parent.capture_ref(),
                original_context: original_context.clone(),
                original_requests: original_requests.clone(),
            },
        }
    }
}

impl From<ActivityWaitFocus> for DiagnosticFocus {
    fn from(focus: ActivityWaitFocus) -> Self {
        Self::PageWait { focus: Box::new(focus) }
    }
}
