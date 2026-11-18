//! Implemented questions retain an exact original receipt; only request questions carry an Activity participant.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticFocus {
    SourceRead {
        focus: Box<SourceReadFocus>,
    },
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
    pub fn activity_participant(&self) -> Option<&ActivityRequestFocus> {
        match self {
            Self::SourceRead { .. } => None,
            Self::PageWait { focus } => Some(&focus.participant),
            Self::PageWaitWriters { focus } => Some(&focus.waiter.participant),
            Self::PageTaskHolders { focus } => Some(&focus.participant),
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => Some(focus),
        }
    }
    pub fn activity_participant_mut(&mut self) -> Option<&mut ActivityRequestFocus> {
        match self {
            Self::SourceRead { .. } => None,
            Self::PageWait { focus } => Some(&mut focus.participant),
            Self::PageWaitWriters { focus } => Some(&mut focus.waiter.participant),
            Self::PageTaskHolders { focus } => Some(&mut focus.participant),
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => Some(focus),
        }
    }
    pub fn parent(&self) -> &DiagnosticEvidenceRef {
        match self {
            Self::SourceRead { focus } => &focus.basis,
            Self::PageWait { focus } => &focus.participant.parent,
            Self::PageWaitWriters { focus } => &focus.waiter.participant.parent,
            Self::PageTaskHolders { focus } => &focus.participant.parent,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => &focus.parent,
        }
    }
    pub fn target(&self) -> &CollectionTarget {
        match self {
            Self::SourceRead { focus } => &focus.target,
            Self::PageWait { focus } => &focus.participant.target,
            Self::PageWaitWriters { focus } => &focus.waiter.participant.target,
            Self::PageTaskHolders { focus } => &focus.participant.target,
            Self::Blocking { focus }
            | Self::RequestMemory { focus }
            | Self::CompilationAdmission { focus }
            | Self::CurrentRequestWork { focus }
            | Self::PageWaitTasks { focus, .. } => &focus.target,
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
            Self::SourceRead { focus } => focus.question.selector(),
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
            Self::SourceRead { focus } => focus.is_valid(),
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
            CollectionSelection::SourceRead { focus, .. } => Some(Self::SourceRead { focus: focus.clone() }),
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
            && self
                .activity_participant()
                .is_some_and(|participant| participant.matches_parent(source, original, reference))
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
        let prerequisite = match self {
            Self::SourceRead { focus } => Some(&focus.prerequisite),
            _ => None,
        };
        std::iter::once(self.parent())
            .chain(prerequisite.into_iter().flat_map(SourceReadPrerequisite::references))
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
    pub fn activity_selection(&self) -> Option<CollectionSelection> {
        Some(match self {
            Self::SourceRead { .. } => return None,
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
        })
    }
}

impl From<ActivityWaitFocus> for DiagnosticFocus {
    fn from(focus: ActivityWaitFocus) -> Self {
        Self::PageWait { focus: Box::new(focus) }
    }
}
