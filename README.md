<p align="center">
  <img src="branding/logos/overlook-social-preview.png" alt="Overlook — A clear view of what matters." width="820">
</p>

<p align="center">
  <strong>A calmer place for your mail.</strong> A native Gmail and Google Calendar client for Linux, written in Rust.<br>
  Thoughtfully made for Linux, Arch, and Omarchy. By <a href="https://lastrefuge.ai/">Last Refuge</a>.
</p>

<p align="center">
  <a href="#install"><img alt="Install with one command" src="https://img.shields.io/badge/install-one%20command-365F73?style=flat-square"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-1.99-536E63?style=flat-square&logo=rust&logoColor=white">
  <img alt="GPUI" src="https://img.shields.io/badge/UI-GPUI-365F73?style=flat-square">
  <img alt="Linux" src="https://img.shields.io/badge/Linux-Wayland-101C24?style=flat-square&logo=linux&logoColor=white">
  <img alt="Status" src="https://img.shields.io/badge/status-early%20daily%20use-FF9D32?style=flat-square">
</p>

---

Overlook is for people who liked the way **New Outlook** works and want that on Linux: a spacious three-pane
workspace, a Focused Inbox, a real calendar, and a composer that feels finished. It is **local-first**: your
mail and events live in a database on your machine, every change is saved on this device before it is sent,
and you can read, search, write, and file mail with no connection at all.

<p align="center">
  <img src="docs/screenshots/inbox-light.png" alt="Overlook inbox in the Light theme" width="920">
</p>

<p align="center"><sub>The inbox in Light. Fictional sample mail, included with the app.</sub></p>

## What you get

- **Mail the Outlook way.** Account and folder sidebar, message list with day groups, a generous reading
  pane you can move below the list, comfortable and compact density, resizable panes that remember their size.
  A slim icon rail switches between Mail, Calendar, and Contacts; the toolbar keeps each module's actions
  close at hand, with crisp two-tone icons that follow your color scheme.
  Drag toolbar buttons into your preferred order or between the strip and More; each module remembers
  your layout. More → Customize toolbar also offers keyboard controls and a reset option.
- **Focused Inbox.** The Inbox splits into Focused and Other from Gmail's own hints, who you write to, and
  your choices: *Move to Other*, or *Always move messages from this sender*. Local, private, and reversible.
- **Reading that respects you.** Received HTML uses isolated Chromium layout inside the native shell;
  scripts never run and remote images/fonts are blocked until you ask. Authored cards, panels and tables
  keep their browser layout, with smooth native scrolling and no blank flash during reflow.
  If the renderer fails, available content stays visible with a Retry action. Image and PDF attachments show thumbnails
  and open in a quick look (PDFs through poppler's `pdftoppm`, which Omarchy ships); everything else opens with
  your system apps, except programs and scripts, which Overlook never launches. Every message has Reply, Forward, Print, Save
  as .eml and View source, and long threads expand or collapse in one click.
- **Writing.** New, reply, reply all, forward, with autosave to the device, recipient suggestions from your
  Google Contacts and people you have written to, attachments (a forward brings the original's along), several named HTML signatures per account (with pictures, a small
  formatting editor, and a picker in the composer), and `mailto:` links from other apps. Formatting as you type: bold, italics, links,
  lists, quotes and code, styled live in the editor with the marks still visible, a Preview of exactly what
  recipients get, and the quoted original kept out of your way below the reply. Like Gmail, Overlook does not
  send programs, scripts, or installers (`.exe`, `.msi`, `.sh`, `.desktop`, `.AppImage` and similar), files whose
  content is one under another name, or `.zip`/`.tar` archives containing them.
- **Drafts across devices.** Gmail and IMAP drafts sync both ways, including attachments and inline images.
  Offline changes wait for a connection; conflicting edits are preserved as separate drafts. POP3 drafts stay local.
- **A calendar, not an afterthought.** Day, Week, Month, and Agenda. Invitations with Accept, Tentative, and
  Decline, from the calendar or from the email itself. Drag across empty time to create an event, right-click one to edit, duplicate, or delete
  it, drag events to move or resize them, edit one occurrence of a repeating event or the whole series, invite guests and check whether they
  are free, add a Google Meet link. Reminders as desktop notifications. Working hours shaded. Google's event
  colors.
- **Offline, properly.** Archive, star, move, reply, and send while disconnected; everything is queued in order
  and replayed when you are back. A visible Outbox says *Waiting for connection*; *Sent* appears only after
  Gmail accepts the message, and a dropped connection mid-send is reconciled before any retry so nothing is
  sent twice. Survives a crash or a restart.
- **A theme for your taste.** Light, Dark, Gray, Blue, White, Alexander, and Pink, with *Follow system*
  selecting Light or Dark. Bundled Inter and Source Serif 4 so it
  looks the same on every machine. Keyboard shortcuts you already know from Outlook.
- **Contacts.** A third module beside Mail and Calendar: your Google Contacts and the people you have written
  to, searchable, with a detail card, add, edit and delete that sync back to Google, and one click to write to
  someone or find everything they sent. Changes made offline wait in the same queue as mail.
- **Any mailbox.** Gmail through its API, and IMAP or POP3 accounts for everything else: type your address
  and password, Overlook looks the servers up (common providers are built in, then Thunderbird's database),
  tests the connection, and keeps the password in the system keyring. IMAP folders become labels, new mail
  arrives the moment the server announces it, changes sync back as moves and flags, and sending goes over
  SMTP. POP3 downloads and keeps your mail here, with a backup button. Scripted servers only so far; see the
  status below.
- **Settings that do things.** Accounts and signatures, notifications, sync interval, mark-as-read timing,
  remote content, download size, working hours, reminders, time-zone overrides, automatic replies (out of office), and more. Every
  control takes effect immediately.
  Mail and calendar use date-aware daylight-saving rules. Overlook checks for system time-zone changes
  every minute and asks before switching; keeping the current zone saves an override.
- **Runs in the background if you want it to.** Settings → General → *Keep running when the window is
  closed* keeps mail and calendars syncing, with new-mail notifications and event reminders, after you close
  the window, and puts the Overlook mark in the system tray (with a dot while the Inbox has unread mail).
  Open it again from the tray, the app launcher or a notification; <kbd>Ctrl</kbd>+<kbd>Q</kbd>, the tray
  menu or `overlook --quit` quits. *Start Overlook when you log in* adds an autostart entry, which starts it
  without a window when it keeps running in the background. Launching Overlook again always brings the
  running one forward rather than starting a second copy.

<p align="center">
  <img src="docs/screenshots/calendar-week-light.png" alt="Overlook calendar week view" width="920">
</p>
<p align="center"><sub>Week view with overlapping meetings, an all-day row, working hours, and today's marker.</sub></p>

<table>
  <tr>
    <td><img src="docs/screenshots/inbox-dark.png" alt="Inbox in Dark"></td>
    <td><img src="docs/screenshots/calendar-week-dark.png" alt="Calendar in Dark"></td>
  </tr>
  <tr>
    <td align="center"><sub>Dark theme</sub></td>
    <td align="center"><sub>Calendar in Dark</sub></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/compose-light.png" alt="Composer with recipient suggestions"></td>
    <td><img src="docs/screenshots/settings-light.png" alt="Settings dialog"></td>
  </tr>
  <tr>
    <td align="center"><sub>Composer with recipient suggestions</sub></td>
    <td align="center"><sub>Settings</sub></td>
  </tr>
</table>

## Install

Message bodies require sandboxed Chromium. The Arch package installs this dependency;
other systems need a compatible Chromium on PATH (or `OVERLOOK_BROWSER_BIN` pointing
to its executable). See [message rendering and isolation](docs/message-formatting.md).

**Arch Linux and Omarchy.** One command:

```bash
curl -fsSL https://raw.githubusercontent.com/last-refuge/overlook/main/install.sh | bash
```

It downloads the [latest release](https://github.com/last-refuge/overlook/releases/latest), verifies its
SHA-256 checksum, and installs the package with `pacman`, so `pacman` can update and remove it. It asks for
your password only for `pacman`. On Omarchy, press <kbd>Super</kbd>+<kbd>Space</kbd> and type "Overlook".
If no prebuilt package exists for your machine it builds one from source instead, which takes a few minutes.

Prefer to read it first? [`install.sh`](install.sh) is short. Options: `--dry-run` shows the plan and changes
nothing, `--prefix DIR` installs into a directory without administrator access, `--ref REF` builds a branch
or tag, `--uninstall` removes it, `--yes` skips confirmations.

**Any other Linux.** The same command builds from source into `~/.local` with no administrator access. You
need Rust (via [rustup](https://rustup.rs)), git, and development packages for Wayland, libxkbcommon,
fontconfig, FreeType, and a Vulkan driver. Or by hand:

```bash
make install PREFIX=~/.local
```

`--source` forces a build from source even when a release exists.

**Remove it:** `sudo pacman -Rns overlook` on Arch, or `./install.sh --uninstall`. Your mail and settings in
`~/.local/share/overlook` stay until you delete them. If Overlook starts at login, switch that off in
Settings first (`./install.sh --uninstall` does it for you).

### Updates

Overlook checks GitHub for a new release once a day (switch it off in Settings → About) and shows a banner
when there is one. **Update now** downloads the release, verifies its checksum, and installs it in place:
through a polkit password dialog when Overlook was installed with pacman, or by swapping the binary when it
lives in your own prefix. Then **Restart Overlook**. Nothing about you is sent during the check.

On first launch, choose to load a **sample mailbox** or start empty and connect your own account. The sample
account's connection is a switch in the status bar; flip it to watch offline changes sync.

## Connect Gmail

Overlook talks to Gmail and Google Calendar directly through Google's APIs, from your machine, with an OAuth
client that belongs to you. Nothing passes through a Last Refuge server.

1. In [Google Cloud](https://console.cloud.google.com), create a project, enable the **Gmail API** and the
   **Google Calendar API**.
2. Configure the consent screen (External, Testing), add yourself as a test user, and add these scopes:
   `gmail.modify`, `gmail.send`, `gmail.settings.basic`, `calendar.events`, `calendar.readonly`,
   `contacts`, `contacts.other.readonly`, `openid`, `userinfo.email`.
3. Create an OAuth client of type **Desktop app** and download its JSON.
4. Save it as `~/.config/overlook/google-oauth-client.json`.
5. In Overlook, open the account menu at the bottom of the sidebar → **Add Gmail account…** and sign in in
   your browser. Repeat for more accounts.

Tokens are stored in your system keyring (GNOME Keyring or KWallet through the Secret Service), never in
files. In Google's *Testing* mode you will be asked to sign in again every seven days; publishing the app
in Google Cloud lifts that. The [native app guide](llm-docs/overlook-native-app.md#gmail) has the details.
The [secrets access policy](docs/secrets-policy.md) lists every stored secret, who can reach it, and how access is logged.

## Connect Microsoft

Outlook.com, Hotmail, Live and MSN addresses and Microsoft 365 work or school accounts are read over IMAP (or
POP3) and sent over SMTP. You sign in with Microsoft in your browser. Overlook never sees your password, and it
keeps the sign-in Microsoft issues in your system keyring.

1. Open the account menu at the bottom of the sidebar → **Add Microsoft account…** (or Settings → Accounts →
   Add account → Microsoft).
2. Enter your address and choose **Sign in with Microsoft**, then approve Overlook in the browser.
3. Overlook fills in Microsoft's server settings, checks both servers, and adds the account.

An Outlook.com address entered as an IMAP or POP3 account switches to this sign-in by itself, because Microsoft
no longer accepts passwords from mail apps. Microsoft 365 organizations may need an administrator to approve
Overlook first, and some turn off IMAP, POP or SMTP sign-in for mail apps; Overlook tells you which. Outlook.com
accounts set up as POP3 need POP turned on in Outlook.com's settings.

Overlook will ship with Last Refuge's Microsoft app registration built in, but current builds don't have one yet.
Until then, register a public client in Microsoft Entra (redirect URI `http://localhost`, personal and work
accounts) and save `{"client_id": "<application id>"}` as `~/.config/overlook/microsoft-oauth-client.json`. The
[Microsoft account notes](llm-docs/microsoft-mail.md#the-app-registration) walk through it.

## Connect iCloud

Open the account menu at the bottom of the sidebar → **Add iCloud account…** (or Settings → Accounts → Add
account → iCloud Mail). iCloud wants an **app-specific password**, not your Apple Account password: create one
at [account.apple.com](https://account.apple.com) under Sign-In and Security → App-Specific Passwords (your
account needs two-factor authentication), then paste it into Overlook, which keeps it in your system keyring.
Overlook fills in Apple's server settings and finds which user name iCloud wants. For an iCloud+ address at your
own domain, use your iCloud Mail address as the user name. iCloud doesn't offer POP, so it's always IMAP.

## Connect Yahoo or AOL

Open the account menu → **Add Yahoo account…** or **Add AOL account…**. Both want an **app password**, not your
account password: create one in your account's Account Security settings
([Yahoo](https://login.yahoo.com/account/security): External connections → Create app password;
[AOL](https://login.aol.com/account/security): Generate app password, after turning on two-step verification),
then paste it into Overlook. Spaces in it don't matter. Yahoo only offers app passwords in a browser you've used to
sign in to Yahoo for several days. Overlook fills in the server settings; both work over IMAP or POP3.

## Keyboard

| Key | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | New message |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>R</kbd> / <kbd>Ctrl</kbd>+<kbd>F</kbd> | Reply / Reply all / Forward |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Send |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> / <kbd>I</kbd> / <kbd>K</kbd> | Bold / italic / link while writing |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>8</kbd> / <kbd>7</kbd> / <kbd>9</kbd> | Bulleted list / numbered list / quote |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>P</kbd> | Preview the message |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Search this device, then Gmail |
| <kbd>J</kbd> / <kbd>K</kbd> | Next / previous conversation |
| <kbd>E</kbd> | Archive, or Move to Inbox when already archived |
| <kbd>Delete</kbd> | Move to Trash |
| <kbd>S</kbd> / <kbd>U</kbd> | Star / toggle read |
| <kbd>Ctrl</kbd>+<kbd>1</kbd> / <kbd>2</kbd> / <kbd>3</kbd> | Mail / Calendar / Contacts |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>A</kbd> | New event |
| <kbd>←</kbd> <kbd>→</kbd> <kbd>T</kbd> <kbd>D</kbd> <kbd>W</kbd> <kbd>M</kbd> <kbd>A</kbd> | Calendar: move, today, day, week, month, agenda |
| <kbd>Enter</kbd> / <kbd>Delete</kbd> | Calendar: edit / delete the selected event |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit Overlook (closing the window keeps it running when that's on) |
| <kbd>Esc</kbd> | Close composer or settings, clear search |

Drag a conversation onto Inbox, Archive, Trash, Starred, or a label in the sidebar to move it. Every move
has Undo.

## Built with

Rust and [GPUI](https://gpui.rs), Zed's GPU-accelerated UI framework, through [GPUI Kit](https://gpui-kit.com).
SQLite with FTS5 for local storage and search. A native shell, no Electron; sandboxed Chromium renders
interactive message bodies through private pipes, without an exposed debugging port.
See the [message formatting review](docs/message-formatting.md) for rendering policy, regression coverage,
and native HTML layout limitations.

```
crates/overlook-core       domain model, Focused Inbox rules, HTML sanitizer, time zones, preferences
crates/overlook-store      SQLite: messages, drafts, Outbox, pending operations, calendars, events, settings
crates/overlook-provider   provider traits, the Gmail and Google Calendar providers, the sample provider
crates/overlook-app        the GPUI application: shell, sync engine, keyring, sign-in, notifications
branding/                  brand system, fonts, logos, design previews
packaging/                 desktop entry, AppStream metadata, PKGBUILD, Flatpak manifest draft
llm-docs/                  product plan, brand system, architecture and implementation notes
```

Run the tests with `cargo test --workspace`. Received HTML is sanitized without flattening authored
layouts, then rendered by isolated Chromium. Scripts are disabled and remote images, stylesheets and
fonts are consent-gated. Originals remain unchanged. Printing and outgoing quotes still use the safe
native subset; see [rendering policy and limits](docs/message-formatting.md).

## Status

Early daily use by its author on Omarchy (Hyprland, Wayland). Gmail accounts work end to end. IMAP/SMTP and
POP3 are built and tested against scripted servers and public servers' login exchanges, but no real account
has been synced yet, so treat them as beta. Outlook.com and Microsoft 365 sign in with Microsoft (OAuth), tested so
far against scripted servers only. A Flatpak build,
GNOME, KDE, and screen-reader testing are still to be done. Open an issue with what you did, what you saw, and what you
expected.

## Brand and credits

The horizon mark, wordmark, palette, and typography are documented in [`llm-docs/overlook-brand.md`](llm-docs/overlook-brand.md).
Fonts: [Inter](https://rsms.me/inter/) and [Source Serif 4](https://github.com/adobe-fonts/source-serif) under the
SIL Open Font License. Icons: [Lucide](https://lucide.dev) (ISC). Overlook is a Last Refuge product.

## License

Overlook is released under the [MIT License](LICENSE). Copyright © 2026 Last Refuge.

The vendored [`vendor/gpui-base`](vendor/gpui-base) keeps its own Apache-2.0 license, and the bundled fonts and icons keep
the licenses listed above.
