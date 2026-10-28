# Overlook session handoff

Continue the Overlook project from the context below. Read the saved documents and inspect the current workspace before making changes. Preserve the agreed product and brand direction.

## Workspace and instructions

Project root: `/home/jason/coding/omarchy-overlook`

Read any applicable `AGENTS.md` instructions and `/home/jason/.codex/RTK.md`. Keep written project documentation in `llm-docs/`, branding assets and previews in `branding/`, application code in `crates/`, packaging files in `packaging/`, and helper scripts in `scripts/`.

Do not assume this session's running processes, scratch profiles, or tool handles still exist. Inspect the environment and version-control state before starting. The repository was initialised with `git init` on 12 October 2026 and **has no commits yet**; the user decides when to commit.

## Read these first

1. `llm-docs/README.md` — documentation index.
2. `llm-docs/foundation-implementation-plan.md` — the validated technical foundation (GPUI), tradeoffs, workspace rules, milestone sequence, status log.
3. `llm-docs/overlook-native-app.md` — how to build, run, and test the app; architecture; keyboard; known gaps.
4. `llm-docs/native-linux-mail-client-plan.md` — authoritative roadmap, offline requirements, and release gates.
5. `llm-docs/overlook-brand.md` and `llm-docs/overlook-brand-assets.md` — the selected identity and asset inventory.
6. `branding/previews/overlook-native-*.png` — captures of the running native app (light, dark, offline Outbox, reconnected, compose) next to the earlier HTML design captures `overlook-mail-light.jpg` / `overlook-mail-dark.jpg`.

## Decisions made on 12 October 2026

- **UI framework is GPUI**, by the user's explicit direction ("Let's make sure we are using Rust and GPUI"). The GTK4/libadwaita/WebKitGTK proposal is withdrawn. The app depends on `gpui-kit` 0.7.3, which pins `gpui-pre` 0.3.9 as `gpui` and provides inputs, resizable panels, menus, notifications, and a native HTML text view. Do not mix `gpui` 0.2.2 into the tree (it also fails to build on Rust 1.99 because of an old `xattr`).
- **No embedded web view.** Received HTML is sanitized in `overlook-core` and rendered natively. Fidelity for layout-table email is limited (tables are flattened into rows); this is the main open technical risk and a foundation gate. See the plan document for the fallback options.
- **Sample account** seeds the fictional mailbox on first launch so the shell is local-first from the start. Its connection is a visible switch in the status bar; it starts disconnected. It never pretends to be a real provider.

## What exists now

- Cargo workspace (Rust 1.99 pinned): `overlook-core`, `overlook-store`, `overlook-provider`, `overlook` (GPUI app). `cargo test --workspace` passes 33 tests; `cargo build` is warning-free.
- Native shell matching the brand previews in Light and Dark: header with mark and wordmark, search, resizable sidebar/list/reader, right or bottom reading pane, comfortable/compact density, day-grouped conversation list, conversation reader with collapsed history, attachment cards, remote-image blocking with per-message “Show images”, composer (new/reply/reply-all/forward) with autosave, recipient suggestions, attachments copied into a protected store, Outbox, status bar with truthful sync wording, toasts, keyboard navigation, Light/Dark/Follow system persisted and applied instantly.
- Durable offline behaviour proven by test and by a scripted run: local change + pending operation in one transaction, Outbox queue, `kill -9`, restart offline, reconnect, ordered replay, provider acceptance before Sent, uncertain-delivery reconciliation without duplicates.
- Gmail preparation: scopes, PKCE, authorization URL, loopback callback parsing, token request body (pure functions with tests). No network, no credentials, no Secret Service yet.
- Packaging drafts in `packaging/` (desktop entry, AppStream metadata, Flatpak manifest). Not built: `flatpak-builder` is not installed here.

## Gmail (added later on 12 October 2026)

Working end to end against the user's real account: sign-in through the system browser with PKCE and a loopback listener, tokens in the GNOME keyring via `oo7`, 150 recent messages downloaded in persisted batches of 20, incremental sync through the history API, read/star/archive/trash/label through `batchModify`, send through `messages.send` with threading headers and Message-ID reconciliation. Multi-account window with an account menu (switch, add Gmail, sign in again, remove). Gmail quota is tight in a fresh project; the engine spaces requests and enters a **Paused** state with backoff when throttled. Read `llm-docs/overlook-native-app.md` → Gmail. The OAuth client file lives at `~/.config/overlook/google-oauth-client.json` and is not in the repository. **Never send mail from the user's account without an explicit request.**

## Daily-use features (added later on 12 October 2026, at the user's request)

Focused Inbox (local classification with per-sender rules), attachment download/open/save, and Google Calendar (week/day/month/agenda, invitations with Accept/Tentative/Decline in the calendar and inside the mail reader, new/edit/delete event, reminders with desktop notifications, event colors, incremental sync) are implemented and validated against the user's real account: 1,189 event instances downloaded, working-location entries shown in the day header. Calendar was deferred in the product plan; the user explicitly moved it up. See `overlook-native-app.md`.

## Distribution and README (added later on 12 October 2026)

`install.sh` at the repository root is the one-command installer in the Last Refuge style (`curl -fsSL https://raw.githubusercontent.com/last-refuge/overlook/main/install.sh | bash`): on Arch/Omarchy it builds a package from `packaging/PKGBUILD` with makepkg and installs it with pacman; elsewhere, or with `--prefix`, it builds into a user prefix. `--dry-run`, `--uninstall`, `--ref`, `--yes`. A `Makefile` offers `make install PREFIX=…`. The README is the public front page with screenshots in `docs/screenshots/` taken from the sample profile (the sample provider now also serves a sample calendar with a week of fictional events, so the calendar can be demonstrated without Google). Refresh screenshots with the capture scripts when the shell changes visibly. Release pipeline and in-app updates were added the same day: see `overlook-native-app.md` → Releases and updates. The first release is cut by tagging `v0.1.0` (the workflow refuses tags that do not match `Cargo.toml`). After each release, bump the version in `Cargo.toml` so running builds report themselves as older than the next tag.

## Settings (added later on 12 October 2026)

A Settings dialog (`Ctrl ,`) with Accounts (signatures, sender name, status, remove), General, Mail, Calendar, and About pages; see `overlook-native-app.md` → Settings. Preferences are typed in `overlook_core::prefs` and every control changes real behaviour. Not built: multiple signatures, automatic replies, rules/sweep.

## What does not exist yet

- Inline `cid:` images, progressive download of older mail history, Gmail batch requests.
- Calendar: recurring-series editing (edits apply to the single instance), attendee editing in the form, drag to move or resize, time-zone display for events in other zones, calendars from more than one account merged in one view.
- Rich-text composition, signatures, HTML output. Received-attachment download. Multi-select, drag-and-drop, undo.
- A proper table layout in the HTML view; GNOME/KDE/fractional-scaling checks; screen-reader checks; Flatpak build.

## Recommended next milestone

1. **Daily-use feedback** on mail, Focused Inbox, attachments, and calendar (the user is now living in Overlook): rendering issues, missing actions, sync correctness. Event editing and invitation handling inside mail are the most likely next asks.
2. **HTML fidelity decision.** Run the plan's corpus (newsletters, receipts, tables, inline images, long threads, malformed messages) through the sanitizer + native view. Decide between improving the native renderer (table layout, inline images via `cid:` from the attachment store), or an out-of-process renderer. Record the decision in the foundation plan.
3. **Deliberate send test** from the user's own address when they ask for it, watching the Sent copy, threading, and the Outbox states.
4. Keep the previews fresh with `scripts/capture-window.sh`, repackage the brand kit after documentation changes, and extend tests for every reliability gate touched.

Judge progress by what the running app does next to the design captures, and by `cargo test`, not by whether a window opens.

## Commands

```bash
cargo test --workspace
```

```bash
cargo run -p overlook
```

```bash
OVERLOOK_DATA_DIR=/tmp/overlook-demo OVERLOOK_SAMPLE_ONLINE=1 cargo run -p overlook
```

```bash
scripts/capture-window.sh /tmp/overlook-demo /tmp/overlook.png
```

Brand assets and the HTML design preview keep their existing maintenance commands (`branding/source/build_assets.py`, `prepare_mail_preview.py`, `package_brand.py`); see `llm-docs/overlook-brand-assets.md`.


## 2026-10-12 (later): Google verification prep

- Scope justifications, demo-video method, and the remaining submission checklist are in
  `llm-docs/google-verification.md`. The captioned demo is `~/Videos/overlook-oauth-demo.mp4`.
- Fixed: calendar shortcuts (T/D/W/M/A, arrows) no longer fire while typing in the event editor
  (`typing_in_event_editor` propagates the key); sync no longer resurrects events with pending local
  operations (`Store::pending_event_ids`, filtered in `sync_calendar`).
- `scripts/record-demo.sh` records a full-screen take on a fresh profile for anyone redoing the video by hand.
- Everything above plus the updater/release batch is uncommitted, awaiting "commit this and push".

- v0.1.0 is published: https://github.com/last-refuge/overlook/releases/tag/v0.1.0 (pacman package,
  tarball, SHA256SUMS, install.sh). First attempt failed on a missing `libxkbcommon-x11` in the CI
  container; it is now a declared dependency. The working version is 0.1.1 so the next tag reads as newer.
  makepkg also uploads a `-debug-` package; the updater and installer skip it when choosing an asset.

- v0.1.1 published 2026-10-12 (Gmail line-break fix, calendar shortcut and sync fixes, updater skips debug package). Working version is now 0.1.2.

- Line breaks in HTML mail: `vendor/gpui-base` is a vendored copy of gpui-base 0.7.3 with one patch (`<br>` renders as a hard line break; see `vendor/README.md`), wired through `[patch.crates-io]`. The sanitizer no longer splits blocks at `<br>`. Worth upstreaming to longbridge/gpui-component; drop the vendor copy when it lands.

- Rendering review (2026-10-12): see "Rendering" in `overlook-native-app.md`. New `prepass.rs` DOM stage,
  quoted-text collapsing, inline `cid:` images (schema v6, `content_id`), plain-text quotes, vendored
  parser patches for `<br>`, block assembly, `<hr>`, `<pre>`, `<ol start>`. The "Show quoted text"
  toggle was not exercised by click during the review (no pointer automation at the time); verify once by hand.

- v0.1.2 published 2026-10-12 (rendering review: pre-pass, quoted text, inline images, vendored parser patches). Working version is now 0.1.3.

- v0.1.3 published 2026-10-12 (Settings segmented controls show the current choice). Working version is now 0.1.4.

- v0.1.4 published 2026-10-12 (shared segmented control with visible selection in header, calendar toolbar, Settings). Working version is now 0.1.5.

- Rich-text composer (2026-10-12): see "Composer" in `overlook-native-app.md`. Markdown-flavoured
  editor with live styling (`ui/mail_highlight.rs`), toolbar, shortcuts, preview, list continuation;
  `overlook_core::compose` renders HTML + text; `Draft.include_quote` (schema v7) replaces inline
  quoting; MIME is multipart/alternative. First item of roadmap phase 1 done.

- Batch 2 (2026-10-12): online search, reader polish (expand all, per-message menu with print/.eml/source),
  image thumbnails + quick look, data tables kept, recurring-series scopes, drag to move/resize, guests with
  free/busy and Google Meet. Schema v8 (`original_start`, `wants_conference`). Drag and the scope prompt were
  verified by build and unit tests only; no pointer automation was available to click them. Deferred items
  are listed in the roadmap doc.

- v0.2.0 published 2026-10-12 (rich-text composer; online search, reader polish, image previews, data tables, recurring-series scopes, drag to move/resize, guests + Meet). Working version is now 0.2.1.

- Batch 3 (2026-10-12): Gmail batch endpoint, PDF previews (pdftoppm), colspan alignment, contacts (People
  API, schema v9), named signatures + automatic replies (gmail.settings.basic), mailto with single instance,
  reminder snooze, calendar Enter/Delete/Escape, sup/sub glyphs. New OAuth scopes mean existing sign-ins show
  "Sign in again" in Settings → Accounts for contacts and automatic replies until re-authorised.

- v0.3.0 published 2026-10-12 (Gmail batch endpoint, PDF previews, colspan, contacts, named signatures + automatic replies, mailto single instance, reminder snooze, calendar keys, sup/sub). Working version is now 0.3.1.

- Rendering follow-up (2026-10-12): author text colours are kept and theme-adapted; Outlook From:/Sent:
  header blocks are no longer treated as quote markers; nested quote splitting takes trailing nodes;
  quotes collapse only when they repeat the thread. Verified on the annotated board-topics message in
  both themes.

- Contacts module (2026-10-13): third module with add/edit/delete through the People API; scope
  `contacts` (read-write) replaces `contacts.readonly`. Schema v10 `people` table. The OAuth scope set is
  now final for the verification submission; the demo video must show Contacts as well.

- v0.4.0 published 2026-10-13 (Contacts module with People API read-write; author text colours kept; Outlook reply headers shown inline; quotes collapse only when they repeat the thread). Working version is now 0.4.1.

- Reader header (2026-10-13): the sender's address sits beside the name ("Brian Cramer <b@…>") and the
  second line is "to …, cc …" only; a rule precedes each embedded message header in a body.

- v0.4.1 published 2026-10-13 (rule before embedded message headers; sender address beside the name). Working version is now 0.4.2.

- v0.4.2 published 2026-10-14 (Alexander theme, native message formatting, isolated Chromium rendering). The working version stayed at 0.4.2 by mistake until 1.5.0.

- v1.5.0 published 2026-10-15: IMAP and POP3 accounts with SMTP sending (plan and milestones M0 to M5 in `imap-pop3-implementation-plan.md`: Add account dialog with autoconfig, IMAP read/write sync with IDLE and QRESYNC, POP3 with local-primary storage and retention, Back up mail, interoperability matrix). Real-server reading and writing was verified by the user against Fastmail only after release; the ignored real-server tests and `scripts/interop-matrix.sh` cover the rest. Working version is now 1.5.1.
- v1.5.1 published 2026-10-21: instant mailbox actions (in-memory `MailIndex`, background ordered writes), fast message bodies (warm Chromium, pre-rendered neighbours, finished-render cache, native plain text, text placeholder, first render at the reader width), and the sync patch (scoped label/attachment loads with an index, read-only reader connection, pending changes sent before downloading). Working version is now 1.5.2.
- v1.5.2 published 2026-10-21: virtual message list (and no per-row database read on repaint), recipient autocomplete from memory, attachment prefetch after sync, Undo Send (Settings > Mail, default 5 s), merged replay of queued changes (`ops::plan_replay`), background draft autosave and reply drafts, cached per-account indexes, startup timing logs, and the Ctrl+K command palette. Working version is now 1.5.3.
- v1.5.3 published 2026-10-22: Omarchy notifications (`notify.rs`: `omarchy-notification-send` with glyphs and click-to-focus when available, freedesktop otherwise; success/warning/error toasts mirrored behind Settings > General > Notifications), and working new-mail notifications. Working version is now 1.5.4.
- v1.5.4 published 2026-10-22: read-only Store methods use the reader connection, settings and attachment paths written by `persist::later`, UI stall watchdog (`watchdog.rs`) and `overlook.log` in the data folder, for the intermittent "not responding" notice. Working version is now 1.5.5.
- v1.5.5 published 2026-10-22: dialogs (Settings, Add account, Quick Look, palette) occlude the message body beneath, which had swallowed their button clicks; also ships the CASA security changes (Google sign-in/OAuth, Gmail API, browser engine, updater, rustls) that entered main in 5146faa. Working version is now 1.5.6.
- v1.5.9 published 2026-10-26 (1.5.6–1.5.8 shipped Fastmail calendars, recipient autocomplete fixes and the Soft stone Light theme without handoff notes): Light's reading and compose area is white again inside the stone frame; Alexander selects with Outlook light blue (`#CCE8FF`) and its amber edge marks unread mail only. Working version is now 1.5.10.
- v1.5.10 published 2026-10-26: Light's message list is pale green (`#F3F7F1`, unread rows `#E3EBE0`) via `Palette::list_surface`/`list_unread`. Working version is now 1.5.11.
- v1.6.0 published 2026-10-27: CASA round-2 code changes (constant crypto/network errors, keyring access audit, release logging lock, credential-free sign-in log events, Sign out with invalid_grant deletion and 90-day idle expiry, outgoing attachment policy); see `casa-remediation-plan.md`. Working version is now 1.6.1.
- v1.6.1 published 2026-10-28: CASA follow-ups (record-ownership checks in the store, no auto-retry after rejected credentials, owner-only log/profile, attachment copies 0600 with clean names, 25 MB cap and optional ClamAV, 180-day session limit, cargo-audit in CI and releases). Working version is now 1.6.2.
