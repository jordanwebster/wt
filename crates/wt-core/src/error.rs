use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExitClass {
    Ok = 0,
    Internal = 1,
    Usage = 2,
    NotFound = 3,
    Conflict = 4,
    State = 5,
    ChildFailed = 6,
    External = 7,
    Timeout = 8,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrorCode(pub String);

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ErrorCode {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Error)]
#[error("{code}: {message}")]
pub struct CoreError {
    pub class: ExitClass,
    pub code: ErrorCode,
    pub message: String,
    pub remedy: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

impl CoreError {
    pub fn new(
        class: ExitClass,
        code: impl Into<ErrorCode>,
        message: impl Into<String>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            class,
            code: code.into(),
            message: message.into(),
            remedy: remedy.into(),
            details: Value::Null,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub const fn exit(&self) -> u8 {
        self.class as u8
    }
}
