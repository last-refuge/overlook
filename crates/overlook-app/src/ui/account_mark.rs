//! Which service an account is with, pictured: the service's own logo where Overlook has
//! one (`branding/providers/`, the same marks as the welcome screen), Overlook's IMAP or
//! POP3 icon for any other server, and Overlook's own mark for the sample account.

use crate::assets::{color_icon, ColorIcon};
use crate::theme::Palette;
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::servers::{MailServers, OAuthIssuer};
use overlook_core::{Account, ProviderKind};
use overlook_provider::app_passwords;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AccountMark {
    /// A service's logo on a white plate, with the optical scale the welcome screen gives
    /// it: a full square like Microsoft's reads larger than a glyph at the same size.
    Logo(&'static str, f32),
    /// One of Overlook's colour icons, for a server without a known service.
    Own(ColorIcon),
    /// The sample account, which Overlook serves itself.
    Overlook,
}

const GOOGLE: AccountMark = AccountMark::Logo("overlook/providers/google-g.png", 1.);
const MICROSOFT: AccountMark = AccountMark::Logo("overlook/providers/microsoft.svg", 0.8);

impl AccountMark {
    /// The mark for `account`. `servers` are its server settings, for IMAP and POP3
    /// accounts: they say whether it signs in with Microsoft and which host it uses.
    pub(crate) fn of(account: &Account, servers: Option<&MailServers>) -> Self {
        match account.provider {
            ProviderKind::Sample => Self::Overlook,
            ProviderKind::Gmail => GOOGLE,
            ProviderKind::Imap | ProviderKind::Pop3 => {
                if servers.is_some_and(|s| s.oauth == Some(OAuthIssuer::Microsoft)) {
                    return MICROSOFT;
                }
                let host = servers.map(|s| s.incoming.host.as_str());
                let domain = account.email.rsplit('@').next().unwrap_or("").trim().to_ascii_lowercase();
                let google_host = host.is_some_and(|h| {
                    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
                    h == "imap.gmail.com" || h == "pop.gmail.com" || h.ends_with(".googlemail.com")
                });
                if google_host || matches!(domain.as_str(), "gmail.com" | "googlemail.com") {
                    return GOOGLE;
                }
                match app_passwords::guide_for(&account.email, host).map(|g| g.short_name) {
                    Some("iCloud") => Self::Logo("overlook/providers/icloud.svg", 1.),
                    Some("Yahoo") => Self::Logo("overlook/providers/yahoo.svg", 0.9),
                    Some("AOL") => Self::Logo("overlook/providers/aol.svg", 1.04),
                    Some("Fastmail") => Self::Logo("overlook/providers/fastmail.svg", 0.9),
                    Some("Proton") => Self::Logo("overlook/providers/proton-mail.svg", 0.9),
                    _ if account.provider == ProviderKind::Pop3 => Self::Own(ColorIcon::Pop3),
                    _ => Self::Own(ColorIcon::Imap),
                }
            }
        }
    }

    /// The mark in a circle `size` across. Logos sit on white in every theme, as the
    /// services draw them; Overlook's icons sit on the theme's quiet chip.
    pub(crate) fn render(self, size: f32, p: &Palette) -> AnyElement {
        let plate = div()
            .flex_none()
            .size(px(size))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden();
        match self {
            Self::Logo(path, scale) => plate
                .bg(white())
                .border_1()
                .border_color(p.divider)
                .child(img(path).size(px((size * 0.62 * scale).round())).flex_none())
                .into_any_element(),
            Self::Own(icon) => plate.bg(p.chip).child(color_icon(icon, (size * 0.72).round(), p)).into_any_element(),
            Self::Overlook => super::brand_tile(size).into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::servers::{Security, ServerEndpoint};
    use overlook_core::AccountId;
    // The GPUI glob above also exports a `test` proc macro.
    use std::prelude::v1::test;

    fn account(provider: ProviderKind, email: &str) -> Account {
        Account { id: AccountId::from("a"), provider, display_name: String::new(), email: email.into(), created_at: 0 }
    }

    fn servers(host: &str, oauth: Option<OAuthIssuer>) -> MailServers {
        let endpoint = |host: &str| ServerEndpoint::new(host, 993, Security::Tls, "u");
        MailServers { incoming: endpoint(host), outgoing: endpoint("smtp.example.org"), pop3: None, imap: None, oauth }
    }

    #[test]
    fn services_get_their_own_logos() {
        assert_eq!(AccountMark::of(&account(ProviderKind::Sample, "a@example.com"), None), AccountMark::Overlook);
        assert_eq!(AccountMark::of(&account(ProviderKind::Gmail, "a@example.com"), None), GOOGLE);
        let work = account(ProviderKind::Imap, "a@contoso.example");
        assert_eq!(AccountMark::of(&work, Some(&servers("outlook.office365.com", Some(OAuthIssuer::Microsoft)))), MICROSOFT);
        let icloud = AccountMark::of(&account(ProviderKind::Imap, "a@icloud.com"), Some(&servers("imap.mail.me.com", None)));
        assert_eq!(icloud, AccountMark::Logo("overlook/providers/icloud.svg", 1.));
        // An address at its own domain is known by the server it uses.
        let gmail_imap = AccountMark::of(&account(ProviderKind::Imap, "a@studio.example"), Some(&servers("imap.gmail.com", None)));
        assert_eq!(gmail_imap, GOOGLE);
    }

    #[test]
    fn other_servers_get_overlooks_icons() {
        let host = servers("mail.studio.example", None);
        assert_eq!(AccountMark::of(&account(ProviderKind::Imap, "a@studio.example"), Some(&host)), AccountMark::Own(ColorIcon::Imap));
        assert_eq!(AccountMark::of(&account(ProviderKind::Pop3, "a@studio.example"), Some(&host)), AccountMark::Own(ColorIcon::Pop3));
        assert_eq!(AccountMark::of(&account(ProviderKind::Imap, "a@studio.example"), None), AccountMark::Own(ColorIcon::Imap));
    }
}
