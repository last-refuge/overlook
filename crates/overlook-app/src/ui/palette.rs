//! Command palette (Ctrl+K): every action, mailbox and label by typing a few letters,
//! Superhuman-style, so nothing needs the mouse.

use super::calendar::Module;
use super::settings::SettingsPage;
use super::*;
use std::rc::Rc;

/// What a palette entry does when chosen.
#[derive(Clone)]
pub(crate) enum PaletteCommand {
    /// A keyboard action, dispatched as if its shortcut was pressed.
    Action(Rc<dyn Action>),
    Module(Module),
    View(MailboxView),
    MoveTo(MailboxView),
    Account(AccountId),
    Settings(SettingsPage),
}

#[derive(Clone)]
pub(crate) struct PaletteEntry {
    pub title: SharedString,
    pub hint: Option<SharedString>,
    /// A heading drawn above the entry when the one before it belongs to another group.
    pub section: Option<SharedString>,
    /// An icon drawn before the title.
    pub icon: Option<&'static str>,
    pub command: PaletteCommand,
}

/// What the palette lists: every command, or only places mail can be moved to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteMode {
    Commands,
    MoveTo,
}

pub(crate) struct CommandPalette {
    pub mode: PaletteMode,
    pub input: Entity<InputState>,
    pub selected: usize,
    pub _subscription: Subscription,
}

/// Entries whose title contains every word of `query` (any case), best first: matches at
/// the start of the title, then at the start of a word, then anywhere.
pub(crate) fn filter_entries(entries: &[PaletteEntry], query: &str) -> Vec<PaletteEntry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(usize, usize, PaletteEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let title = e.title.to_lowercase();
            if !words.iter().all(|w| title.contains(w.as_str())) {
                return None;
            }
            let rank = match words.first() {
                None => 0,
                Some(w) if title.starts_with(w.as_str()) => 0,
                Some(w) if title.split_whitespace().any(|t| t.starts_with(w.as_str())) => 1,
                Some(_) => 2,
            };
            Some((rank, i, e.clone()))
        })
        .collect();
    hits.sort_by_key(|(rank, i, _)| (*rank, *i));
    hits.into_iter().map(|(_, _, e)| e).collect()
}

impl MailWindow {
    pub(crate) fn palette_entries(&self) -> Vec<PaletteEntry> {
        let entry = |title: &str, hint: Option<&str>, command: PaletteCommand| PaletteEntry {
            title: SharedString::from(title.to_owned()),
            hint: hint.map(|h| SharedString::from(h.to_owned())),
            section: None,
            icon: None,
            command,
        };
        let action = |a: Box<dyn Action>| PaletteCommand::Action(Rc::from(a));
        let mut out = vec![
            entry("New message", Some("Ctrl+N"), action(Box::new(Compose))),
            entry("Reply", Some("Ctrl+R"), action(Box::new(Reply))),
            entry("Reply all", Some("Ctrl+Shift+R"), action(Box::new(ReplyAll))),
            entry("Forward", Some("Ctrl+F"), action(Box::new(Forward))),
            entry("Archive", Some("E"), action(Box::new(ArchiveSelected))),
            entry("Delete", Some("Del"), action(Box::new(TrashSelected))),
            entry("Mark read or unread", Some("U"), action(Box::new(ToggleRead))),
            entry("Star or unstar", Some("S"), action(Box::new(ToggleStar))),
            entry("Undo", Some("Ctrl+Z"), action(Box::new(UndoLast))),
            entry("Show change history", Some("Ctrl+H"), action(Box::new(ShowHistory))),
            entry("Search mail", Some("Ctrl+E"), action(Box::new(FocusSearch))),
            entry("Sync now", Some("F5"), action(Box::new(SyncNow))),
            entry("New event", Some("Ctrl+Shift+A"), action(Box::new(NewEventAction))),
            entry("Go to Mail", Some("Ctrl+1"), PaletteCommand::Module(Module::Mail)),
            entry("Go to Calendar", Some("Ctrl+2"), PaletteCommand::Module(Module::Calendar)),
            entry("Go to Contacts", Some("Ctrl+3"), PaletteCommand::Module(Module::Contacts)),
            entry("Quit Overlook", Some("Ctrl+Q"), action(Box::new(QuitOverlook))),
        ];
        if self.renamable_open_folder().is_some() {
            out.push(entry("Rename folder", Some("F2"), action(Box::new(RenameFolder))));
        }
        for (name, kind) in [
            ("Inbox", LabelKind::Inbox),
            ("Starred", LabelKind::Starred),
            ("Drafts", LabelKind::Drafts),
            ("Outbox", LabelKind::Outbox),
            ("Sent", LabelKind::Sent),
            ("Archive", LabelKind::Archive),
            ("Junk", LabelKind::Spam),
            ("Trash", LabelKind::Trash),
        ] {
            out.push(entry(&format!("Go to {name}"), None, PaletteCommand::View(MailboxView::Kind(kind))));
        }
        let mut user: Vec<&Label> = self.labels.iter().filter(|l| l.kind == LabelKind::User).collect();
        user.sort_by_cached_key(|l| overlook_core::sidebar::folder_title(&l.name).to_lowercase());
        for l in &user {
            out.push(entry(&format!("Go to {}", overlook_core::sidebar::folder_title(&l.name)), Some("label"), PaletteCommand::View(MailboxView::Label(l.id.clone()))));
        }
        if self.multi_selected() || !self.thread.is_empty() {
            out.push(entry("Move to…", Some("V"), action(Box::new(MoveSelected))));
            for l in &user {
                out.push(entry(&format!("Move to {}", overlook_core::sidebar::folder_title(&l.name)), Some("label"), PaletteCommand::MoveTo(MailboxView::Label(l.id.clone()))));
            }
        }
        for a in self.accounts.iter().filter(|a| a.id != self.account.id) {
            out.push(entry(&format!("Switch to {}", a.email), Some("account"), PaletteCommand::Account(a.id.clone())));
        }
        for page in SettingsPage::ALL {
            out.push(entry(&format!("Settings: {}", page.label()), Some("Ctrl+,"), PaletteCommand::Settings(page)));
        }
        out
    }

    /// The Move picker: the last few destinations first, then every folder, filtered by
    /// what is typed.
    pub(crate) fn move_entries(&self) -> Vec<PaletteEntry> {
        let all = self.move_destinations();
        // Recent rows wear a clock, folder rows the icon the sidebar gives them.
        let entry = |view: &MailboxView, name: &str, recent: bool| PaletteEntry {
            title: SharedString::from(name.to_owned()),
            hint: matches!(view, MailboxView::Label(_)).then(|| SharedString::from("label")),
            section: Some(SharedString::from(if recent { "Recent" } else { "Folders" })),
            icon: Some(match (recent, view) {
                (true, _) => "clock",
                (false, MailboxView::Kind(kind)) => super::sidebar::system_folder(*kind).map(|(_, icon)| icon).unwrap_or("folder"),
                (false, MailboxView::Label(_)) => "folder",
            }),
            command: PaletteCommand::MoveTo(view.clone()),
        };
        let mut out: Vec<PaletteEntry> = self
            .recent_moves
            .iter()
            .filter_map(|r| all.iter().find(|(v, _)| v == r))
            .map(|(v, name)| entry(v, name, true))
            .collect();
        out.extend(all.iter().map(|(v, name)| entry(v, name, false)));
        out
    }

    pub(crate) fn open_move_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_in(PaletteMode::MoveTo, window, cx);
    }

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_in(PaletteMode::Commands, window, cx);
    }

    fn open_palette_in(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match mode {
            PaletteMode::Commands => "Type a command, mailbox or label",
            PaletteMode::MoveTo => "Move to…",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, |this, _state, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                if let Some(p) = this.palette.as_mut() {
                    p.selected = 0;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.run_palette_selection(window, cx),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.palette = Some(CommandPalette { mode, input, selected: 0, _subscription: subscription });
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    fn palette_matches(&self, cx: &App) -> Vec<PaletteEntry> {
        let Some(p) = &self.palette else { return Vec::new() };
        let query = p.input.read(cx).value().to_string();
        let entries = match p.mode {
            PaletteMode::Commands => self.palette_entries(),
            // A recent destination is listed under Recent and under Folders. Headings go
            // once something is typed, so only the Recent row is kept then.
            PaletteMode::MoveTo if !query.trim().is_empty() => {
                let mut seen: Vec<MailboxView> = Vec::new();
                self.move_entries().into_iter().filter(|e| match &e.command {
                    PaletteCommand::MoveTo(v) if seen.contains(v) => false,
                    PaletteCommand::MoveTo(v) => { seen.push(v.clone()); true }
                    _ => true,
                }).collect()
            }
            PaletteMode::MoveTo => self.move_entries(),
        };
        filter_entries(&entries, &query)
    }

    fn move_palette_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let n = self.palette_matches(cx).len().min(PALETTE_ROWS);
        if let Some(p) = self.palette.as_mut() {
            if n > 0 {
                p.selected = (p.selected as i32 + delta).rem_euclid(n as i32) as usize;
            }
        }
        cx.notify();
    }

    fn run_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.palette.as_ref().map(|p| p.selected).unwrap_or(0);
        if let Some(entry) = self.palette_matches(cx).into_iter().nth(selected) {
            self.run_palette_command(entry.command, window, cx);
        }
    }

    pub(crate) fn run_palette_command(&mut self, command: PaletteCommand, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match command {
            PaletteCommand::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            PaletteCommand::Module(module) => self.set_module(module, window, cx),
            PaletteCommand::View(view) => {
                self.set_module(Module::Mail, window, cx);
                self.select_view(view, window, cx);
            }
            PaletteCommand::MoveTo(view) => {
                let drag = ConversationDrag { mail: DraggedMail::Selection, title: String::new(), count: 0 };
                self.drop_on_view(drag, view, window, cx);
            }
            PaletteCommand::Account(id) => self.switch_account(id, window, cx),
            PaletteCommand::Settings(page) => self.open_settings(page, window, cx),
        }
    }

    pub(crate) fn render_palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(state) = &self.palette else { return div().into_any_element() };
        let matches = self.palette_matches(cx);
        let selected = state.selected.min(matches.len().saturating_sub(1));
        // Headings only while browsing: once filtered, the groups no longer mean much.
        let headings = state.input.read(cx).value().trim().is_empty();
        let mut last_section: Option<SharedString> = None;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, e) in matches.into_iter().take(PALETTE_ROWS).enumerate() {
            if headings && e.section.is_some() && e.section != last_section {
                let heading = e.section.clone().expect("checked");
                // A rule closes the group before, so Recent reads as its own block.
                if i > 0 {
                    rows.push(div().mx(px(12.)).mt(px(6.)).h(px(1.)).bg(p.divider).into_any_element());
                }
                rows.push(div().px(px(12.)).pt(px(if i == 0 { 2. } else { 8. })).pb(px(2.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(heading).into_any_element());
                last_section = e.section.clone();
            }
            let command = e.command.clone();
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("palette-{i}")))
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(12.))
                    .h(px(34.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .when(i == selected, |d| d.bg(p.selection).text_color(p.accent))
                    .when(i != selected, |d| d.hover(|d| d.bg(p.hover)))
                    .children(e.icon.map(|name| crate::assets::icon(name).size(px(15.)).text_color(if i == selected { p.accent } else { p.muted }).into_any_element()))
                    // Folder paths can be long: the name gives way, the hint keeps its place.
                    .child(div().flex_1().min_w_0().truncate().child(e.title.clone()))
                    .children(e.hint.clone().map(|h| div().flex_none().text_size(px(11.)).text_color(p.muted).child(h)))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_palette_command(command.clone(), window, cx)))
                    .into_any_element(),
            );
        }
        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            // Nothing underneath (a message body) may see the clicks meant for this.
            .occlude()
            .flex()
            .justify_center()
            .pt(px(90.))
            .bg(gpui_kit::black().opacity(0.25))
            .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
            .child(
                v_flex()
                    .id("palette")
                    .w(px(560.))
                    .max_h(px(470.))
                    .p(px(8.))
                    .gap(px(6.))
                    .rounded(px(10.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    // Keep clicks inside from reaching the backdrop.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                        "up" => {
                            this.move_palette_selection(-1, cx);
                            cx.stop_propagation();
                        }
                        "down" => {
                            this.move_palette_selection(1, cx);
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.close_palette(window, cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }))
                    .child(Input::new(&state.input))
                    .child(v_flex().gap(px(2.)).children(rows)),
            )
            .into_any_element()
    }
}

/// How many matches the palette lists.
const PALETTE_ROWS: usize = 10;

#[cfg(test)]
mod tests {
    use super::{filter_entries, Module, PaletteCommand, PaletteEntry};
    use gpui_kit::SharedString;

    fn entries(titles: &[&str]) -> Vec<PaletteEntry> {
        titles
            .iter()
            .map(|t| PaletteEntry { title: SharedString::from(t.to_string()), hint: None, section: None, icon: None, command: PaletteCommand::Module(Module::Mail) })
            .collect()
    }

    fn titles(found: Vec<PaletteEntry>) -> Vec<String> {
        found.into_iter().map(|e| e.title.to_string()).collect()
    }

    #[test]
    fn every_word_must_match_and_title_starts_rank_first() {
        let list = entries(&["Go to Archive", "Archive", "Go to Projects", "Move to Projects", "Reply all"]);
        assert_eq!(titles(filter_entries(&list, "arch")), vec!["Archive", "Go to Archive"]);
        assert_eq!(titles(filter_entries(&list, "go proj")), vec!["Go to Projects"]);
        assert_eq!(titles(filter_entries(&list, "PROJ")), vec!["Go to Projects", "Move to Projects"]);
        assert_eq!(filter_entries(&list, "").len(), 5);
        assert!(filter_entries(&list, "zzz").is_empty());
    }
}
