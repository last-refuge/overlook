//! Standards-based read receipts (RFC 8098 Message Disposition Notifications).
//!
//! Overlook never embeds tracking pixels. When a user explicitly opts in on a message,
//! outgoing MIME adds a `Disposition-Notification-To` header (RFC 8098 §2.1). Whether a
//! receipt is returned depends entirely on the recipient's mail client and mail provider
//! policy. Incoming `message/disposition-notification` reports are parsed here so the UI
//! can describe them truthfully as a receipt sent by the recipient's mail client.

use crate::ids::MessageId;
use crate::model::Address;
use crate::threading::normalize_message_id;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// Label for the per-message opt-in compose toggle when off.
pub const REQUEST_READ_RECEIPT_LABEL: &str = "Request read receipt";

/// Label for the per-message opt-in compose toggle when enabled.
pub const REQUEST_READ_RECEIPT_ACTIVE_LABEL: &str = "Read receipt requested";

/// Explanation shown in the composer tooltip for the opt-in toggle.
pub const REQUEST_READ_RECEIPT_TOOLTIP: &str = "Adds a standards-based RFC 8098 read receipt request to this message. Delivery of a receipt depends on the recipient's mail client and provider policy; Overlook does not use tracking pixels.";

/// Canonical, truthful label for a confirmed RFC 8098 `displayed` receipt.
pub const CONFIRMED_READ_RECEIPT_LABEL: &str = "Read receipt from the recipient's mail client";

/// Plural form when more than one recipient's client returned a confirmed receipt.
pub const CONFIRMED_READ_RECEIPTS_PLURAL_LABEL: &str = "Read receipts from recipients' mail clients";

/// RFC 8098 §3.2.6.2 disposition type reported by the recipient's mail client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadReceiptDisposition {
    /// The message was displayed to the user (`displayed`).
    Displayed,
    /// The message was deleted without being displayed (`deleted`).
    Deleted,
    /// The message was dispatched to another medium/system (`dispatched`).
    Dispatched,
    /// The message was processed automatically without display (`processed`).
    Processed,
    /// Another or unrecognized RFC 8098 disposition type.
    Other,
}

impl ReadReceiptDisposition {
    fn parse(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "displayed" => Self::Displayed,
            "deleted" => Self::Deleted,
            "dispatched" => Self::Dispatched,
            "processed" => Self::Processed,
            _ => Self::Other,
        }
    }
}

/// Machine-readable fields parsed from an RFC 8098 `message/disposition-notification` part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadReceipt {
    /// Normalized RFC 5322 `Original-Message-ID` (with angle brackets), when present.
    #[serde(default)]
    pub original_message_id: Option<String>,
    /// Recipient address from `Final-Recipient` (falling back to `Original-Recipient`),
    /// with any `rfc822;` address-type prefix stripped.
    #[serde(default)]
    pub final_recipient: Option<String>,
    /// Mail client or server identified by `Reporting-UA`, when present.
    #[serde(default)]
    pub reporting_ua: Option<String>,
    /// Raw RFC 8098 `Disposition` header value (e.g. `manual-action/MDN-sent-manually; displayed`).
    pub disposition: String,
    /// Parsed disposition type from `Disposition`.
    pub disposition_type: ReadReceiptDisposition,
    /// Action mode from `Disposition` (`manual-action` or `automatic-action`), when present.
    #[serde(default)]
    pub action_mode: Option<String>,
    /// Sending mode from `Disposition` (`MDN-sent-manually` or `MDN-sent-automatically`), when present.
    #[serde(default)]
    pub sending_mode: Option<String>,
}

/// An incoming RFC 8098 receipt matched to a sent message by `Original-Message-ID`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedReadReceipt {
    pub receipt_message_id: MessageId,
    pub received_at: Timestamp,
    pub sender: Address,
    pub receipt: ReadReceipt,
}

impl ReadReceipt {
    pub fn is_displayed(&self) -> bool {
        self.disposition_type == ReadReceiptDisposition::Displayed
    }

    /// Truthful headline describing this receipt without overclaiming universal open tracking.
    pub fn summary_banner(&self) -> &'static str {
        match self.disposition_type {
            ReadReceiptDisposition::Displayed => CONFIRMED_READ_RECEIPT_LABEL,
            ReadReceiptDisposition::Deleted => "Deletion receipt from the recipient's mail client",
            ReadReceiptDisposition::Dispatched
            | ReadReceiptDisposition::Processed
            | ReadReceiptDisposition::Other => "Disposition receipt from the recipient's mail client",
        }
    }

    /// Short human-readable description of who sent the receipt, how, and from which client.
    pub fn detail_summary(&self, fallback_sender: Option<&Address>) -> String {
        let who = self
            .final_recipient
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                fallback_sender
                    .filter(|a| !a.email.trim().is_empty())
                    .map(|a| a.to_header())
            });
        let action = match self.disposition_type {
            ReadReceiptDisposition::Displayed => "reported as displayed",
            ReadReceiptDisposition::Deleted => "reported as deleted without being displayed",
            ReadReceiptDisposition::Dispatched => "reported as dispatched",
            ReadReceiptDisposition::Processed => "reported as processed",
            ReadReceiptDisposition::Other => "reported a disposition update",
        };
        let mode = self.sending_mode.as_deref().and_then(|m| {
            let lower = m.to_ascii_lowercase();
            if lower.contains("manually") {
                Some("sent manually")
            } else if lower.contains("automatically") {
                Some("sent automatically")
            } else {
                None
            }
        });
        let client = self
            .reporting_ua
            .as_deref()
            .map(clean_reporting_ua)
            .filter(|s| !s.is_empty());

        let mut parts = Vec::new();
        match who {
            Some(who) => parts.push(format!("{who} {action}")),
            None => parts.push(format!("Message {action}")),
        }
        if let Some(mode) = mode {
            parts.push(mode.to_owned());
        }
        if let Some(client) = client {
            parts.push(format!("via {client}"));
        }
        format!("{}.", parts.join(" · "))
    }
}

/// Cleans a `Reporting-UA` field (`host; product`) for concise display.
fn clean_reporting_ua(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((host, product)) = trimmed.split_once(';') {
        let product = product.trim();
        let host = host.trim();
        if !product.is_empty() {
            return product.to_owned();
        }
        return host.to_owned();
    }
    trimmed.to_owned()
}

/// Extracts the bare address from an RFC 8098 `Final-Recipient` / `Original-Recipient` value
/// such as `rfc822; maya@northstar.example` or `rfc822; <maya@northstar.example>`.
fn parse_mdn_recipient(value: &str) -> Option<String> {
    let raw = match value.split_once(';') {
        Some((_, addr)) => addr,
        None => value,
    };
    let cleaned = raw.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// Unfolds RFC 5322 continuation lines and yields `(field_name, field_value)` pairs.
fn unfold_fields(text: &str) -> Vec<(String, String)> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            // Stop at the first blank line only if we already collected fields;
            // leading blank lines are skipped gracefully.
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            let prev = lines.last_mut().unwrap();
            prev.push(' ');
            prev.push_str(line.trim());
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
        .into_iter()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim();
            let value = value.trim();
            (!name.is_empty()).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Parses an RFC 8098 `message/disposition-notification` body.
///
/// Returns `None` if the mandatory `Disposition` field is missing or blank.
pub fn parse_mdn(text: &str) -> Option<ReadReceipt> {
    let mut reporting_ua = None;
    let mut final_recipient = None;
    let mut original_recipient = None;
    let mut original_message_id = None;
    let mut disposition = None;

    for (name, value) in unfold_fields(text) {
        if value.is_empty() {
            continue;
        }
        match name.to_ascii_lowercase().as_str() {
            "reporting-ua" if reporting_ua.is_none() => reporting_ua = Some(value),
            "final-recipient" if final_recipient.is_none() => {
                final_recipient = parse_mdn_recipient(&value);
            }
            "original-recipient" if original_recipient.is_none() => {
                original_recipient = parse_mdn_recipient(&value);
            }
            "original-message-id" if original_message_id.is_none() => {
                let normalized = normalize_message_id(&value);
                if !normalized.is_empty() {
                    original_message_id = Some(normalized);
                }
            }
            "disposition" if disposition.is_none() => disposition = Some(value),
            _ => {}
        }
    }

    let disposition = disposition?;
    let (modes_part, type_part) = match disposition.split_once(';') {
        Some((modes, rest)) => (Some(modes.trim()), rest.trim()),
        None => (None, disposition.trim()),
    };
    let (action_mode, sending_mode) = match modes_part.and_then(|m| m.split_once('/')) {
        Some((a, s)) => (
            (!a.trim().is_empty()).then(|| a.trim().to_owned()),
            (!s.trim().is_empty()).then(|| s.trim().to_owned()),
        ),
        None => (None, None),
    };
    let raw_type = type_part
        .split(['/', ';', ','])
        .next()
        .unwrap_or("")
        .trim();
    if raw_type.is_empty() {
        return None;
    }
    let disposition_type = ReadReceiptDisposition::parse(raw_type);

    Some(ReadReceipt {
        original_message_id,
        final_recipient: final_recipient.or(original_recipient),
        reporting_ua,
        disposition,
        disposition_type,
        action_mode,
        sending_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_rfc8098_mdn_for_displayed_message() {
        let mdn = "\
Reporting-UA: mail.northstar.example; Mozilla Thunderbird 128.0\r\n\
Final-Recipient: rfc822; maya@northstar.example\r\n\
Original-Message-ID: <draft-42@overlook.lastrefuge.ai>\r\n\
Disposition: manual-action/MDN-sent-manually; displayed\r\n";

        let receipt = parse_mdn(mdn).expect("valid RFC 8098 MDN parses");
        assert!(receipt.is_displayed());
        assert_eq!(receipt.disposition_type, ReadReceiptDisposition::Displayed);
        assert_eq!(
            receipt.original_message_id.as_deref(),
            Some("<draft-42@overlook.lastrefuge.ai>")
        );
        assert_eq!(
            receipt.final_recipient.as_deref(),
            Some("maya@northstar.example")
        );
        assert_eq!(
            receipt.reporting_ua.as_deref(),
            Some("mail.northstar.example; Mozilla Thunderbird 128.0")
        );
        assert_eq!(receipt.action_mode.as_deref(), Some("manual-action"));
        assert_eq!(receipt.sending_mode.as_deref(), Some("MDN-sent-manually"));
        assert_eq!(
            receipt.summary_banner(),
            "Read receipt from the recipient's mail client"
        );
        let detail = receipt.detail_summary(None);
        assert!(detail.contains("maya@northstar.example"), "{detail}");
        assert!(detail.contains("sent manually"), "{detail}");
        assert!(detail.contains("Mozilla Thunderbird 128.0"), "{detail}");
    }

    #[test]
    fn handles_folded_lines_case_insensitivity_and_non_displayed_dispositions() {
        let mdn = concat!(
            "reporting-ua: outlook.office365.com;\r\n",
            "  Microsoft Outlook 16.0\r\n",
            "original-recipient: rfc822; <alex@studio.example>\r\n",
            "original-message-id: bare-id@overlook.lastrefuge.ai\r\n",
            "disposition: automatic-action/MDN-sent-automatically; deleted/error\r\n",
        );

        let receipt = parse_mdn(mdn).expect("folded MDN parses");
        assert!(!receipt.is_displayed());
        assert_eq!(receipt.disposition_type, ReadReceiptDisposition::Deleted);
        assert_eq!(
            receipt.original_message_id.as_deref(),
            Some("<bare-id@overlook.lastrefuge.ai>")
        );
        assert_eq!(
            receipt.final_recipient.as_deref(),
            Some("alex@studio.example")
        );
        assert_eq!(
            receipt.summary_banner(),
            "Deletion receipt from the recipient's mail client"
        );
        let detail = receipt.detail_summary(None);
        assert!(detail.contains("deleted without being displayed"), "{detail}");
        assert!(detail.contains("sent automatically"), "{detail}");
        assert!(detail.contains("Microsoft Outlook 16.0"), "{detail}");
    }

    #[test]
    fn rejects_non_mdn_or_missing_disposition() {
        assert!(parse_mdn("").is_none());
        assert!(parse_mdn("Final-Recipient: rfc822; a@b.example\r\nStatus: 5.1.1\r\n").is_none());
        assert!(parse_mdn("Disposition: manual-action/MDN-sent-manually;\r\n").is_none());
    }
}
