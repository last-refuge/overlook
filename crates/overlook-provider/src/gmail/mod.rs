//! Gmail through the Gmail API.
//!
//! - `oauth`: installed-app OAuth 2.0 (PKCE, loopback redirect, token exchange/refresh).
//! - `api`: a small authenticated REST client with transparent token refresh.
//! - `mapping`: Gmail JSON to Overlook's domain types. Outgoing RFC 822 bytes for
//!   `messages.send` come from the shared `crate::outgoing` builder.
//! - `provider`: the `MailProvider` implementation.

pub mod api;
pub mod calendar;
pub mod mapping;
mod drafts;
pub mod oauth;
pub mod provider;

pub use api::GmailClient;
pub use oauth::{AuthorizationRequest, ClientConfig, OAuthTokens, Pkce, UserInfo};
pub use calendar::GoogleCalendarProvider;
pub use provider::GmailProvider;
