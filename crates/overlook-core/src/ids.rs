//! Strongly typed identifiers.
//!
//! Local identifiers are UUIDs generated on this device. Provider identifiers (for
//! example a Gmail message id or an IMAP UID) are stored separately and never reused as
//! local keys, so a message can exist locally before the provider has assigned it an id.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Generates a new random local identifier.
            pub fn generate() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// Identifies a configured account on this device.
    AccountId
);
id_type!(
    /// Local identifier of a message.
    MessageId
);
id_type!(
    /// Local identifier of a conversation (Gmail thread or heuristically grouped messages).
    ConversationId
);
id_type!(
    /// Local identifier of a label (Gmail) or system mailbox view.
    LabelId
);
id_type!(
    /// Local identifier of an IMAP folder. Unused until phase 2.
    FolderId
);
id_type!(
    /// Local identifier of a draft, including Outbox items.
    DraftId
);
id_type!(
    /// Local identifier of an attachment record.
    AttachmentId
);
id_type!(
    /// Local identifier of a pending operation.
    OperationId
);
id_type!(
    /// Local identifier of a calendar.
    CalendarId
);
id_type!(
    /// Local identity of an independently configured calendar connection.
    CalendarConnectionId
);
id_type!(
    /// Local identifier of a subscribed iCalendar (.ics / webcal) URL.
    CalendarSubscriptionId
);
id_type!(
    /// Local identifier of a calendar event (an expanded instance for recurring events).
    EventId
);

/// An identifier assigned by the remote provider. Kept distinct from local ids on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
