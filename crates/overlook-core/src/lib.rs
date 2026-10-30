//! Overlook domain core.
//!
//! This crate is independent of any UI toolkit, database, or network library. It defines
//! the vocabulary the rest of Overlook shares: accounts, labels, folders, messages,
//! conversations, drafts, pending operations, synchronization status, and appearance
//! preferences. It also contains pure logic that must behave identically everywhere:
//! conversation grouping (threading by References for IMAP and POP3), truthful status wording, HTML sanitization, and theme resolution.

pub mod appearance;
pub mod attachment_policy;
pub mod browser;
pub mod calendar;
pub mod calendar_connection;
pub mod caldav_change;
pub use caldav_change::*;
pub mod compose;
pub mod focus;
pub mod ics;
pub mod mailto;
pub mod ids;
pub mod model;
pub mod ops;
pub mod prefs;
pub mod recipients;
pub mod prepass;
pub mod sanitize;
pub mod servers;
pub mod signature;
pub mod status;
pub mod threading;
pub mod time;
pub mod tz;

pub use appearance::{AppearancePreference, ResolvedTheme, SystemAppearance};
pub use browser::{prepare_document, BrowserDocument};
pub use calendar::*;
pub use calendar_connection::*;
pub use focus::{classify_focus, FocusRule, FocusTab};
pub use ids::*;
pub use model::*;
pub use ops::*;
pub use prefs::*;
pub use servers::*;
pub use status::*;
