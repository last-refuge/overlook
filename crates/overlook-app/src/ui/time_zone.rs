use super::*;
use gpui_kit::component::Disableable;
use overlook_core::tz;

impl MailWindow {
    pub(super) fn change_time_zone(
        &mut self,
        choice: Option<tz::ZoneDefinition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.time_zone_pending {
            return;
        }
        let detected = self.time_zone_prompt.clone();
        let definition = choice
            .clone()
            .or_else(|| detected.as_ref().map(|zone| zone.definition().clone()))
            .unwrap_or_else(tz::system_definition);
        if definition.resolve().is_err() {
            return;
        }
        let mut prefs = self.prefs.clone();
        prefs.time_zone = choice.clone();
        let store = self.store.clone();
        let previous = self.time_zone.clone();
        self.persist_time_zone(
            window,
            cx,
            move || {
                store
                    .set_time_zone_preferences(&previous, &definition, &prefs)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
            move |this, result, window, cx| match result {
                Ok(()) => {
                    // Other preferences can change while the writer is running.
                    this.prefs.time_zone = choice.clone();
                    // Pin first when keeping the old zone; accepting follows the
                    // detected zone after its calendar projection has committed.
                    tz::set_override(choice.as_ref()).expect("the selected zone was validated");
                    if let Some(detected) = detected {
                        tz::update_system_zone(detected);
                    }
                    this.time_zone_prompt = None;
                    this.refresh_time_zone(window, cx);
                }
                Err(error) => toast(
                    window,
                    cx,
                    crate::notify::Level::Error,
                    format!("Couldn’t save the time zone: {error}"),
                ),
            },
        );
    }

    fn persist_time_zone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        write: impl FnOnce() -> Result<(), String> + Send + 'static,
        complete: impl FnOnce(&mut Self, Result<(), String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.time_zone_pending = true;
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::persist::later(move || {
            let _ = tx.send(write());
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx
                .await
                .unwrap_or_else(|_| Err("The settings writer stopped.".into()));
            this.update_in(cx, |this, window, cx| {
                this.time_zone_pending = false;
                complete(this, result, window, cx);
                if let Some(input) = this
                    .settings
                    .as_ref()
                    .map(|settings| settings.time_zone.clone())
                {
                    let choices = settings::time_zone_choices(&this.prefs.time_zone);
                    input.update(cx, |state, cx| {
                        state.set_items(
                            gpui_kit::component::searchable_list::SearchableVec::new(choices),
                            window,
                            cx,
                        );
                        state.set_selected_values(
                            std::slice::from_ref(&this.prefs.time_zone),
                            window,
                            cx,
                        );
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn watch_system_time_zone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| loop {
            // Zone detection can read TZif files. Keep it off the UI thread and out of render.
            let detected = cx
                .background_spawn(async { tz::detect_system_zone() })
                .await;
            if this
                .update_in(cx, |this, window, cx| {
                    if let Some(detected) = detected.filter(|_| !this.time_zone_pending) {
                        if this.prefs.time_zone.is_none() && detected.zone() != &this.time_zone {
                            // Discovery alone never changes the effective zone.
                            this.time_zone_prompt = Some(detected);
                        } else {
                            this.time_zone_prompt = None;
                            tz::update_system_zone(detected);
                            this.refresh_time_zone(window, cx);
                        }
                    }
                    // Midnight and OS clock corrections also refresh relative dates and clocks.
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_secs(60))
                .await;
        })
        .detach();
    }

    pub(super) fn render_time_zone_prompt(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let detected = self.time_zone_prompt.as_ref()?;
        let p = *cx.palette();
        let description = format!(
            "Your system time zone changed to {} (UTC{}). Change Overlook from {} (UTC{})?",
            tz::zone_label(detected.zone()),
            tz::in_zone(now(), detected.zone()).strftime("%:z"),
            tz::zone_label(&self.time_zone),
            tz::in_zone(now(), &self.time_zone).strftime("%:z"),
        );
        Some(
            h_flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .px(px(16.))
                .py(px(10.))
                .bg(p.chip)
                .border_b_1()
                .border_color(p.divider)
                .child(div().flex_1().min_w(px(220.)).child(description))
                .child(
                    Button::new("accept-time-zone")
                        .primary()
                        .small()
                        .disabled(self.time_zone_pending)
                        .label("Change time zone")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.change_time_zone(None, window, cx)
                        })),
                )
                .child(
                    Button::new("keep-time-zone")
                        .small()
                        .disabled(self.time_zone_pending)
                        .label("Keep current")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let current = tz::current_definition();
                            let kept = this
                                .time_zone
                                .iana_name()
                                .and_then(|name| {
                                    let named = tz::ZoneDefinition::Named(name.into());
                                    named
                                        .resolve()
                                        .ok()
                                        .filter(|zone| zone == &this.time_zone)
                                        .map(|_| named)
                                })
                                .unwrap_or(current);
                            this.change_time_zone(Some(kept), window, cx);
                        })),
                ),
        )
    }

    fn refresh_time_zone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.time_zone_revision == tz::revision() {
            return;
        }
        let previous = self.time_zone.clone();
        let date = tz::in_zone(self.cal_anchor, &previous).date();
        let was_today = date == tz::in_zone(now(), &previous).date();
        self.time_zone = tz::current_zone();
        self.time_zone_revision = tz::revision();
        self.cal_anchor = if was_today {
            tz::day_start(now())
        } else {
            tz::date_start(date)
        };
        self.cal_scrolled_for.set(None);
        self.cal_drag = None;
        self.rows_key.borrow_mut().take();
        if let Some(original) = self
            .new_event
            .as_mut()
            .and_then(|form| form.original.as_mut())
        {
            tz::reproject_all_day(original, &previous, &self.time_zone).ok();
        }
        self.reload_calendar();
        self.sync_all(window, cx);
        cx.notify();
    }
}
