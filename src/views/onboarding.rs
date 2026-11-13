//! First-run onboarding / keymap picker overlay + shell-command install row. Crate-root child.

use crate::{
    apply_keymap, btn_primary, checkbox_box, div, keymap, overlay, pretty_key, px, shellcmd, theme,
    ui, Context, FerriteMCU, FluentBuilder, FontWeight, InteractiveElement, IntoElement,
    MouseButton, OpenKeymap, ParentElement, Preset, SharedString, Styled, Window,
};

const RECOMMENDED_LANGUAGES: &[crate::Lang] = &[
    crate::Lang::Json,
    crate::Lang::Rust,
    crate::Lang::Markdown,
    crate::Lang::Toml,
    crate::Lang::Yaml,
];

impl FerriteMCU {
    pub(crate) fn render_onboarding(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.onboarding.recommended_plugins {
            return self.render_recommended_plugins(ui, fs, cx);
        }
        let preset_card = |preset: Preset, cx: &mut Context<Self>| {
            let selected = self.onboarding.choice == preset;
            // Only the actions this preset binds (VSCode leaves Push / Pull unbound).
            let sample: Vec<gpui::AnyElement> = keymap::ACTIONS
                .iter()
                .filter_map(|a| {
                    let key = match preset {
                        Preset::VSCode => a.vscode,
                        _ => a.webstorm,
                    };
                    (!key.is_empty()).then_some((a, key))
                })
                .map(|(a, key)| {
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(a.label)),
                        )
                        // The key never shrinks: a long label truncates instead of pushing
                        // it past the card border.
                        .child(
                            div()
                                .flex_none()
                                .px_2()
                                .bg(theme::get().bg_mid)
                                .rounded_md()
                                .text_color(theme::get().line_number)
                                .child(SharedString::from(pretty_key(key))),
                        )
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap_2()
                // Off macOS the keys are words ("Ctrl+Alt+J"), wider than the glyphs: the
                // card widens so its longest row keeps its key inside the border.
                .w(px(if cfg!(target_os = "macos") {
                    300.0
                } else {
                    340.0
                }))
                .p_3()
                .rounded_lg()
                // Selection = thick accent border + a cool same-family gradient.
                .border_2()
                .border_color(if selected {
                    theme::get().primary
                } else {
                    theme::get().bg_light
                })
                .when(selected, |d| {
                    d.bg(gpui::linear_gradient(
                        145.0,
                        gpui::linear_color_stop(gpui::rgb(0x232838), 0.0),
                        gpui::linear_color_stop(gpui::rgb(0x2E3A5C), 1.0),
                    ))
                })
                .when(!selected, |d| d.bg(theme::get().panel_bg))
                .cursor_pointer()
                .child(
                    div()
                        .text_color(theme::get().text)
                        .child(SharedString::from(preset.label())),
                )
                .children(sample)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.onboarding.choice = preset;
                        cx.notify();
                    }),
                )
        };

        let panel = div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .text_color(theme::get().text)
            .child(
                div()
                    .text_color(theme::get().text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose your keymap"),
            )
            .child(
                div()
                    .text_color(theme::get().line_number)
                    .child(format!(
                        "{} ({}).",
                        if self.onboarding.forced {
                            "Pick a keymap and theme to get started. You can change these any time in FerriteMCU → Settings"
                        } else {
                            "Change your keymap, theme and more any time in FerriteMCU → Settings"
                        },
                        pretty_key("cmd-,")
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .child(preset_card(Preset::WebStorm, cx))
                    .child(preset_card(Preset::VSCode, cx)),
            )
            .child(self.theme_select(cx))
            .child(self.render_shell_command_row(cx))
            // Single primary action, bottom-right: confirm the highlighted choice.
            .child(
                ui::modal_footer().mt_2().child(
                    btn_primary("onboarding-continue", "Continue")
                        .px_5()
                        .py_1p5()
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                // Apply the shell-command checkbox before closing: only
                                // when ticked and a name is actually free to claim.
                                if this.onboarding.install_cmd
                                    && matches!(shellcmd::state(), shellcmd::State::Available(_))
                                {
                                    if let Err(e) = shellcmd::install() {
                                        this.onboarding.shell_cmd_error = Some(e);
                                    }
                                }
                                let choice = this.onboarding.choice;
                                this.choose_preset(choice, cx);
                            }),
                        ),
                ),
            );

        // Clicks inside the panel must not reach the backdrop.
        let panel = panel.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        overlay(cx, !self.onboarding.forced)
            .child(panel)
            .into_any_element()
    }

    fn render_recommended_plugins(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let names = RECOMMENDED_LANGUAGES
            .iter()
            .filter_map(|lang| lang.pack().map(|pack| pack.name))
            .collect::<Vec<_>>()
            .join(", ");
        let panel = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(460.0))
            .max_w_full()
            .p_4()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .text_color(theme::get().text)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Install recommended language plugins?"))
            .child(if names.is_empty() {
                "No recommended language plugins are included in this build.".to_string()
            } else {
                format!("Enable syntax highlighting and language features for {names}. You can change these later in Language Plugins.")
            })
            .when_some(self.onboarding.plugins_error.as_ref(), |d, error| {
                d.child(div().child(format!("Could not save your choice: {error}")))
            })
            .child(
                ui::modal_footer()
                    .child(crate::btn_secondary("recommended-skip", "Not now").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_recommended_plugins(false, cx)),
                    ))
                    .when(!names.is_empty(), |d| {
                        d.child(btn_primary("recommended-install", "Install plugins").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.finish_recommended_plugins(true, cx)),
                        ))
                    }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        overlay(cx, false).child(panel).into_any_element()
    }

    fn finish_recommended_plugins(&mut self, install: bool, cx: &mut Context<Self>) {
        let mut plugins = self.plugins.clone();
        if install {
            for lang in RECOMMENDED_LANGUAGES {
                if let Some(pack) = lang.pack() {
                    plugins.install(pack.id);
                }
            }
        }
        if let Err(error) = plugins.try_save() {
            self.onboarding.plugins_error = Some(error.to_string());
            cx.notify();
            return;
        }
        self.plugins = plugins;
        self.onboarding.recommended_plugins = false;
        self.onboarding.plugins_error = None;
        if install {
            for &lang in RECOMMENDED_LANGUAGES {
                self.rehighlight_for(lang, cx);
            }
        }
        cx.notify();
    }

    /// One row in the keymap picker: a checkbox to install a `fmc`/`ferritemcu` shell
    /// launcher (symlink into ~/.local/bin, VSCode-style). Shown on both first
    /// run and reopened Settings; the symlink is created when Continue confirms.
    /// Renders nothing when we can't resolve a location (`Unavailable`).
    fn render_shell_command_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let st = shellcmd::state();
        if matches!(st, shellcmd::State::Unavailable) {
            return div().into_any_element();
        }

        // Visual state of the box: installed → always on (and locked); a free
        // name → reflects the pending checkbox; taken → off (and locked).
        let (checked, enabled) = match &st {
            shellcmd::State::Installed(_) => (true, false),
            shellcmd::State::Available(_) => (self.onboarding.install_cmd, true),
            _ => (false, false),
        };
        let label = match &st {
            shellcmd::State::Installed(n) => {
                format!("Shell command installed — run `{n}` in any terminal")
            }
            shellcmd::State::Available(n) => {
                format!("Install `{n}` command — open FerriteMCU from any terminal")
            }
            _ => "`fmc` and `ferritemcu` are already taken on your PATH — skipped".to_string(),
        };

        let checkbox = checkbox_box(checked);

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pt_3()
            .mt_1()
            .border_t_1()
            .border_color(t.divider)
            .child(checkbox)
            .child(
                div()
                    .text_color(if enabled {
                        t.secondary_text
                    } else {
                        t.line_number
                    })
                    .child(SharedString::from(label)),
            );

        if enabled {
            row = row.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.onboarding.install_cmd = !this.onboarding.install_cmd;
                    cx.notify();
                }),
            );
        }

        let mut col = div().flex().flex_col().gap_1().child(row);
        if let Some(err) = &self.onboarding.shell_cmd_error {
            col = col.child(
                div()
                    .text_color(t.status_deleted)
                    .text_size(px(12.0))
                    .child(SharedString::from(err.clone())),
            );
        }
        col.into_any_element()
    }

    // ── keymap / onboarding ───────────────────────────────────────
    /// ⌘, / the native Settings… menu → open the Settings window. (First-run keymap selection
    /// still uses the forced onboarding overlay, opened from `new()`, not this path.)
    pub(crate) fn open_keymap(&mut self, _: &OpenKeymap, _: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(cx);
    }

    pub(crate) fn choose_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.keymap.set_preset(preset);
        self.keymap.save();
        apply_keymap(cx, &self.keymap);
        self.onboarding.open = false;
        self.onboarding.forced = false;
        cx.notify();
    }
}
