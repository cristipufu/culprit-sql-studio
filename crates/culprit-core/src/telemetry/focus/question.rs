//! A finite set of implemented native questions over one exact authenticated original request.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticFocus {
    PageWait { focus: Box<ActivityWaitFocus> },
    RequestMemory { focus: Box<ActivityRequestFocus> },
}

impl DiagnosticFocus {
    pub fn participant(&self) -> &ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &focus.participant,
            Self::RequestMemory { focus } => focus,
        }
    }
    pub fn participant_mut(&mut self) -> &mut ActivityRequestFocus {
        match self {
            Self::PageWait { focus } => &mut focus.participant,
            Self::RequestMemory { focus } => focus,
        }
    }
    pub fn page_wait(&self) -> Option<&ActivityWaitFocus> {
        match self {
            Self::PageWait { focus } => Some(focus),
            _ => None,
        }
    }
    pub fn page_wait_mut(&mut self) -> Option<&mut ActivityWaitFocus> {
        match self {
            Self::PageWait { focus } => Some(focus),
            _ => None,
        }
    }
    pub fn selector(&self) -> RecipeSelector {
        match self {
            Self::PageWait { .. } => RecipeSelector::FocusedPageWait,
            Self::RequestMemory { .. } => RecipeSelector::FocusedRequestMemory,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::PageWait { focus } => focus.is_valid(),
            Self::RequestMemory { focus } => focus.is_request_memory_focus(),
        }
    }
    pub fn from_selection(selection: &CollectionSelection) -> Option<Self> {
        match selection {
            CollectionSelection::FocusedPageWait { focus } | CollectionSelection::SharedPageWaitFocus { focus, .. } => {
                Some(Self::PageWait { focus: focus.clone() })
            }
            CollectionSelection::FocusedRequestMemory { focus } => Some(Self::RequestMemory { focus: focus.clone() }),
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
}

impl From<ActivityWaitFocus> for DiagnosticFocus {
    fn from(focus: ActivityWaitFocus) -> Self {
        Self::PageWait { focus: Box::new(focus) }
    }
}
