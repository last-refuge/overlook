//! Confirmations (`window.prompt`) drawn in the app's theme. gpui's own fallback is a fixed
//! white card that clips long titles; this replaces it for every prompt in the app.

use super::*;

/// Registers the themed prompt for every window.
pub fn init(cx: &mut App) {
    cx.set_prompt_builder(|level, message, detail, actions, handle, window, cx| {
        let view = cx.new(|cx| ThemedPrompt { level, message: message.into(), detail: detail.map(|d| d.to_owned().into()), actions: actions.to_vec(), focus: cx.focus_handle() });
        handle.with_view(view, window, cx)
    });
}

struct ThemedPrompt {
    level: PromptLevel,
    message: SharedString,
    detail: Option<SharedString>,
    actions: Vec<PromptButton>,
    focus: FocusHandle,
}

impl ThemedPrompt {
    /// The button Esc and a click outside choose: the Cancel button, or else the last one
    /// ("Not now").
    fn dismiss_index(&self) -> usize {
        self.actions.iter().position(PromptButton::is_cancel).unwrap_or(self.actions.len().saturating_sub(1))
    }

    /// The button Enter chooses: the first that isn't Cancel.
    fn default_index(&self) -> usize {
        self.actions.iter().position(|a| !a.is_cancel()).unwrap_or(0)
    }
}

impl EventEmitter<PromptResponse> for ThemedPrompt {}

impl Focusable for ThemedPrompt {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ThemedPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let (default, dismiss) = (self.default_index(), self.dismiss_index());
        // A warning's main button is the destructive one (Delete, Empty).
        let destructive = matches!(self.level, PromptLevel::Warning | PromptLevel::Critical);
        // Cancel first, so the main action sits at the right edge.
        let mut order: Vec<usize> = (0..self.actions.len()).collect();
        order.sort_by_key(|&i| (i != dismiss, i));
        let buttons = order.into_iter().map(|ix| {
            let button = Button::new(("prompt-action", ix)).small().label(self.actions[ix].label().clone());
            let button = if ix == default {
                if destructive { button.danger() } else { button.primary() }
            } else {
                button.ghost()
            };
            button.on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(PromptResponse(ix));
            }))
        });
        div()
            .id("prompt-overlay")
            // Drawn outside the window's own tree, so it doesn't inherit the app font.
            .font_family("Inter")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PromptResponse(dismiss))))
            .child(
                v_flex()
                    .id("prompt")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(move |_, event: &KeyDownEvent, _, cx| {
                        let pick = match event.keystroke.key.as_str() {
                            "escape" => dismiss,
                            "enter" => default,
                            _ => return,
                        };
                        cx.stop_propagation();
                        cx.emit(PromptResponse(pick));
                    }))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .w(px(420.))
                    .max_w(relative(0.94))
                    .rounded(px(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .p(px(20.))
                    .gap(px(8.))
                    .text_color(p.foreground)
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(self.message.clone()))
                    .children(self.detail.clone().map(|d| div().text_size(px(13.)).text_color(p.muted).child(d)))
                    .child(h_flex().mt(px(8.)).justify_end().gap(px(8.)).children(buttons)),
            )
    }
}
