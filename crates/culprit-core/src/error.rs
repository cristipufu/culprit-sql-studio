use crate::i18n::Message;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    ConnectionFailed,
    AuthFailed,
    PermissionDenied,
    Timeout,
    Unsupported,
    QueryFailed,
    Internal,
    /// The statement changes data on a production connection: ask the person, then retry with confirmation.
    ConfirmationRequired,
}

/// Facts a caller can act on independently of a localized error message or hint.
#[derive(Serialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ErrorDetail {
    Statement(crate::sql::StatementProblem),
    ActualPlanStatisticsUnavailable,
}

#[derive(Serialize, TS, Clone, Debug, thiserror::Error)]
#[error("{message}")]
#[ts(export)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<ErrorDetail>,
    #[serde(skip)]
    #[ts(skip)]
    message_source: Option<Box<Message>>,
    #[serde(skip)]
    #[ts(skip)]
    hint_source: Option<Box<Message>>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<Message>) -> Self {
        Error { code, message: String::new(), hint: None, detail: None, message_source: None, hint_source: None }
            .with_message(message)
    }

    pub fn with_message(mut self, message: impl Into<Message>) -> Self {
        let message = message.into();
        self.message = message.render();
        self.message_source = message.is_translatable().then(|| Box::new(message));
        self
    }

    pub fn with_hint(mut self, hint: impl Into<Message>) -> Self {
        let hint = hint.into();
        self.hint = Some(hint.render());
        self.hint_source = hint.is_translatable().then(|| Box::new(hint));
        self
    }

    pub fn message_source(&self) -> Message {
        self.message_source.as_deref().cloned().unwrap_or_else(|| Message::from(self.message.clone()))
    }

    pub fn hint_source(&self) -> Option<Message> {
        self.hint_source.as_deref().cloned().or_else(|| self.hint.clone().map(Message::from))
    }

    pub fn with_detail(mut self, detail: ErrorDetail) -> Self {
        self.detail = Some(detail);
        self
    }

    pub fn invalid(message: impl Into<Message>) -> Self {
        Error::new(ErrorCode::InvalidInput, message)
    }

    pub fn not_found(message: impl Into<Message>) -> Self {
        Error::new(ErrorCode::NotFound, message)
    }

    pub fn internal(message: impl Into<Message>) -> Self {
        Error::new(ErrorCode::Internal, message)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
