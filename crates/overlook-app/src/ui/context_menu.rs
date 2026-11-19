//! Standard editor-style right-click context menus for email bodies and composer fields.

use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{Copy, Cut, Delete, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::{actions, App, Entity, Focusable, Window};

actions!(overlook_email_menu, [OpenContextLink, CopyContextLink]);

/// Describes which menu entries are enabled for a read-only email body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadOnlyEmailMenuState {
    pub has_selection: bool,
    pub has_content: bool,
    pub has_link: bool,
}

/// Describes which menu entries are enabled for an editable email text field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditableTextMenuState {
    pub editable: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub can_copy: bool,
    pub has_selection: bool,
    pub has_clipboard_text: bool,
    pub has_text: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuSpec {
    Separator,
    Item { label: &'static str, enabled: bool },
}

impl ReadOnlyEmailMenuState {
    #[cfg(test)]
    fn specs(self) -> Vec<MenuSpec> {
        let mut out = Vec::with_capacity(8);
        if self.has_link {
            out.push(MenuSpec::Item { label: "Open Link", enabled: true });
            out.push(MenuSpec::Item { label: "Copy Link Address", enabled: true });
            out.push(MenuSpec::Separator);
        }
        out.push(MenuSpec::Item { label: "Cut", enabled: false });
        out.push(MenuSpec::Item { label: "Copy", enabled: self.has_selection });
        out.push(MenuSpec::Item { label: "Paste", enabled: false });
        out.push(MenuSpec::Separator);
        out.push(MenuSpec::Item { label: "Select All", enabled: self.has_content });
        out
    }

    pub(crate) fn build(self) -> NativeMenu {
        let mut menu = NativeMenu::new();
        if self.has_link {
            menu = menu
                .menu("Open Link", Box::new(OpenContextLink))
                .menu("Copy Link Address", Box::new(CopyContextLink))
                .separator();
        }
        menu.menu_with_disabled("Cut", true, Box::new(Cut))
            .menu_with_disabled("Copy", !self.has_selection, Box::new(Copy))
            .menu_with_disabled("Paste", true, Box::new(Paste))
            .separator()
            .menu_with_disabled("Select All", !self.has_content, Box::new(SelectAll))
    }
}

impl EditableTextMenuState {
    #[cfg(test)]
    fn specs(self) -> Vec<MenuSpec> {
        vec![
            MenuSpec::Item { label: "Undo", enabled: self.editable && self.can_undo },
            MenuSpec::Item { label: "Redo", enabled: self.editable && self.can_redo },
            MenuSpec::Separator,
            MenuSpec::Item { label: "Cut", enabled: self.editable && self.can_copy },
            MenuSpec::Item { label: "Copy", enabled: self.can_copy },
            MenuSpec::Item { label: "Paste", enabled: self.editable && self.has_clipboard_text },
            MenuSpec::Item { label: "Delete", enabled: self.editable && self.has_selection },
            MenuSpec::Separator,
            MenuSpec::Item { label: "Select All", enabled: self.has_text },
        ]
    }

    pub(crate) fn build(self, menu: NativeMenu) -> NativeMenu {
        menu.menu_with_disabled("Undo", !(self.editable && self.can_undo), Box::new(Undo))
            .menu_with_disabled("Redo", !(self.editable && self.can_redo), Box::new(Redo))
            .separator()
            .menu_with_disabled("Cut", !(self.editable && self.can_copy), Box::new(Cut))
            .menu_with_disabled("Copy", !self.can_copy, Box::new(Copy))
            .menu_with_disabled(
                "Paste",
                !(self.editable && self.has_clipboard_text),
                Box::new(Paste),
            )
            .menu_with_disabled(
                "Delete",
                !(self.editable && self.has_selection),
                Box::new(Delete),
            )
            .separator()
            .menu_with_disabled("Select All", !self.has_text, Box::new(SelectAll))
    }
}

/// Builds a standard editor right-click menu (Undo, Redo, Cut, Copy, Paste, Delete, Select All)
/// for a composer `Input`, `Textarea`, or `Editor`, enabling/disabling each item based on live state.
pub(crate) fn editable_text_menu<M: InputModeKind>(
    state: Entity<InputBaseState<M>>,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    move |menu, window, cx| {
        let focus = state.read(cx).focus_handle(cx);
        if !focus.is_focused(window) {
            focus.focus(window, cx);
        }
        let has_clipboard_text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        let input = state.read(cx);
        let can_copy = input.is_copyable();
        let has_selection = can_copy || !input.selected_range().is_empty();
        let menu_state = EditableTextMenuState {
            editable: input.is_editable(),
            can_undo: input.can_undo(),
            can_redo: input.can_redo(),
            can_copy,
            has_selection,
            has_clipboard_text,
            has_text: input.text().len() > 0,
        };
        menu_state.build(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    #[test]
    fn read_only_email_menu_disables_edits_and_tracks_selection_and_content() {
        let empty = ReadOnlyEmailMenuState {
            has_selection: false,
            has_content: false,
            has_link: false,
        };
        assert_eq!(
            empty.specs(),
            vec![
                MenuSpec::Item { label: "Cut", enabled: false },
                MenuSpec::Item { label: "Copy", enabled: false },
                MenuSpec::Item { label: "Paste", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: false },
            ]
        );

        let selected = ReadOnlyEmailMenuState {
            has_selection: true,
            has_content: true,
            has_link: false,
        };
        assert_eq!(
            selected.specs(),
            vec![
                MenuSpec::Item { label: "Cut", enabled: false },
                MenuSpec::Item { label: "Copy", enabled: true },
                MenuSpec::Item { label: "Paste", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: true },
            ]
        );
    }

    #[test]
    fn read_only_email_menu_prepends_link_actions_when_over_link() {
        let on_link = ReadOnlyEmailMenuState {
            has_selection: false,
            has_content: true,
            has_link: true,
        };
        assert_eq!(
            on_link.specs(),
            vec![
                MenuSpec::Item { label: "Open Link", enabled: true },
                MenuSpec::Item { label: "Copy Link Address", enabled: true },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Cut", enabled: false },
                MenuSpec::Item { label: "Copy", enabled: false },
                MenuSpec::Item { label: "Paste", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: true },
            ]
        );
    }

    #[test]
    fn editable_menu_enables_items_only_when_applicable() {
        let idle = EditableTextMenuState {
            editable: true,
            can_undo: false,
            can_redo: false,
            can_copy: false,
            has_selection: false,
            has_clipboard_text: false,
            has_text: false,
        };
        assert_eq!(
            idle.specs(),
            vec![
                MenuSpec::Item { label: "Undo", enabled: false },
                MenuSpec::Item { label: "Redo", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Cut", enabled: false },
                MenuSpec::Item { label: "Copy", enabled: false },
                MenuSpec::Item { label: "Paste", enabled: false },
                MenuSpec::Item { label: "Delete", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: false },
            ]
        );

        let active = EditableTextMenuState {
            editable: true,
            can_undo: true,
            can_redo: true,
            can_copy: true,
            has_selection: true,
            has_clipboard_text: true,
            has_text: true,
        };
        assert_eq!(
            active.specs(),
            vec![
                MenuSpec::Item { label: "Undo", enabled: true },
                MenuSpec::Item { label: "Redo", enabled: true },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Cut", enabled: true },
                MenuSpec::Item { label: "Copy", enabled: true },
                MenuSpec::Item { label: "Paste", enabled: true },
                MenuSpec::Item { label: "Delete", enabled: true },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: true },
            ]
        );

        let readonly = EditableTextMenuState {
            editable: false,
            can_undo: true,
            can_redo: true,
            can_copy: true,
            has_selection: true,
            has_clipboard_text: true,
            has_text: true,
        };
        assert_eq!(
            readonly.specs(),
            vec![
                MenuSpec::Item { label: "Undo", enabled: false },
                MenuSpec::Item { label: "Redo", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Cut", enabled: false },
                MenuSpec::Item { label: "Copy", enabled: true },
                MenuSpec::Item { label: "Paste", enabled: false },
                MenuSpec::Item { label: "Delete", enabled: false },
                MenuSpec::Separator,
                MenuSpec::Item { label: "Select All", enabled: true },
            ]
        );
    }
}
