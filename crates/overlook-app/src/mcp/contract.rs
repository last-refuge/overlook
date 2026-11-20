//! Transport-independent operations for Overlook's local, account-scoped service.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Request {
    AccountsList,
    AccountCapabilities { account_id: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Response {
    Accounts { accounts: Vec<Account> },
    Capabilities(Capabilities),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Account {
    pub account_id: String,
    pub display_name: String,
    pub email: String,
    pub provider: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Capabilities {
    pub account_id: String,
    pub mail: bool,
    pub calendar: bool,
    pub contacts: bool,
}

pub type Reply = Result<Response, Failure>;

#[derive(Debug, Deserialize, Serialize)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AccessDenied,
    ProfileUnavailable,
    UnsupportedVersion,
    InvalidRequest,
    Internal,
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}
