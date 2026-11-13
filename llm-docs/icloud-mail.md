# iCloud Mail

Written 12 November 2026 for release target 1.6.22. This is milestone 1 of priority 2 in the [mail service provider roadmap](mail-provider-roadmap.md): guided setup for iCloud Mail accounts, including iCloud+ custom email domains.

iCloud Mail needs no new protocol work. Apple offers mail apps IMAP and SMTP with an app-specific password and nothing else: no OAuth for third-party clients, and no POP. The existing IMAP and SMTP providers were already enough. What was missing was setup that gets Apple's details right and explains its passwords.

## What Apple documents

From "iCloud Mail server settings for other email client apps" ([support.apple.com/102525](https://support.apple.com/en-us/102525), published 3 February 2026):

| | Server | Port | Security | User name |
|---|---|---|---|---|
| IMAP | `imap.mail.me.com` | 993 | SSL required ("if you see an error message when using SSL, try using TLS instead") | "usually the name of your iCloud Mail email address (for example, johnappleseed, not johnappleseed@icloud.com). If your email client app can't connect to iCloud Mail using just the name of your email address, try using the full address." |
| SMTP | `smtp.mail.me.com` | 587 | SSL required (TLS or STARTTLS as the fallback) | "Your full iCloud Mail email address (for example, johnappleseed@icloud.com, not johnappleseed)" |

- The password for both is an app-specific password ([support.apple.com/102654](https://support.apple.com/en-us/102654)). Creating one requires two-factor authentication on the Apple Account.
- "iCloud Mail does not support POP."
- Apple's page says nothing about custom domains, aliases or sending from another address.

Overlook's preset used the full address for IMAP before this change, against Apple's advice.

## What exists

| Area | Where | What it does |
|---|---|---|
| App-password guides | `crates/overlook-provider/src/app_passwords.rs` | `AppPasswordGuide`, a reusable description of a service that wants an app password: its domains and server hosts, the dialog's explanation, what the password is called and looks like, where to create one, what a refusal means, whether it offers POP3, whether the user name may take two forms, and its server settings. `ICLOUD` is the first entry in `GUIDES`. Yahoo and AOL followed ([yahoo-aol-mail.md](yahoo-aol-mail.md)); Fastmail and Zoho (roadmap priorities 4 and 6) are meant to be next. |
| Presets | `crates/overlook-provider/src/autoconfig.rs` | `preset()` takes a guided service's own settings first. For iCloud addresses the IMAP user name is now the name part and the SMTP user name is the full address. The old iCloud table row is gone. |
| User name fallback | `ImapProvider::probe_user_names` | Signs in with each user name form in turn and returns the one accepted. A temporary failure stops it, so only refusals lead to another try. |
| Setup dialog | `crates/overlook-app/src/ui/account_setup.rs` | Guided mode: the explanation, a **Create an app-specific password** button (`account.apple.com`), "App-specific password" as the field label with an `xxxx-xxxx-xxxx-xxxx` placeholder, Apple's settings without a lookup, the user name fallback in **Test connection**, and refusal hints. |
| Entry points | sidebar account menu, Settings → Accounts → Add account, empty window, `OVERLOOK_SIGN_IN=icloud` | **Add iCloud account…** opens the guided dialog for any address, including a custom domain. |
| Settings | `ui/settings.rs` | An iCloud account's connection line says an app-specific password is kept in the keyring. |

### How setup behaves

1. **Add iCloud account…**, or an @icloud.com, @me.com or @mac.com address in the IMAP or POP3 dialog, puts the dialog in guided mode. A POP3 setup becomes IMAP and says why.
2. **Continue** fills in Apple's settings directly. A custom-domain address keeps itself as both user names, with a note telling the person to use their iCloud Mail address instead, because Overlook can't know it.
3. **Test connection** and **Add account** try the configured IMAP user name, then the other form (name part ↔ full address, or the name at @icloud.com for a custom domain). An accepted alternative replaces the user name field, and the check counts for the settings as they then read.
4. A refused password adds iCloud's advice: Apple wants an app-specific password, not the Apple Account password, and a revoked one stops working. If the password doesn't have the shape of one (16 letters, in four hyphenated groups or none), the hint says so first. The shape check never blocks anything.
5. Reopening an iCloud account (**Server settings…**, **Update password**) keeps the guidance. Custom domains are recognised by their `mail.me.com` server.

## What iCloud's IMAP may do differently

Reported by Thunderbird's IMAP maintainer on Apple's developer forums ([thread 694251](https://developer.apple.com/forums/thread/694251)), and checked against Overlook's code:

- **`SELECT … (QRESYNC …)` returns `FETCH` responses with sequence number -1.** Overlook never sends QRESYNC parameters with `SELECT`; its fast path is `ENABLE QRESYNC` plus `UID FETCH … (CHANGEDSINCE … VANISHED)`.
- **The `ENABLE QRESYNC` reply lacked the untagged `ENABLED` line** (later reported fixed). Overlook treats QRESYNC as on only after that line, so it would fall back to CONDSTORE.
- **`LANGUAGE` is advertised but always fails.** Overlook doesn't use it.
- **LSUB mishandling** (Thunderbird bug 1817707). Subscriptions are informational in Overlook; sync never depends on them.

Not known: whether `UID FETCH … CHANGEDSINCE … VANISHED` behaves, whether IDLE works for third-party clients, and whether iCloud advertises SPECIAL-USE. The folder name rules already map iCloud's names: Sent Messages, Deleted Messages, Junk, Archive and Drafts.

Sent copies: nothing indicates that iCloud's SMTP server files its own copy. Thunderbird users configure Sent Messages as their copy folder rather than turning copies off, which suggests it doesn't. Overlook therefore appends its copy as for any IMAP server.

## Tests

- `app_passwords::tests`: Apple's settings and user names, custom domains, finding the guide by address or server host (a host that only contains `mail.me.com` doesn't match), the user name forms tried, and the app-password shape.
- `autoconfig::tests::parses_ispdb_xml`: the iCloud preset's user names, and no POP3.
- `imap::tests::the_accepted_user_name_form_is_found`: against the fake server, the full address is refused, the name is accepted, a wrong password fails both forms, and a temporary failure stops at once.

Live, 13 November 2026: the user added a real iCloud account with an app-specific password through the guided dialog and confirmed that it works. Which user name form iCloud accepted, and the detailed checks below, were not recorded.

## Qualification still to do

Beyond that first working account, per the roadmap's "What earns explicit support":

1. Setup with an app-specific password at @icloud.com and at @me.com or @mac.com. Note which IMAP user name form iCloud accepts, and the refusal for the Apple Account password.
2. Sync: the folder mapping, CONDSTORE or QRESYNC (watch the log for unreadable replies), IDLE, and draft sync.
3. Sending: one Sent copy in Sent Messages, and the sending identity.
4. An iCloud+ custom domain, signing in with the iCloud Mail address and sending as the custom address.
5. Revoking the app-specific password: Overlook should show **Update password** and stop retrying.

## Not covered

- Aliases and Hide My Email addresses as extra sending identities. Overlook sends as the account's address.
- iCloud calendars and contacts (CalDAV and CardDAV at `caldav.icloud.com`), which belong to the [calendar provider roadmap](calendar-provider-roadmap.md).
