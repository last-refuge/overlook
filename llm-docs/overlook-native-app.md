# Overlook Native App: Build, Run, Test

Status as of 15 October 2026: foundation milestones 2 to 5 implemented (see the [foundation plan](foundation-implementation-plan.md)), plus the three daily-use features the user asked for: Focused Inbox, attachments, and Google Calendar, and the IMAP/SMTP and POP3 providers through milestone M5 of the [IMAP and POP3 plan](imap-pop3-implementation-plan.md) (verified against scripted servers and, on screen, in the app; no real IMAP, SMTP or POP3 account has been synced yet). This is a native Rust + GPUI application with a durable local store, a sample provider, and working Gmail accounts (sign-in, download, incremental sync, mailbox actions, send).

Calendar update, 23 October 2026: existing IMAP/POP accounts can attach Fastmail or a manually configured CalDAV service through **Settings → Accounts → Calendars**. New Fastmail accounts can opt in during setup. Personal creation/editing/deletion, offline changes, explicit conflicts and supported recurrence edits use the shared calendar views. Unchanged calendars now reuse cached data through collection sync tokens; Settings includes a persistent default writable calendar. The final workspace suite, independent Radicale round trips, DST checks, Clippy and release build passed. Hosted Fastmail setup and online edits were observed previously; fresh native visual/accessibility and broader hosted qualification remain incomplete. Meeting invitations/RSVP, new recurrence rules and series-wide editing remain disabled. See [current completion and verification](fastmail-calendar-completion.md), [write behavior](fastmail-calendar-write-sync.md), [connection instructions](fastmail-calendar-read-sync.md) and the [provider roadmap](calendar-provider-roadmap.md).

## Install

End users: `curl -fsSL https://raw.githubusercontent.com/last-refuge/overlook/main/install.sh | bash` (see the README). Developers: read on.

## Build and run

Requirements on Arch/Omarchy: `rustup` with Rust 1.99 (pinned in `rust-toolchain.toml`), Vulkan drivers, Wayland. No GTK or WebKit packages are needed.

```bash
cargo build
```

```bash
cargo run -p overlook
```

Data lives in `~/.local/share/overlook/` (`overlook.sqlite`, `outgoing/` for attachments on queued messages, `attachments/` for downloaded ones, and `raw/<account>/` holding every POP3 account's messages as `.eml` files, the only copy of that mail). Override it for experiments:

```bash
OVERLOOK_DATA_DIR=/tmp/overlook-demo cargo run -p overlook
```

On first launch Overlook seeds a **Sample account** (“Jason Alexander <jason@example.com>”) with the fictional mailbox from the design preview. The account is labelled as a sample everywhere; its connection is simulated and starts **disconnected**.

## Focused Inbox

A local view, not a Gmail feature. `overlook_core::focus::classify_focus` puts a conversation in Focused when the user said so (per-message override or per-sender rule), when the sender is someone the user has written to, or when Gmail marks the mail personal or important; promotions, social, forums, and updates go to Other, as do obvious automated senders when Gmail gives no hint. The Inbox shows Focused/Other tabs with a filter menu; the reader toolbar offers Move to Other/Focused with an "always from this sender" rule. Toggle in the layout menu. Schema v2 stores category, importance, and overrides.

## Attachments

Cards in the reader download on click (Gmail attachments endpoint), open with the system handler, and offer Save as… through a file dialog. Files live under `<data dir>/attachments/<account>/<message>/`. Outgoing attachments are copied into `<data dir>/outgoing/` and sent as MIME parts.

## Calendar

Google Calendar on the same OAuth client, with the `calendar.events` and `calendar.readonly` scopes (added 12 Oct 2026; existing accounts must sign in again, and the calendar banner says so while mail keeps working). Calendars and events live in schema v3/v4 tables; the first download covers 60 days back and a year ahead per visible calendar with `singleEvents=true`, then incremental changes through Google sync tokens (a 410 restarts the window). Week, Day, and Agenda views; event details with Join, Open in Google Calendar, guests and their responses; Accept/Tentative/Decline for invitations (PATCH of the attendee list, `sendUpdates=all`); New event with a client-generated base32hex id so retries never duplicate; delete for events you own. Working-location entries show in the day header, not as events. Month view shows up to three events per day with "+N more"; a day number opens Day view. Events you own can be edited (title, date, times, all-day, location, description) and deleted. Reminders: Google's popup offsets per event or the calendar default, falling back to 10 minutes when Google reports none; due reminders raise a desktop notification (freedesktop, via `notify-rust`) and an in-app toast, checked every 30 s, once per event and offset per session; toggle "Event reminders" in the layout menu. Event-specific Google colors (`colorId`) override the calendar color. Mail messages carrying a `text/calendar` part show an invitation card in the reader with the event's time and location, Accept/Tentative/Decline wired to the same response path, and "Show in calendar"; cancellations are labelled. All calendar writes go through the pending-operation queue and the shared sync engine; calendar failures never block mail.

## Releases and updates

- `.github/workflows/ci.yml` runs `cargo test --workspace` in an Arch container on every push and pull request.
- `.github/workflows/release.yml` runs on tags `v*`: checks the tag matches `Cargo.toml`, builds the release binary, builds the pacman package from `packaging/PKGBUILD` (reusing the binary), builds the generic tarball with `scripts/make-release-tarball.sh`, writes `SHA256SUMS`, attaches `install.sh`, and publishes a GitHub release with generated notes. Releasing is therefore: bump `version` in `Cargo.toml`, commit, `git tag vX.Y.Z`, `git push --tags`.
- `install.sh` downloads the matching release asset when one exists (pacman package on Arch, tarball elsewhere), verifies it against `SHA256SUMS`, and installs it; `--source` forces a build.
- In-app updater (`crates/overlook-app/src/updater.rs`): checks `releases/latest` about 20 s after launch and then daily (preference `check_for_updates`, Settings → About, with a manual "Check for updates" button). A newer version shows a banner with What's new, Update now, and Later (dismissal is remembered per version). Update now downloads the asset for the detected installation, verifies its SHA-256, and applies it: pacman installs go through `pkexec pacman -U` (polkit dialog) with a terminal + `sudo` fallback when no agent answers; prefix installs replace the binary atomically and refresh the desktop entry and icons; development builds are told to pull and rebuild. Then Restart Overlook re-executes the binary.

## Settings

`Ctrl ,` or the gear in the header opens a Settings dialog (an overlay with a page list). Every control writes immediately and is applied without restart. Preferences live in `overlook_core::prefs` (`Prefs` per profile under the `prefs` setting, `AccountPrefs` per account under `account_prefs:<id>`).

- **Accounts**: account list, an "Add account" menu (Gmail sign-in, IMAP, POP3), your name (the sender name on outgoing mail), status with Sign in again (Gmail) or Update password (IMAP, POP3) / Sync now, **Server settings…** for IMAP and POP3 accounts, **signature** (plain text) with "include in new messages" and "include in replies and forwards" toggles, remove account from this device.
- **IMAP accounts** (`crates/overlook-app/src/ui/account_setup.rs`): step 1 asks for name, address and password; Continue looks the domain up (built-in presets for the common providers, then Thunderbird's ISPDB, then `mail.<domain>`) and fills step 2, where host / port / security (SSL/TLS, STARTTLS, none for localhost only) and user name for IMAP and SMTP can be changed, "use the same password for sending" toggled, and **Test connection** probes both servers off the UI thread and shows the server's own error text. Saving writes the account, its server settings (store) and passwords (keyring), builds the provider and starts the first download. Folders appear as labels (Inbox, Sent, Trash, Spam, Archive by SPECIAL-USE or name, or by the **Folders on the server** overrides in step 2: Archive, Sent and Trash folder paths, and "Delete permanently when the server has no Trash folder"; everything else as a user label); mail is read over IMAP with the sync interval from General, and a second connection per account sits in `IDLE` on the Inbox so new mail triggers a sync within seconds (servers without `IDLE` are polled; the thread reconnects with backoff and stops on a refused password). On servers with `CONDSTORE`/`QRESYNC` a quiet change cycle is one `EXAMINE` per folder; otherwise flags and deletions are diffed with `UID FETCH (FLAGS)` and `UID SEARCH ALL`. The folder list is re-read hourly. Mailbox changes go to the server on the next sync: read and star are `UID STORE` flags; archive, delete, restore and label changes are moves between folders (`UID MOVE`, or copy + expunge on servers without it), and the Archive folder is created the first time it is needed. Sending goes over SMTP (the outgoing password from the keyring, or the incoming one when "use the same password" was chosen) and the message is appended to the server's Sent folder; when the connection drops after the message left, the Outbox item reads "Delivery uncertain" and offers **Send again** and **Discard** instead of resending on its own. `OVERLOOK_SIGN_IN=imap` opens the dialog at launch.
- **POP3 accounts** (same dialog, `Add account → POP3…`, `OVERLOOK_SIGN_IN=pop3` at launch): the lookup asks the ISPDB for the `pop3` server (and has its own preset table for the providers that still offer POP3; `mail.<domain>` on port 995 otherwise), step 2 is labelled "Incoming (POP3)" and adds an **On the server** section: leave messages on the server after downloading (default on), delete from the server after N days (empty: never), and delete from the server when a message is moved to Trash. Test connection probes POP3 (sign in and `QUIT`) and SMTP. A POP3 account is download-and-keep: labels are local (Inbox, Sent, Archive, Trash, no server folders), every mailbox change completes on this device, and each sync is one POP3 session (`UIDL` diffed against the ledger of every UIDL ever seen, `TOP` for messages over 1 MB so they list before their body arrives, `RETR` through the download backlog, retention `DELE`s committed by `QUIT`). Downloaded bytes are written to `raw/<account>/<message id>.eml` before anything else and are the account's primary record: View source and attachments come from that file, a message removed from the server stays here, and the server copy is only ever deleted after its `.eml` is on disk. Sending goes over SMTP; the Sent copy is local (no server Sent folder), and an uncertain send stays in the Outbox with Send again / Discard because there is nothing to reconcile against. The "This device holds the only copy" line in Settings → Accounts and **Back up mail…** in Settings → About are the reminders that POP3 mail needs backing up. The UIDL ledger forgets a UIDL 30 days after the server stopped listing it (and caps the gone entries at 10 000), so it stays bounded by the maildrop.
- **General**: theme, density, reading pane, start-up module (Mail / Calendar / last used), 12/24-hour clock, week start, new-mail notifications (off / Focused only / all), event reminders, sync interval (30 s to 15 min).
- **Mail**: Focused Inbox, sender pictures, preview text, mark as read (when selected / after 3 s / never), remote images (block / always show), default reply (reply / reply all), quote original in replies, attachment save folder, Inbox messages to download (100 / 300 / 1,000; changing it re-lists the mailboxes and queues what is missing).
- **Calendar**: working hours (shaded outside in Day/Week), default event length, default reminder, show declined events, event reminders.
- **About**: version, links, data locations, keyboard reference. **Back up mail…** asks for a folder and copies `overlook.sqlite` (a consistent `VACUUM INTO` copy), `raw/` and `attachments/` into it off the UI thread; the copy opens as a profile of its own (`OVERLOOK_DATA_DIR=<that folder>`), which is the restore path for now.

Signatures are appended with a blank line when composing, and above the quoted original in replies. Not yet: multiple signatures per account, automatic replies (needs the `gmail.settings.basic` scope), rules and sweep, and rich-text signatures.

## Search

Typing in the search field runs the local FTS5 query at once and, after 450 ms without further typing,
asks the provider too (`MailProvider::search`: Gmail's `users.messages.list?q=`; for IMAP a `UID SEARCH TEXT`
per word in every folder except Spam and Trash, Inbox first, with `CHARSET UTF-8` literals for non-ASCII words;
POP3 has nothing to ask). Up to 25 matches that are not on this device are downloaded, threaded and merged; the
line above the results says what was covered: "Searching N downloaded messages on this device · 25 more from
Gmail" (or "… from imap.example.com", "Searching Gmail…", "Offline, so Gmail was not searched", or the failure).
A generation counter drops results of a superseded query.

## Reader

The conversation header has Expand all / Collapse all when there is more than one message. An expanded
message has a ⋯ menu: Reply, Reply all, Forward (this message, not the latest), Mark as read/unread, Print
(a standalone HTML page in `~/.cache/overlook/print/` opened in the browser, which prints), Save as .eml and
View source (both from `MailProvider::fetch_raw`, Gmail's `format=raw`, with a rebuilt message as fallback).
Clicking an expanded message's header collapses it, except the latest.

Image attachments (`Attachment::is_image`) are downloaded with the open conversation (up to 4 MB each) and
shown as thumbnails on their cards; clicking one opens a quick look over the window with Open, Save as and
Esc. Other attachments open with the system application as before.

Data tables survive the HTML flattening: `sanitize::is_data_table` keeps a table that has header cells or is
a consistent grid of 2 to 8 short columns without nested tables or images, and the view lays it out as a
grid (the vendored parser now keeps empty cells so columns stay aligned). Layout tables are still flattened.

## Composer

The body is Markdown-flavoured text edited in the kit's code-editor state (`EditorState`, language
`mail`, line numbers and indent guides off, soft wrap on, Inter instead of the editor's monospace
default). `ui/mail_highlight.rs` is an `InputHighlighter` that rescans the text on every edit and
styles it live: bold, italic, strikethrough, inline code, link text and URLs, headings, quotes and
list markers, with the marks themselves faded. The toolbar and shortcuts (Ctrl+B/I/K, Ctrl+Shift+X
strike, Ctrl+Shift+C code, Ctrl+Shift+8/7/9 bullets/numbers/quote, Ctrl+Shift+P preview) edit the
text through `format_wrap`, `format_link` and `format_lines` in `ui/composer.rs`; Enter inside a
list continues it and Enter on an empty item ends it. Preview swaps the editor for `TextView::html`
of the rendered body.

`overlook_core::compose` turns the text into the two parts of the message: `render_markdown`
(the `markdown` crate, GFM, raw HTML escaped, single newlines as `<br>`) and `markdown_to_plain`.
`outgoing_bodies` appends the quoted original when `Draft.include_quote` is set (schema v7):
replies get an attribution line and a blockquote of the original's HTML (`sanitize::clean_only`,
no pre-pass, so the recipient gets the whole chain), forwards a header block and the original in
full. The quoted text is no longer part of `body_text`; the composer shows it collapsed under the
editor with Show and Remove. `mime.rs` sends `multipart/alternative` (text + HTML), inside
`multipart/mixed` when there are attachments; the local Sent copy and the Outbox preview use the
same bodies. Drafts save `body_html` on every autosave.

Not yet: pasting rich text (paste is plain), inline images in the composer, tables, font or
colour choices (by design), and underline, which Markdown has no mark for.

## Contacts, signatures, automatic replies, mailto

- **Contacts.** A module of its own (Ctrl+3, `ui/contacts.rs`): list with search and three filters, a
  detail pane, New contact (Ctrl+N inside the module), Edit and Delete for address-book entries, New message
  and "Mail from them". The model is `Contact` (id = People API resource name or `local:<uuid>`, primary
  `email` plus `emails`, given/family name, phone, organization, title, notes, photo, `etag`, `pending`) in the
  `people` table (schema v10, migrated from the v9 `contacts` table). Edits go through the operation log
  (`CreateContact`, `UpdateContact`, `DeleteContact`) and `MailProvider::{create,update,delete}_contact`;
  Google's `people:createContact`, `:updateContact` (etag refetched when stale) and `:deleteContact`. The
  daily `fetch_contacts` (`people/me/connections` + `otherContacts`) keeps rows with pending local edits.
  Scopes: `contacts` (read-write) and `contacts.other.readonly`; without the write scope the sidebar says
  changes stay on this device and offers Sign in again. The composer suggests address-book matches first,
  and a sender card in the reader opens or adds the person in Contacts.
- **Signatures.** `AccountPrefs.signatures` is a list of named Markdown-flavoured signatures with
  `default_new` / `default_reply` ids (`None` = no signature); `normalized()` migrates the old single
  `signature` field and the two include switches. Settings → Accounts manages them (add, rename, delete, pick
  defaults); the composer toolbar has a Signature picker that swaps the block at the end of the body.
- **Automatic replies.** `MailProvider::fetch_vacation` / `set_vacation` (Gmail `settings/vacation`, scope
  `gmail.settings.basic`). Settings → Accounts loads them when the page opens and saves with one button; the
  end date is stored exclusive, so the last day runs through midnight.
- **mailto:.** `overlook mailto:…` parses the link (`overlook_core::mailto`) and opens the composer. A second
  launch hands the link to the running instance over a Unix socket (`single_instance.rs`, one socket per
  profile under `$XDG_RUNTIME_DIR`) and exits, so the desktop entry's `mailto` handler never opens a second
  window.
- **Reminders** have Snooze 5 min on the in-app toast; a snoozed reminder returns once.
- **Gmail batch endpoint.** `GmailClient::batch_get` sends up to 20 `messages.get` calls in one
  multipart request to `batch/gmail/v1`; `fetch_messages` uses it for every multi-message download (first
  sync, backlog, online search). Per-part 429s surface as `RateLimited` like before; quota units are unchanged,
  round trips are twenty times fewer.
- **PDF previews** render through poppler's `pdftoppm` (`pdf_preview.rs`, cached under
  `~/.cache/overlook/pdf/` by path, size and mtime; first 40 pages at 1400 px; first-page thumbnails on cards).
  Without `pdftoppm` the card opens the system viewer and says what to install.
- **Tables**: a cell with `colspan` is followed by empty cells in the vendored parser so later columns keep
  their place, and the data-table heuristic counts spans.
- `<sup>`/`<sub>` of digits and signs become Unicode glyphs in the pre-pass (m², H₂O); other content stays inline.

## Rendering

Received HTML goes through three stages before the native view sees it, all in `overlook-core`:

1. `prepass::prepare` works on the DOM (html5ever). It removes what a browser would hide
   (`display:none`, `visibility:hidden`, zero font size, `max-height:0`, `mso-hide`, the `hidden`
   attribute, `preheader` classes, 1×1 tracking pixels, `<head>`/`<title>`/`<style>`), drops spacer
   blocks (Gmail's `<div><br></div>`, Outlook's `<p>&nbsp;</p>`), trims `<br>`s that only add blank
   lines, rewrites `<center>` to `<div>` and `<dt>/<dd>` to blocks, protects `<pre>` whitespace, turns
   bare URLs into links, and splits the quoted history of a reply (Gmail `gmail_quote`, `blockquote
   type=cite`, Yahoo, Thunderbird) into `Sanitized.quoted`, taking everything after the quote start at
   every nesting level. Forwards and Outlook's `From: / Sent: / To:` header blocks are not split: the
   header is context for notes people write inside the quoted text, and Gmail shows it too. Every
   embedded message header (Outlook's block, Gmail's `gmail_attr` line, a "Forwarded message" or
   "Original Message" marker) gets a rule in front of it, so a forwarded chain reads as separate messages;
   the reader adds the same rule when it inlines a split quote.
2. `sanitize_html` (ammonia) enforces the tag and attribute allow-list and the remote-image policy,
   then `flatten_tables` turns layout tables into block flow. Of an author's inline styling only the
   text colour is kept (`style="color:…"` reduced to that one declaration, `<font color>`), because
   colour carries meaning in annotated replies; the vendored view renders it through a `TextMark`
   colour. The reader runs `adapt_colors` so dark colours lighten on the dark theme and near-white
   ones darken on the light theme, hue preserved.
3. The view, `TextView::html` from the vendored `gpui-base` (see `vendor/README.md`), renders it.

The reader collapses `Sanitized.quoted` behind "Show quoted text" only when the quoted text repeats an
earlier message of the same conversation (`quote_repeats_thread`); otherwise it is shown inline, since a
quote the reader has never seen is content, not history. It replaces `cid:
image references with `data:` URLs built from the downloaded inline attachment (`Attachment.content_id`,
schema v6; inline images of the open conversation are fetched automatically, up to 4 MB each; a
one-time backfill re-fetches older messages with `cid:` references). Plain-text mail is converted by
`text_to_html`: paragraphs, `<br>` line breaks, nested blockquotes for `>` quoting with the markers
removed, and linked URLs.

`cargo run -p overlook-core --example sanitize_dump -- file.html` prints the sanitized output for a
saved message. The rendering review of 2026-10-12 used fourteen fixtures (Gmail reply with quoted
thread, newsletter with preheader and tables, nested lists, Outlook/Word mail, plain text with
quotes, code, inline cid image, headings and rules, data table, Unicode/RTL, dark-mode hostile
colors, hidden junk and tracking, forwarded message, long paragraphs); all render correctly. Known
remaining gaps: data tables are flattened rather than laid out as a grid; `sup`/`sub` are shown
inline; styles and colors from the mail are ignored by design.

### Repeating events, drag, guests

- A repeating occurrence carries `recurring_event_id` and `original_start`. Edit and Delete on one ask for a
  scope: this event, this and following, or all (`RecurrenceScope`, `cal_scope_prompt`). The store applies
  the edit to the local occurrences the scope covers (`apply_series_change`, `delete_event_locally_scoped`)
  and queues one operation with the scope. Google: "all" patches the master with the series' times shifted
  by the same amount; "following" ends the master's RRULE with `UNTIL` just before the occurrence and creates
  a new series from it (`recurrence_until`, `recurrence_for_split`; a COUNT is dropped because it no longer
  adds up). The next incremental sync brings every instance back from Google.
- In Day and Week, a block can be dragged to another time or day and its bottom edge dragged to change the end
  (`CalDrag`, 15-minute snapping, saved as an ordinary update of that occurrence).
- The event editor has Guests (comma-separated addresses; known guests keep their responses, the owner is
  added as organizer), Check availability (`CalendarProvider::free_busy`, Google's `freeBusy`), and Add a
  Google Meet link (`wants_conference` → `conferenceData.createRequest` with `conferenceDataVersion=1`).
  Rooms are Workspace resource calendars and are not offered; a personal Gmail account has none.

## Gmail

1. Create a Google Cloud OAuth client of type **Desktop app** with the Gmail API enabled and the scopes `gmail.modify`, `gmail.send`, `openid`, `userinfo.email` on the consent screen (Testing mode with test users is fine for development; restricted-scope verification is needed for public release).
2. Save the downloaded JSON as `~/.config/overlook/google-oauth-client.json` (owner-only permissions). It is never embedded in the binary or committed.
3. In Overlook open the account menu (chevron button at the bottom of the sidebar) → **Add Gmail account…**, or launch with `OVERLOOK_SIGN_IN=gmail`. The system browser opens Google's consent screen; Overlook listens on a loopback port with PKCE, exchanges the code, reads the user's email, and stores tokens in the system keyring (Secret Service via `oo7`). Tokens refresh automatically; a revoked grant turns into the **Sign-in required** state with a "Sign in again" action, and local work is kept.
4. The first sync lists the 150 most recent messages (ids only, cheap), then downloads full bodies in batches of 20, committing after every batch. The backlog is persisted (`settings` key `download_backlog:<account>`), so quota pauses, going offline, or a crash never lose progress. Later syncs use `users.history.list` from the saved checkpoint; an expired checkpoint triggers a full resynchronization that preserves pending local intent.
5. Mailbox actions map to `batchModify` (UNREAD/STARRED/INBOX/user labels) and `messages.trash`/`untrash`. Send builds RFC 822 with threading headers (lettre) and posts to `messages.send`; a transport failure after the request is **uncertain** and is reconciled with `rfc822msgid:` before any retry.

Gmail meters usage in quota units per user per minute. Overlook spaces requests about 170 ms apart and, when Gmail still answers with a quota error, enters the **Paused** state and backs off (60 s, growing to 5 minutes) instead of hammering the API. In a fresh project the observed budget allowed roughly 40 full-message downloads per minute, so a first download of 150 messages can take a few minutes. Check **APIs & Services → Gmail API → Quotas** in Google Cloud if downloads pause often.

Environment switches for development:

| Variable | Effect |
|---|---|
| `OVERLOOK_DATA_DIR` | Profile directory |
| `OVERLOOK_CONFIG_DIR` | Directory holding `google-oauth-client.json` (default `~/.config/overlook`) |
| `OVERLOOK_SIGN_IN=gmail` | Start the Google sign-in at launch |

Keyboard additions: `Ctrl 1` Mail, `Ctrl 2` Calendar, `Ctrl Shift A` new event, `Ctrl S` save the event editor; with the calendar focused, `↑`/`↓` select an event, `Enter` edits it, `Delete` removes it, `←`/`→` move by period, `T` goes to today, and `D`/`W`/`M`/`A` choose views. Text inputs and focused buttons retain their normal keys.
| `OVERLOOK_SAMPLE_ONLINE=1` | Start the sample connection connected, so queued work syncs at launch |
| `OVERLOOK_UTC_OFFSET_MINUTES` | Override the local time zone offset used for day groups |
| `RUST_LOG=overlook=debug` | Tracing output |

## Tests

```bash
cargo test --workspace
```

The scripted servers for manual runs live in `scripts/spike-mail-servers.py` (plaintext IMAP on 1143, POP3 on 1110, SMTP on 2525; user `spike`, password `secret`). With it running, the ignored end-to-end tests drive the real providers through the store: `cargo test -p overlook imap_exit_criteria -- --ignored --nocapture` and `cargo test -p overlook pop3_exit_criteria -- --ignored --nocapture` (fresh server process per run; the POP3 run ends with the spike's INBOX emptied by the leave-on-server-off policy). The provider crate's `real_imap_server_round_trip`, `real_imap_server_write_round_trip` (in a scratch folder it creates and deletes), `real_smtp_server_round_trip` (probe, plus one message when `OVERLOOK_SMTP_TEST_TO` is set) and `real_pop3_server_round_trip` take `OVERLOOK_IMAP_TEST_*` / `OVERLOOK_SMTP_TEST_*` / `OVERLOOK_POP3_TEST_*` (host, port, security `tls|starttls|none`, user, password) and work against the spike or a real server; `scripts/interop-matrix.sh "<name>"` runs them all and prints a row for the plan's interoperability matrix. `cargo test -p overlook imap_idle -- --ignored --nocapture` measures how fast `IDLE` reports a message appended with the spike's `XDELIVER` control command.

88 tests cover: appearance resolution (Follow system is not a third theme), recipient parsing, conversation summaries, reply/forward drafts, HTML sanitization and remote-image blocking, local time formatting, PKCE/OAuth request building, the sample provider, store durability across reopen, atomic change+operation (a failing change leaves no operation), Outbox queue/cancel, send completion, FTS search and coverage, settings, and the sync engine (offline leaves work queued; ordered replay; uncertain send reconciled without duplicates; cancelled send never sent).

The manual scenario used to validate the shell (keyboard only, with `wtype` on Hyprland):

1. Launch disconnected. Press `s` (star), `j`, `e` (archive), `Ctrl R`, type a reply, `Ctrl Enter`. Status reads “You’re offline. 5 changes will sync when you’re back online.” Outbox shows 1.
2. `kill -9` the process. Relaunch disconnected: the Outbox item and five pending changes are still there, read from SQLite.
3. Relaunch with `OVERLOOK_SAMPLE_ONLINE=1`: the queue replays in sequence, the reply is accepted by the provider, a Sent copy is stored, the draft row disappears, the toast reads “Sent: …”, and the status bar reads “Up to date · Last synced …”.

Reference captures from this run are in `branding/previews/overlook-native-*.png`.

## Workspace

| Crate | Role |
|---|---|
| `overlook-core` | Domain types (accounts, labels, folders, messages, conversations, drafts, pending operations, sync status), appearance resolution, status wording, HTML sanitizer, time helpers. No UI, no database. |
| `overlook-store` | SQLite schema v1 (`PRAGMA user_version`), WAL + `synchronous=FULL`, FTS5 index with triggers, atomic `apply_change` (local change + pending operation in one transaction), drafts/Outbox, settings, search coverage, known recipients. |
| `overlook-provider` | `MailProvider` trait, `SampleProvider` (in-process server with a connection switch and a “drop the next send” fault), Gmail OAuth/PKCE preparation (no network yet). |
| `overlook` (crates/overlook-app) | GPUI application: theme mapping, fonts, assets, sync engine, and the mail window. |

### Key behaviours

- **Local-first.** Every view reads from the store. `apply_change` writes the change and its operation together in one transaction.
- **Instant mailbox actions.** The window keeps a `MailIndex` (`overlook-store/src/index.rs`): every message of the account without bodies, plus labels, pending ids and Focused Inbox rules. The conversation list and all sidebar counts are derived from it in memory (about 1 ms for ~1,000 messages, against ~450 ms when each count re-read every message from SQLite). An action (`apply_local`) applies the change to the index and the open thread and redraws at once, then queues the store write; one background task writes queued changes in order, reads a fresh index, and starts the sync. `reload()` shows the in-memory index immediately and refreshes it on a background thread; a refresh is dropped while local writes are queued so it cannot undo a change the user just made. `MailIndex::apply` must mirror `apply_change_tx`; the store test `in_memory_index_matches_the_store_after_each_change` guards that.
- **Fast message bodies.** HTML bodies render in an isolated headless Chromium (`browser_engine.rs`) as screenshot tiles. Launching Chromium is most of the wait (~190 ms of ~270 ms to first tile), so `browser_engine::prewarm` keeps one process started and idle; a new document takes it (first tile ~100 ms) and the pool starts a replacement. A process is never reused across documents. Bodies start at the reader's real width and scale (`browser_body::reader_size`) instead of a default width plus a resize. Finished renders (the opening screenful, keyed by document hash, width and scale; eight kept) show instantly when a message is opened again. After a conversation opens, `schedule_prerender` renders the latest message of the conversations above and below it off-screen into that cache, skipping any message whose remote content would load, so nothing is fetched for unopened mail. Plain-text messages are drawn natively (`TextView`), and an HTML body shows its text until the first tiles arrive.
- **Desktop notifications** (`notify.rs`). On Omarchy (detected by `omarchy-notification-send` on `PATH` or in `/usr/share/omarchy/bin`) they go through that command with a Nerd Font glyph per kind, and clicking one runs `omarchy-launch-or-focus ai.lastrefuge.Overlook overlook`; elsewhere they use the freedesktop service. Every in-app toast goes through `ui::toast`, which also sends success, warning and error messages to Omarchy's notifications when Settings > General > Notifications > "Show in Omarchy notifications" is on (default on, shown only on Omarchy); plain information such as "Archived" stays in the app. New mail is announced (Settings "New mail": Off, Focused only, All) by comparing each fresh mail index with the previous one (`new_inbox_mail`): unread incoming Inbox messages that are new since the last index and since the window opened; one notification per message, or a summary for several. Calendar reminders use the same path.
- **Never block the UI thread.** The store has a writer connection (sync, local changes) and a read-only reader connection; every read-only `Store` method uses the reader (`with_read`), which under WAL never waits for a sync's write. Settings and attachment paths are written by `persist::later`, one background thread that applies writes in order. `watchdog.rs`: the window beats a heartbeat every 200 ms and a watcher thread logs any UI stall of a second or more, with the last step the window marked (`watchdog::mark`) and the thread's kernel state (running, or waiting on a lock, the disk, the GPU). Logs go to stderr and to `overlook.log` in the data folder (kept to about 4 MiB plus `overlook.log.old`); look there first when Hyprland reports Overlook as not responding.
- **Outbox.** Send validates recipients, moves the draft to `Queued`, and records a `SendDraft` operation in the same transaction. Queued and failed items can be edited (which cancels the send) until submission begins. Sent is reported only after the provider returns a receipt; `complete_send` stores the Sent copy, removes the draft, and marks the operation applied atomically.
- **Uncertain delivery.** If the provider reports a dropped connection after accepting a message, the draft and operation become `Uncertain`, replay stops, and the next cycle calls `reconcile_send` before any retry.
- **Sync engine** (`crates/overlook-app/src/sync.rs`) runs on a background thread: probe → `Syncing` → replay pending operations by `seq` → `UpToDate` with a timestamp, or `Offline` / `SignInRequired` / `SyncFailed(reason)`. It runs at startup when reachable, after each local change while connected, every 30 seconds, and from Sync now / F5.
- **Themes.** `theme.rs` holds Light (Soft stone, approved 23 October 2026) and Dark (matching `branding/tokens.json`) plus Gray, Blue, White, Alexander, and Pink, and projects the active palette onto the component library. Light uses stone reading/compose surfaces with charcoal text and pine accents. Its HTML defaults have zero CSS specificity and defer to sender styles and legacy body color attributes. The browser cache includes the resolved colorway, and pre-rendering uses the same themed preparation as opening a message. The preference is stored in `settings`; Follow system selects Light or Dark from the window appearance, which GPUI reads from the desktop settings portal. Switching changes display colors only, never stored messages or outgoing mail formatting.
- **Received HTML** is sanitized by `overlook_core::sanitize` (allowlisted tags and bounded inline presentation, no stylesheets/scripts/forms/frames, `http(s)`/`mailto`/`cid` only, remote images blocked by default). The native renderer preserves hard breaks, distinct flow/paragraph spacing, and inherited typography. Composer quotes and Outbox previews enforce the same resource consent. See [the formatting review and regression workflow](../docs/message-formatting.md) for supported behavior and browser-layout limitations.
- **Fonts.** Inter and Source Serif 4 Display are embedded and registered at startup; the wordmark is the outlined SVG.

### Keyboard

Window: `Ctrl N` new message, `Ctrl E` search, `Ctrl K` (or `Ctrl Shift P`) command palette: every action, mailbox and label by typing, `Move to <label>` for the open conversation, Settings pages and accounts, `Ctrl R` reply, `Ctrl Shift R` reply all, `Ctrl F` forward, `Ctrl U` toggle read, `Ctrl Enter` send (from any composer field), `Esc` save and close composer / clear search, `F5` sync.
Message list: `J`/`↓` next, `K`/`↑` previous, `E`/`Backspace` archive, `Delete` trash, `S` star, `U` toggle read, `Enter` expand latest message.
Composer header fields: `Enter` moves to the next field.

### Layout and density

The sidebar and list panes are resizable; sizes persist in `settings`. The layout menu in the header switches the reading pane between right and below, and density between comfortable and compact; both persist.

## Known gaps (tracked for the next sessions)

- IMAP, SMTP and POP3 have only ever synced against the scripted servers in `scripts/spike-mail-servers.py` and the in-crate fakes; public servers were probed up to the login exchange over TLS and STARTTLS (Fastmail, Gmail, iCloud, Microsoft 365, Yahoo, Zoho) but no real account has been read from or written to. XOAUTH2 (Microsoft 365, Outlook.com, Gmail without an app password) is not implemented. `IDLE` has not been observed across a laptop sleep. POP3: restore is manual (stop Overlook, replace the data directory with a backup); `APOP` and `SASL` logins are not offered (`USER`/`PASS` over TLS or STLS only). After an uncertain send the account reads "Offline" until the next sync. See the interoperability matrix in `llm-docs/imap-pop3-implementation-plan.md`.

- Gmail: no attachment download for received mail yet, no progressive download beyond the newest 150 messages, no `cid:` inline images, and bodies are fetched one request per message (the Gmail batch endpoint would cut request count).
- Composer is plain text. Rich text, signatures, and HTML output are phase 1 items; `body_html` is already stored separately.
- Attachment download for received mail is not implemented (cards show “Not downloaded”). Outgoing attachments are copied into the protected `outgoing/` store and persisted with the draft.
- Multi-select and drag-and-drop are not implemented.
- Hyprland draws no title bar; the kit’s window controls are not shown there. GNOME and KDE behaviour, fractional scaling, and screen readers (Orca) have not been checked yet.
- Flatpak tooling is absent on this machine; `packaging/` holds the desktop entry, AppStream metadata, and a manifest draft that still need a build.
- Omarchy applies a default window opacity (0.985 active, 0.96 inactive) to every app, including Overlook. That is compositor policy, not an app setting.
