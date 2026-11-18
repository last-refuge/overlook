//! The native macOS menu bar. GPUI dispatches its items to the focused window.

use super::toolbar::NewItem;
use super::*;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};

actions!(
    overlook,
    [
        AboutOverlook,
        ShowSettings,
        HideOverlook,
        HideOtherApps,
        ShowAllApps,
        NewMailMessage,
        NewContactAction,
        AddAccountAction,
        FindInOverlook,
        ShowCommandPalette,
        ReplyToSender,
        ArchiveMessages,
        CloseOverlookWindow,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullScreen,
        OpenOverlookHelp,
        OpenKeyboardShortcuts,
    ]
);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", ShowSettings, None),
        KeyBinding::new("cmd-h", HideOverlook, None),
        KeyBinding::new("cmd-alt-h", HideOtherApps, None),
        KeyBinding::new("cmd-q", QuitOverlook, None),
        KeyBinding::new("cmd-n", NewMailMessage, None),
        KeyBinding::new("cmd-w", CloseOverlookWindow, None),
        KeyBinding::new("cmd-a", SelectAll, Some(LIST_CONTEXT)),
        KeyBinding::new("cmd-f", FindInOverlook, None),
        KeyBinding::new("cmd-1", SwitchToMail, None),
        KeyBinding::new("cmd-2", SwitchToCalendar, None),
        KeyBinding::new("cmd-3", SwitchToContacts, None),
        KeyBinding::new("cmd-shift-p", ShowCommandPalette, None),
        KeyBinding::new("cmd-r", ReplyToSender, None),
        KeyBinding::new("cmd-shift-r", ReplyAll, None),
        KeyBinding::new("cmd-shift-f", Forward, None),
        KeyBinding::new("cmd-ctrl-a", ArchiveMessages, None),
        KeyBinding::new("cmd-backspace", TrashSelected, None),
        KeyBinding::new("cmd-shift-n", SyncNow, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
    ]);

    cx.on_action(|_: &AboutOverlook, cx| {
        cx.defer(|cx| crate::lifecycle::show_settings(SettingsPage::About, cx));
    });
    cx.on_action(|_: &ShowSettings, cx| {
        cx.defer(|cx| crate::lifecycle::show_settings(SettingsPage::General, cx));
    });
    cx.on_action(|_: &HideOverlook, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &QuitOverlook, cx| cx.defer(crate::lifecycle::quit));
    cx.on_action(|_: &OpenOverlookHelp, cx| {
        cx.open_url("https://github.com/last-refuge/overlook#readme");
    });
    cx.on_action(|_: &OpenKeyboardShortcuts, cx| {
        cx.open_url("https://github.com/last-refuge/overlook#keyboard");
    });

    cx.set_menus([
        Menu::new("Overlook").items([
            MenuItem::action("About Overlook", AboutOverlook),
            MenuItem::separator(),
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Overlook", HideOverlook),
            MenuItem::action("Hide Others", HideOtherApps),
            MenuItem::action("Show All", ShowAllApps),
            MenuItem::separator(),
            MenuItem::action("Quit Overlook", QuitOverlook),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Message", NewMailMessage),
            MenuItem::action("New Event", NewEventAction),
            MenuItem::action("New Contact", NewContactAction),
            MenuItem::separator(),
            MenuItem::action("Add Account…", AddAccountAction),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseOverlookWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", FindInOverlook),
        ]),
        Menu::new("View").items([
            MenuItem::action("Mail", SwitchToMail),
            MenuItem::action("Calendar", SwitchToCalendar),
            MenuItem::action("Contacts", SwitchToContacts),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", ShowCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Message").items([
            MenuItem::action("Reply", ReplyToSender),
            MenuItem::action("Reply All", ReplyAll),
            MenuItem::action("Forward", Forward),
            MenuItem::separator(),
            MenuItem::action("Archive", ArchiveMessages),
            MenuItem::action("Move to Trash", TrashSelected),
            MenuItem::action("Move to Folder…", MoveSelected),
            MenuItem::action("Toggle Read Status", ToggleRead),
            MenuItem::action("Toggle Star", ToggleStar),
            MenuItem::separator(),
            MenuItem::action("Sync Now", SyncNow),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
        ]),
        Menu::new("Help").items([
            MenuItem::action("Overlook Help", OpenOverlookHelp),
            MenuItem::action("Keyboard Shortcuts", OpenKeyboardShortcuts),
        ]),
    ]);
}

impl MailWindow {
    pub(super) fn native_window_menu_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &CloseOverlookWindow, window, cx| {
            if this.prepare_composer_window_close(window, cx) {
                window.remove_window();
            }
        }))
        .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
        .on_action(|_: &ZoomWindow, window, _| window.zoom_window())
        .on_action(|_: &ToggleFullScreen, window, _| window.toggle_fullscreen())
        .when(self.settings.is_some(), |root| {
            root.on_action(cx.listener(|this, _: &FindInOverlook, window, cx| {
                this.focus_settings_search(window, cx);
            }))
        })
    }

    pub(super) fn native_mail_menu_actions(
        &self,
        root: Stateful<Div>,
        menu_can_act: bool,
        message_selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (in_trash, in_inbox) = self.removal_state();
        root.when(menu_can_act, |root| {
            root.on_action(cx.listener(|this, _: &NewMailMessage, window, cx| {
                this.start_new_item(NewItem::Message, window, cx);
            }))
            .when(self.contact_form.is_none(), |root| {
                root.on_action(cx.listener(|this, _: &NewContactAction, window, cx| {
                    this.start_new_item(NewItem::Contact, window, cx);
                }))
            })
            .on_action(cx.listener(|this, _: &AddAccountAction, window, cx| {
                this.open_add_account(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowCommandPalette, window, cx| {
                this.open_palette(window, cx);
            }))
            .when(self.module != Module::Calendar, |root| {
                root.on_action(cx.listener(|this, _: &FindInOverlook, window, cx| {
                    this.on_focus_search(&FocusSearch, window, cx);
                }))
            })
        })
        .when(message_selected && !self.multi_selected(), |root| {
            root.on_action(cx.listener(|this, _: &ReplyToSender, window, cx| {
                this.reply(DraftKind::Reply, window, cx);
            }))
        })
        .when(message_selected && in_inbox && !in_trash, |root| {
            root.on_action(cx.listener(|this, _: &ArchiveMessages, window, cx| {
                this.on_archive(&ArchiveSelected, window, cx);
            }))
        })
        .when(menu_can_act && self.list_focus.is_focused(window), |root| {
            root.on_action(cx.listener(|this, _: &SelectAll, window, cx| {
                this.on_select_all(&SelectAllConversations, window, cx);
            }))
        })
    }
}
