---
version: 1
slug: "crates-overlook-app-src-ui-shell-rs"
primary_target: "crates/overlook-app/src/ui/shell.rs"
related_targets: ["crates/overlook-app/src/ui/reader.rs","crates/overlook-app/src/ui/list.rs","crates/overlook-app/src/ui/sidebar.rs","crates/overlook-app/src/ui/toolbar.rs"]
---

# Main mail window: polished, everything kept (mock-up)

**Scope and mode.** A design mock-up of Overlook's main mail window, made on the "Overlook Redesign" canvas (https://claude.ai/artifact/TpFQGpy7AQRnW6XKQY1MMS). Mode: Operate. The native window lives in `ui/shell.rs`, `toolbar.rs`, `sidebar.rs`, `list.rs` and `reader.rs`.

**History.** On 13 November 2026 the user rejected a conversation-first direction that folded the toolbar away and merged the navigation. They said it "went backwards… dumbing down the UI", and that its "additions" already existed. They chose New Outlook as the benchmark for finish and density.

**Constraints.** Keep every element and capability of today's window:
- Title bar and search.
- The full labelled, customisable command toolbar.
- The 48 px module rail with History and Settings.
- Favorites, account folders, labels and folders.
- Focused/Other, date groups, stars, thread counts, attachments and invitations in the list.
- Folded earlier messages, attachment tiles, per-message and bottom Reply / Reply all / Forward, and the "Reply to…" bar.
- The status bar.

Use fictional mail only. Show Light, Alexander (the user's daily colourway) and Dark.

## Direction contract

THESIS: Polish, not reinvention. Today's dense, Outlook-familiar window is kept piece for piece and brought to New Outlook's level of finish: grouped commands, consistent two-tone icons, tighter rhythm, and richer list rows. It refuses simplification: no folded toolbar, no merged navigation, no hidden commands.

OWN-WORLD: Overlook's colourways.
- **Light:** Soft stone frame (#E1E2DC), a pale green list (#F3F7F1), white reading paper, pine actions.
- **Alexander:** white chrome, a dark teal navigation and gutter (#1F343F), a blue action colour, a golden-orange unread edge.
- **Dark:** blue-black, ivory text, glacier actions.
- **Shared:** two-tone icons (stroke plus soft fill). The horizon mark and serif wordmark sit in the title bar. A white toolbar floats on the navigation colour. Corners are 7–10 px, flat at rest with hairline edges.

STORY: Everything the person uses today is where they expect it, only clearer:
- **Toolbar:** New message leads. Delete, Archive and Move; Reply, Reply all and Forward; then Mark unread and Star form separated groups.
- **List:** rows name their attachments and show invitation times with Accept and Decline. Unread rows carry an edge, and hovering a row offers quick actions.
- **Reader:** earlier messages fold to a line each. The latest message has its actions, attachment cards and Reply / Reply all / Forward. The "Reply to…" bar waits at the bottom.

FIRST VIEWPORT: A 1440 × 900 window:
- **Title bar (48 px):** the mark, "Overlook", search with a scope chip and Ctrl E, the account with its provider badge.
- **Rail (52 px).**
- **Floating toolbar (44 px)** across the mail area.
- **Folder pane (232 px).**
- **List (392 px, rounded top).**
- **Reader:** the rest, with a rounded top-left corner.
- **Status bar (28 px).**

FORM: Category standard (the canon) at full fidelity, benchmarked on New Outlook; it replaces the conversation-first roll (seed 8dd62da5). The signature details are the unread edge, the hover quick actions and invitation actions in the list.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance


## Outcome (13 November 2026)

**Shipped** in the native window, everything kept:
- **Title bar:** a centred search field (at most 620px), the account's name beside the avatar, and a service-mark badge on the avatar. The title bar and rail sit on `Palette::chrome()`, which is the frame in Dark.
- **Toolbar:**
  - Commands are grouped by `ToolbarGroup`/`StripPlan`, with separators wherever the group changes, and navigation and app commands trail. This holds for any saved order.
  - New is a split button with the other two New items in its menu.
  - Labels are measured for overflow, the drag grip is an overlay, and Move to has a chevron.
- **Folder pane:** the Inbox unread count is a filled pill in the message accent. Other folders' unread counts are semibold, totals stay muted, and account headers carry service marks.
- **List:**
  - The header reads "N unread · total", and the tab not on show adds "· N".
  - Rows have 32px tinted avatars, thread-count pills, a file chip with "+N", and invitation chips with Accept and Decline (or the answer). The extras line wraps.
  - Hover quick actions act on their row alone; on the open row they match the toolbar.
- **Reader:**
  - The subject has a Print button, and label pills carry colour dots.
  - Folded messages join into one stack.
  - Each message header has the date over a segmented Reply / Reply all / Forward / More group.
  - Attachments show their count and size, with Save all for two or more files.
  - A "Reply to…" bar with Ctrl R sits at the foot, and the email paper's top inset is tighter.
- **Status bar:** the service mark sits beside the account.

**Deliberately adapted from the comp:**
- **Not built, because nothing backs them:** the search scope chip, "Open in its own window" in the reader, and the list's "Select conversations" button.
- **Kept despite the finish review:**
  - Light's sage folder pane (Stone Sidebar #C4CBC1).
  - The selected row's edge bar in Light and Dark, like Outlook.
  - The existing toolbar order: Archive before Delete, and Mark and Star before Reply. The comp led with Delete.
- **Measurements:** the title bar stays at the shell's density height (52px comfortable) and the rail at 48px. The comp asked for 48px and 52px.

Recorded in DESIGN.md: The Polish, Never Remove Rule, plus the main-window components.
