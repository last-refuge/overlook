//! Per-account synchronization status and the literal product wording for it.
//!
//! The wording follows the brand voice table: literal, reassuring only when the underlying
//! state supports it.

use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "detail")]
pub enum SyncState {
    /// No connection to the provider. Downloaded mail is still available.
    Offline,
    Syncing,
    /// Synchronization is in progress but waiting, for example because the provider asked
    /// us to slow down. Work resumes automatically.
    Paused(String),
    UpToDate,
    SignInRequired,
    SyncFailed(String),
}

impl SyncState {
    pub fn label(&self) -> &str {
        match self {
            SyncState::Offline => "Offline",
            SyncState::Syncing => "Syncing",
            SyncState::Paused(_) => "Paused",
            SyncState::UpToDate => "Up to date",
            SyncState::SignInRequired => "Sign-in required",
            SyncState::SyncFailed(_) => "Sync failed",
        }
    }

    /// Longer sentence for tooltips and the account card.
    pub fn sentence(&self, pending: usize) -> String {
        match self {
            SyncState::Offline => {
                if pending == 0 {
                    "You’re offline. Downloaded mail is still available.".to_owned()
                } else {
                    format!(
                        "You’re offline. {} will sync when you’re back online.",
                        pending_phrase(pending)
                    )
                }
            }
            SyncState::Syncing => "Syncing your changes…".to_owned(),
            SyncState::Paused(reason) => reason.clone(),
            SyncState::UpToDate => "Up to date.".to_owned(),
            SyncState::SignInRequired => {
                "Sign in again to continue syncing. Your local drafts are safe.".to_owned()
            }
            SyncState::SyncFailed(reason) => format!("Sync failed: {reason}"),
        }
    }
}

pub fn pending_phrase(pending: usize) -> String {
    match pending {
        0 => "Nothing pending".to_owned(),
        1 => "1 change".to_owned(),
        n => format!("{n} changes"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub state: SyncState,
    pub last_successful_sync: Option<Timestamp>,
    pub pending_operations: usize,
    pub outbox_items: usize,
}

impl Default for AccountStatus {
    fn default() -> Self {
        Self {
            state: SyncState::Offline,
            last_successful_sync: None,
            pending_operations: 0,
            outbox_items: 0,
        }
    }
}

/// Wording for a draft's save indicator and Outbox rows.
pub fn draft_state_label(state: &crate::model::DraftState) -> String {
    use crate::model::DraftState::*;
    match state {
        Editing => "Saved on this device".to_owned(),
        Queued => "Waiting for connection".to_owned(),
        Sending => "Sending…".to_owned(),
        Uncertain => "Delivery uncertain: the server may have it. Send again or discard.".to_owned(),
        Failed(reason) => format!("Couldn’t send: {reason}"),
        Sent => "Sent".to_owned(),
    }
}

/// Local search must say what it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCoverage {
    pub messages_indexed: usize,
    pub messages_total: usize,
    pub bodies_cached: usize,
}

impl SearchCoverage {
    pub fn sentence(&self) -> String {
        if self.messages_total == 0 {
            return "No downloaded mail to search yet.".to_owned();
        }
        if self.bodies_cached >= self.messages_total {
            format!("Searching {} downloaded messages on this device.", self.messages_total)
        } else {
            format!(
                "Searching {} downloaded messages on this device. {} have headers only.",
                self.messages_total,
                self.messages_total.saturating_sub(self.bodies_cached)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_coverage_does_not_underflow_when_cached_exceeds_total() {
        let c = SearchCoverage {
            messages_indexed: 5,
            messages_total: 5,
            bodies_cached: 8,
        };
        assert_eq!(c.sentence(), "Searching 5 downloaded messages on this device.");
    }
}
