# Yahoo Mail and AOL Mail

Written 13 November 2026 for release target 1.6.22. This is step 1 of priority 3 in the [mail service provider roadmap](mail-provider-roadmap.md): guided app-password setup for Yahoo Mail and AOL Mail, on the guide that [iCloud Mail](icloud-mail.md) introduced. Step 2, signing in with Yahoo through OAuth, depends on what Yahoo allows new apps and has not started.

Yahoo and AOL run on the same platform. Both offer IMAP, POP3 and SMTP to mail apps with an app password, the full address as the user name, and implicit TLS. The work was setup and wording, not protocol.

## What the services document

| | Yahoo Mail | AOL Mail |
|---|---|---|
| IMAP | `imap.mail.yahoo.com:993`, SSL | `imap.aol.com:993`, SSL |
| POP3 | `pop.mail.yahoo.com:995`, SSL | `pop.aol.com:995`, SSL |
| SMTP | `smtp.mail.yahoo.com:465` (or 587), SSL | `smtp.aol.com:465` (or 587), SSL |
| User name | The full address | The full address |
| App password | Account Security → External connections → **Create app password**, at `login.yahoo.com/account/security` | Account Security → **Generate app password**, at `login.aol.com/account/security`, after turning on two-step verification |

Sources:
- **Yahoo:** its help pages "IMAP server settings for Yahoo Mail" ([SLN4075](https://help.yahoo.com/kb/SLN4075.html)) and "Generate and manage 3rd-party app passwords" ([SLN15241](https://help.yahoo.com/kb/SLN15241.html)), read November 2026.
- **AOL:** its help article on app passwords didn't render without a browser. The AOL column follows third-party guides that agree with each other, and with Yahoo's platform.

Yahoo's help adds three things that matter for setup:
- App passwords are offered only in "a browser that you've used to sign into Yahoo for several days in a row", not in private windows, and customer care can't override this.
- App passwords "remain active, even if you change your main account password". Only deleting one stops it.
- The page doesn't say two-step verification is required for Yahoo. Guides say AOL requires it.

Guides for AOL also say to enter the app password without the spaces it may be shown with.

## What exists

| Area | Where | What it does |
|---|---|---|
| Guides | `crates/overlook-provider/src/app_passwords.rs` | `YAHOO` and `AOL` beside `ICLOUD` in `GUIDES`. Guides now take the protocol (`servers(ServerProtocol, email)`), because Yahoo and AOL offer POP3; they carry a `short_name` for menus and `OVERLOOK_SIGN_IN`, and a `strip_spaces` flag. |
| Domains | same | Yahoo: yahoo.com, ymail.com, rocketmail.com and the main regional Yahoo domains. Yahoo! JAPAN (`yahoo.co.jp`) is a separate service and is excluded. AOL: aol.com, aim.com, its regional domains, and AOL's vanity domains (love.com, ygm.com, games.com, wow.com). Custom domains are recognised by the `mail.yahoo.com` and `aol.com` servers. |
| Presets | `crates/overlook-provider/src/autoconfig.rs` | `preset()` and `preset_pop3()` take guided services from the guides; the old Yahoo and AOL table rows are gone. |
| Setup dialog | `crates/overlook-app/src/ui/account_setup.rs` | Each service's explanation (Yahoo's includes the browser rule), a **Create an app password** button, the "App password" field, its server settings for IMAP or POP3, spaces removed from the typed password, and refusal hints. A password that isn't 16 letters is flagged when sign-in is refused, without blocking anything. |
| Entry points | sidebar account menu, Settings → Accounts → Add account, empty window | Generated from `GUIDES`: **Add iCloud account…**, **Add Yahoo account…**, **Add AOL account…**, and `OVERLOOK_SIGN_IN=icloud`, `yahoo` or `aol`. A Yahoo or AOL address in the plain IMAP or POP3 dialog gets the same guidance. |

Overlook doesn't send the IMAP `ID` command. Yahoo's developer documentation asked clients to identify themselves "to gather statistics and facilitate troubleshooting"; that page now returns 404. The old community claim that Yahoo requires an `ID` before login is about 2010-era servers, and nothing current suggests it's needed. Revisit this if a real account misbehaves.

## Tests

- `app_passwords::tests`: Yahoo and AOL settings for IMAP and POP3 and their user names, finding guides by address or server (and not Yahoo! JAPAN or Verizon), the menu order, one user name form, the 16-letter shape with or without spaces, and which services strip spaces.
- `autoconfig::tests::parses_ispdb_xml`: Yahoo's POP3 and AOL's IMAP presets come from the guides.

Live, 13 November 2026: the user added a real Yahoo account with an app password through the guided dialog and confirmed that it works. AOL is checked by build and tests only. The detailed checks below were not recorded.

## Qualification still to do

Separately for Yahoo and for AOL, per the roadmap:

1. Creating an app password, including Yahoo's browser rule and AOL's two-step verification, and pasting it with spaces.
2. IMAP sync and the folder mapping (Sent, Trash, Bulk/Spam, Archive, Drafts), IDLE, and draft sync.
3. POP3 download and retention.
4. Sending, and whether the server files its own Sent copy (Overlook appends one).
5. Deleting the app password: Overlook should show **Update password** and stop retrying.

## Not covered

- **Sign in with Yahoo (OAuth):** step 2 would reuse the Microsoft OAuth and XOAUTH2 code. It depends on registering with Yahoo and on whether Yahoo grants new apps mail access; its developer page for IMAP and SMTP over OAuth now returns 404, so this needs fresh research.
- **ISP addresses on these platforms:** Verizon (on AOL's servers), AT&T and others. The roadmap asks for each to be qualified on its own rather than assumed.
- **Yahoo! JAPAN, and Yahoo business mail with custom domains,** which use other servers.
