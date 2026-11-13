# Fastmail mail setup

Written 13 November 2026 for release target 1.6.22. This is priority 4 in the [mail service provider roadmap](mail-provider-roadmap.md): guided app-password setup for Fastmail mail, as an entry in the app-password guide that [iCloud Mail](icloud-mail.md) introduced and [Yahoo and AOL](yahoo-aol-mail.md) extended. Fastmail calendars have their own documents, starting with [Fastmail onboarding and workflows](fastmail-onboarding-and-workflows.md); nothing here changes them.

As the roadmap asks, mail stays on IMAP and SMTP. A JMAP adapter can be evaluated once this experience is dependable.

## What Fastmail documents

Read from Fastmail's help in November 2026: "App passwords" ([360058752854](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords)), "Server names and ports" ([1500000278342](https://www.fastmail.help/hc/en-us/articles/1500000278342-Server-names-and-ports)) and "Choosing your data residency" ([16796454162063](https://www.fastmail.help/hc/en-us/articles/16796454162063-Choosing-your-data-residency)).

| | Server | Port | Security |
|---|---|---|---|
| IMAP | `imap.fastmail.com` | 993 | SSL/TLS, "but not STARTTLS" |
| POP3 | `pop.fastmail.com` | 995 | SSL/TLS, not STARTTLS |
| SMTP | `smtp.fastmail.com` | 465 (587 with STARTTLS as the fallback) | SSL/TLS |

- **User name:** "Your Fastmail email address, including the domain". You "must use the username you used to sign up your account", custom domains included.
- **App passwords:** "Every third-party program or app needs its own app password." They are created under Settings → Privacy & Security → Manage app passwords and access → **New app password**. The default access, Mail, Contacts & Calendars, covers IMAP/POP/SMTP, CardDAV and CalDAV. A password is 16 random letters and digits, without 0, 1, O or I; Fastmail says spaces and capitalisation don't matter. It is shown once, and can be disabled or removed later.
- **Plans:** "Basic plans do not include access to IMAP, SMTP, CalDAV, or CardDAV", and Basic users can't create app passwords.
- **Data residency:** US and EU accounts need no changes to their mail apps. Regional host names (`eu-imap.fastmail.com`, `us-imap.fastmail.com`, and so on) are optional.

## What exists

| Area | Where | What it does |
|---|---|---|
| Guide | `crates/overlook-provider/src/app_passwords.rs` | `FASTMAIL` in `GUIDES`. It has the servers above for IMAP or POP3, and recognises Fastmail's main domains and, for custom domains, its `fastmail.com` and `messagingengine.com` servers (regional names included). The explanation names the default access that also covers calendars, and says Basic plans can't use mail apps. Spaces are stripped, and the shape check is 16 letters and digits. |
| Presets | `crates/overlook-provider/src/autoconfig.rs` | The Fastmail rows of the preset tables gave way to the guide. |
| Setup dialog | `crates/overlook-app/src/ui/account_setup.rs` | **Add Fastmail account…** and `OVERLOOK_SIGN_IN=fastmail` come from the guide list. The other change applies to every guided service: an address at a domain no guide lists, whose looked-up settings point at a guided service's servers, gets that service's guidance. That matters for Fastmail's many address domains, which Thunderbird's database knows. The **Create an app password** button now also shows on the Servers step, because guidance found this way arrives after the first step. |
| Calendars | unchanged | The setup dialog's existing **Connect calendars using this mail password** switch works with an app password that has the default access. |

The refusal hint covers the two usual causes together: an account password instead of an app password, and a Basic plan. Fastmail's refusal doesn't say which it is.

## Tests

- `app_passwords::tests::fastmail_follows_its_help_pages`: servers for IMAP and POP3, the user name, domain and host recognition (including `eu-imap.fastmail.com` and `messagingengine.com`), the password shape with and without spaces, and space stripping.
- `autoconfig::tests::presets_and_guesses`: the Fastmail preset, now from the guide, still gives `imap.fastmail.com` with the full address and SMTP on 465 with TLS.

Live, 13 November 2026: the user set up a real Fastmail account with an app password through the guided dialog and confirmed that it works. The detailed checks below were not recorded.

## Qualification still to do

1. Mail setup with an app password, including one pasted with spaces. Then the folder mapping, IDLE, draft sync, and sending with one Sent copy.
2. A custom-domain address, and one at one of Fastmail's other domains (through the settings lookup).
3. Calendars connected from the same dialog with the default-access app password.
4. A Basic-plan account, to see Fastmail's actual refusal.

The earlier Fastmail observations are in [Fastmail live qualification](fastmail-live-qualification.md).
