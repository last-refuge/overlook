# Provider marks

These are the marks on the welcome screen's account choices, embedded by `crates/overlook-app/src/assets.rs` under `overlook/providers/`. The service names and logos are trademarks of their owners. They appear only to identify the services Overlook connects to, and imply no endorsement. The owner chose this approach on 13 November 2026: official artwork where a company offers it, Simple Icons where it doesn't, and Overlook-made placeholders otherwise.

| File | Mark | Source |
|---|---|---|
| `google-g.png` | Google "G" | Google's "Sign in with Google" branding kit (<https://developers.google.com/identity/branding-guidelines>), `signin-assets.zip` downloaded 13 November 2026. It is the centre 96 × 96 pixels, unaltered, of `Android + Web/PNG @4x/Light/Theme=Light, Show text=No, Shape=Square, Platform=Android+Web@4x.png`. The kit's SVG builds the gradient with `foreignObject`, which SVG renderers skip, so the PNG is the faithful copy. Shown on white beside "Sign in with Google". |
| `microsoft.svg` | Microsoft logo | Microsoft's "Sign in with Microsoft" branding guidance (<https://learn.microsoft.com/en-us/entra/identity-platform/howto-add-branding-in-apps>), `ms-symbollockup_mssymbol_19.svg`, unaltered. |
| `icloud.svg` | iCloud | Simple Icons 13.21.0 (CC0-1.0), filled with its listed brand colour `#3693F3`. |
| `aol.svg` | AOL | Simple Icons 13.21.0 (CC0-1.0), filled with its listed brand colour `#3399FF`. |
| `proton-mail.svg` | Proton Mail | Simple Icons 13.21.0 (CC0-1.0), filled with its listed brand colour `#6D4AFF`. |
| `yahoo.svg` | Placeholder | Drawn for Overlook: a plain "Y" in Yahoo purple. It is **not** Yahoo's logo; Yahoo withdrew its marks from Simple Icons. Replace it with official artwork. |
| `fastmail.svg` | Placeholder | Drawn for Overlook: a plain "F" in a Fastmail blue. It is **not** Fastmail's logo, for the same reason. Replace it with official artwork. |

The IMAP and POP3 choices use Overlook's own colour icons, `branding/icons/imap.svg` and `pop3.svg`, which follow the theme like the other action icons.

To replace a mark, keep the file name. Use an SVG or PNG that reads at 24 to 32 pixels on a white plate, and update this table.
