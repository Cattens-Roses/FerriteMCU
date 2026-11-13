//! Native Espressif workflow integration.
//!
//! Device operations use the upstream Rust libraries directly. Cargo remains a
//! background child process because Cargo explicitly does not provide a stable
//! embeddable build API. Tool output is routed into FerriteMCU's terminal panel.

use crate::*;
use gpui::prelude::*;
use std::{
    io::Read,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub(super) const ESP_CHIPS: &[&str] = &[
    "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32s2",
    "esp32s3", "esp32s31", "esp32p4",
];

impl FerriteMCU {
    pub(crate) fn render_embedded_board_button(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let root = self.repo_root.as_ref()?;
        if !root.join("Cargo.toml").is_file() {
            return None;
        }
        let button = ui::icon_btn("esp-board-settings", 30.0)
            .tooltip(ui::tip("Board and serial settings"))
            .child(
                svg()
                    .path("icons/esp-board.svg")
                    .size(px(17.0))
                    .text_color(theme::get().secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.open_embedded_settings(cx);
                }),
            );
        Some(button.into_any_element())
    }

    fn open_embedded_settings(&mut self, cx: &mut Context<Self>) {
        self.embedded.board_picker_open = false;
        self.embedded.port_picker_open = false;
        self.refresh_embedded_ports(cx);
        self.open_modal_window(ModalKind::EspBoard, "ESP Board Settings", 580.0, 600.0, cx);
    }

    pub(crate) fn render_embedded_toolbar(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let root = self.repo_root.as_ref()?;
        if !root.join("Cargo.toml").is_file() {
            return None;
        }
        let t = theme::get();
        let busy = self.embedded.busy.is_some();
        let monitor_is_open = self.embedded.monitor_stop.is_some();
        let flash = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .child(
                div()
                    .id("esp-flash")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(30.0))
                    .rounded_lg()
                    .when(!busy, |d| {
                        d.bg(gpui::rgb(0x16A34A))
                            .hover(|d| d.bg(gpui::rgb(0x15803D)))
                            .cursor_pointer()
                    })
                    .when(busy, |d| d.opacity(0.55))
                    .tooltip(ui::tip(if busy {
                        "ESP operation running"
                    } else {
                        "Flash firmware"
                    }))
                    .child(
                        svg()
                            .path("icons/play-circle.svg")
                            .size(px(18.0))
                            .text_color(gpui::rgb(0xFFFFFF)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.start_embedded_operation(EmbeddedOperation::Flash, cx);
                        }),
                    ),
            )
            .child(
                ui::icon_btn("esp-actions-dropdown", 24.0)
                    .tooltip(ui::tip("Build and debug actions"))
                    .child(
                        svg()
                            .path("icons/chevron-down.svg")
                            .size(px(14.0))
                            .text_color(t.secondary_text),
                    )
                    .when(busy, |d| d.opacity(0.55))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                            if this.embedded.busy.is_none() {
                                this.open_menu(e.position, MenuTarget::EmbeddedActions, cx);
                            }
                        }),
                    ),
            );
        let monitor =
            ui::icon_btn("esp-serial-monitor", 30.0)
                .tooltip(ui::tip(if monitor_is_open {
                    "Stop serial monitor"
                } else {
                    "Open serial monitor"
                }))
                .child(svg().path("icons/monitor.svg").size(px(17.0)).text_color(
                    if monitor_is_open {
                        gpui::rgb(0x22C55E)
                    } else {
                        t.secondary_text.into()
                    },
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_embedded_monitor(cx);
                    }),
                );
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .pr_1()
                .child(flash)
                .child(monitor)
                .into_any_element(),
        )
    }

    pub(crate) fn render_esp_board_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let chip = self.embedded.chip.to_uppercase().replace("ESP", "ESP ");
        let port = self
            .embedded
            .selected_port
            .as_deref()
            .unwrap_or("Not selected")
            .to_string();
        let row = |title: &'static str, subtitle: &'static str, button: gpui::AnyElement| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .px_4()
                .py_3()
                .rounded_lg()
                .bg(t.main_bg)
                .border_1()
                .border_color(t.divider)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_color(t.text).child(title))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(t.line_number)
                                .child(subtitle),
                        ),
                )
                .child(button)
        };
        let mut board_options = div()
            .id("esp-board-options")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(170.0))
            .overflow_y_scroll()
            .p_2()
            .rounded_lg()
            .bg(t.main_bg)
            .border_1()
            .border_color(t.divider);
        for board in ESP_CHIPS {
            let selected = self.embedded.chip == *board;
            let value = *board;
            board_options = board_options.child(
                div()
                    .id(SharedString::from(format!("esp-board-option-{value}")))
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_light))
                    .hover(|d| d.bg(t.bg_light))
                    .text_color(t.text)
                    .child(if selected { "✓  " } else { "   " })
                    .child(value.to_uppercase().replace("ESP", "ESP "))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.embedded.chip = value;
                            this.embedded.board_picker_open = false;
                            this.embedded.message = format!("Selected {value}");
                            cx.notify();
                        }),
                    ),
            );
        }
        let mut port_options = div()
            .id("esp-port-options")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(170.0))
            .overflow_y_scroll()
            .p_2()
            .rounded_lg()
            .bg(t.main_bg)
            .border_1()
            .border_color(t.divider);
        if self.embedded.ports.is_empty() {
            port_options = port_options.child(div().px_3().py_2().text_color(t.line_number).child(
                if self.embedded.message.contains("Scanning") {
                    "Scanning serial ports…"
                } else {
                    "No serial ports found"
                },
            ));
        }
        for available_port in &self.embedded.ports {
            let port_name = available_port.clone();
            let selected = self.embedded.selected_port.as_ref() == Some(available_port);
            let label = port_name.clone();
            port_options = port_options.child(
                div()
                    .id(SharedString::from(format!("esp-port-option-{port_name}")))
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_light))
                    .hover(|d| d.bg(t.bg_light))
                    .text_color(t.text)
                    .child(if selected { "✓  " } else { "   " })
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.embedded.selected_port = Some(port_name.clone());
                            this.embedded.port_picker_open = false;
                            this.embedded.message = format!("Selected {port_name}");
                            cx.notify();
                        }),
                    ),
            );
        }
        let mut profile_options = div()
            .id("esp-profile-options")
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_lg()
            .bg(t.main_bg)
            .border_1()
            .border_color(t.divider);
        for (label, release) in [("Debug", false), ("Release", true)] {
            let selected = self.embedded.release_build == release;
            profile_options = profile_options.child(
                div()
                    .id(if release {
                        "esp-profile-release"
                    } else {
                        "esp-profile-debug"
                    })
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_light))
                    .hover(|d| d.bg(t.bg_light))
                    .text_color(t.text)
                    .child(if selected { "✓  " } else { "   " })
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.embedded.release_build = release;
                            this.embedded.advanced_picker = None;
                            this.embedded.message = format!("Build profile: {label}");
                            cx.notify();
                        }),
                    ),
            );
        }
        let mut flash_baud_options = div()
            .id("esp-flash-baud-options")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(130.0))
            .overflow_y_scroll()
            .p_2()
            .rounded_lg()
            .bg(t.main_bg)
            .border_1()
            .border_color(t.divider);
        for baud in [115_200, 230_400, 460_800, 921_600] {
            let selected = self.embedded.flash_baud_rate == baud;
            flash_baud_options = flash_baud_options.child(
                div()
                    .id(SharedString::from(format!("esp-flash-baud-{baud}")))
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_light))
                    .hover(|d| d.bg(t.bg_light))
                    .text_color(t.text)
                    .child(if selected { "✓  " } else { "   " })
                    .child(format!("{baud} baud"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.embedded.flash_baud_rate = baud;
                            this.embedded.advanced_picker = None;
                            this.embedded.message = format!("Flash speed: {baud} baud");
                            cx.notify();
                        }),
                    ),
            );
        }
        let mut monitor_baud_options = div()
            .id("esp-monitor-baud-options")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(150.0))
            .overflow_y_scroll()
            .p_2()
            .rounded_lg()
            .bg(t.main_bg)
            .border_1()
            .border_color(t.divider);
        for baud in [
            9_600, 19_200, 38_400, 57_600, 74_880, 115_200, 230_400, 460_800,
        ] {
            let selected = self.embedded.monitor_baud_rate == baud;
            monitor_baud_options = monitor_baud_options.child(
                div()
                    .id(SharedString::from(format!("esp-monitor-baud-{baud}")))
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_light))
                    .hover(|d| d.bg(t.bg_light))
                    .text_color(t.text)
                    .child(if selected { "✓  " } else { "   " })
                    .child(format!("{baud} baud"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.embedded.monitor_baud_rate = baud;
                            this.embedded.advanced_picker = None;
                            this.embedded.message = format!("Monitor speed: {baud} baud");
                            cx.notify();
                        }),
                    ),
            );
        }
        let advanced_button = |id: &'static str,
                               label: String,
                               picker: EmbeddedAdvancedPicker,
                               cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(t.bg_mid)
                .hover(|d| d.bg(t.bg_light))
                .cursor_pointer()
                .text_color(t.text)
                .child(label)
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size(px(14.0))
                        .text_color(t.secondary_text),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.embedded.advanced_picker =
                            (this.embedded.advanced_picker != Some(picker)).then_some(picker);
                        cx.notify();
                    }),
                )
                .into_any_element()
        };
        let profile_picker = advanced_button(
            "esp-profile-picker",
            if self.embedded.release_build {
                "Release"
            } else {
                "Debug"
            }
            .into(),
            EmbeddedAdvancedPicker::BuildProfile,
            cx,
        );
        let flash_baud_picker = advanced_button(
            "esp-flash-baud-picker",
            format!("{} baud", self.embedded.flash_baud_rate),
            EmbeddedAdvancedPicker::FlashBaud,
            cx,
        );
        let monitor_baud_picker = advanced_button(
            "esp-monitor-baud-picker",
            format!("{} baud", self.embedded.monitor_baud_rate),
            EmbeddedAdvancedPicker::MonitorBaud,
            cx,
        );
        let auto_monitor = div()
            .id("esp-auto-monitor")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .text_color(t.secondary_text)
            .child(checkbox_box(self.embedded.monitor_after_flash))
            .child("Open serial monitor after flashing")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.embedded.monitor_after_flash = !this.embedded.monitor_after_flash;
                    cx.notify();
                }),
            );
        let advanced_panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row(
                "Build profile",
                "Debug builds faster; Release builds are optimized for firmware.",
                profile_picker,
            ))
            .when(
                self.embedded.advanced_picker == Some(EmbeddedAdvancedPicker::BuildProfile),
                |d| d.child(profile_options),
            )
            .child(row(
                "Flash speed",
                "Higher speeds are faster but may not work with every USB adapter.",
                flash_baud_picker,
            ))
            .when(
                self.embedded.advanced_picker == Some(EmbeddedAdvancedPicker::FlashBaud),
                |d| d.child(flash_baud_options),
            )
            .child(row(
                "Serial monitor speed",
                "Must match the baud rate configured by your firmware.",
                monitor_baud_picker,
            ))
            .when(
                self.embedded.advanced_picker == Some(EmbeddedAdvancedPicker::MonitorBaud),
                |d| d.child(monitor_baud_options),
            )
            .child(auto_monitor);
        let advanced_header = div()
            .id("esp-advanced-options-toggle")
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .text_color(t.text)
            .child("Build and serial options")
            .child(
                svg()
                    .path(if self.embedded.advanced_open {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    })
                    .size(px(15.0))
                    .text_color(t.secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.embedded.advanced_open = !this.embedded.advanced_open;
                    if !this.embedded.advanced_open {
                        this.embedded.advanced_picker = None;
                    }
                    cx.notify();
                }),
            );
        let board_picker = div()
            .id("esp-window-board-picker")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(t.bg_mid)
            .hover(|d| d.bg(t.bg_light))
            .cursor_pointer()
            .child(chip)
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size(px(14.0))
                    .text_color(t.secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.embedded.board_picker_open = !this.embedded.board_picker_open;
                    this.embedded.port_picker_open = false;
                    this.embedded.advanced_picker = None;
                    cx.notify();
                }),
            )
            .into_any_element();
        let port_picker = div()
            .id("esp-window-port-picker")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(t.bg_mid)
            .hover(|d| d.bg(t.bg_light))
            .cursor_pointer()
            .child(port)
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size(px(14.0))
                    .text_color(t.secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.embedded.port_picker_open = !this.embedded.port_picker_open;
                    this.embedded.board_picker_open = false;
                    this.embedded.advanced_picker = None;
                    cx.notify();
                }),
            )
            .into_any_element();
        let refresh = btn_secondary("esp-window-refresh", "Refresh ports").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.refresh_embedded_ports(cx);
            }),
        );
        let new_project = btn_secondary("esp-window-new-project", "New ESP project").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.open_esp_generator(window, cx);
            }),
        );
        let setup = btn_primary("esp-window-setup", "Setup toolchain").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.start_embedded_operation(EmbeddedOperation::Setup, cx);
            }),
        );
        let status = self
            .embedded
            .busy
            .as_deref()
            .unwrap_or(&self.embedded.message)
            .to_string();

        div()
            .id("esp-settings-body")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .bg(t.panel_bg)
            .text_size(px(13.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .pb_2()
                    .child(
                        svg()
                            .path("icons/esp-board.svg")
                            .size(px(24.0))
                            .text_color(t.primary),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(17.0))
                                    .text_color(t.text)
                                    .child("ESP hardware"),
                            )
                            .child(
                                div()
                                    .text_color(t.secondary_text)
                                    .child("Board, serial port, and toolchain"),
                            ),
                    ),
            )
            .child(row(
                "Board",
                "Select the target MCU used for build and flash.",
                board_picker,
            ))
            .when(self.embedded.board_picker_open, |d| d.child(board_options))
            .child(row(
                "Serial port",
                "Choose the USB serial connection for flashing and monitor.",
                port_picker,
            ))
            .when(self.embedded.port_picker_open, |d| d.child(port_options))
            .child(advanced_header)
            .when(self.embedded.advanced_open, |d| d.child(advanced_panel))
            .child(div().flex().flex_row().justify_end().child(refresh))
            .child(div().h(px(1.0)).bg(t.divider))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(new_project)
                    .child(setup),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(t.secondary_text)
                    .child(status),
            )
            .into_any_element()
    }

    pub(super) fn start_embedded_operation(
        &mut self,
        operation: EmbeddedOperation,
        cx: &mut Context<Self>,
    ) {
        if self.embedded.busy.is_some() {
            return;
        }
        let stop_after_flash = operation == EmbeddedOperation::Flash
            && self.embedded.monitor_stop.take().is_some_and(|stop| {
                stop.store(true, Ordering::Release);
                true
            });
        if stop_after_flash {
            self.write_embedded_terminal_output(
                "\r\n[ESP] Stopping serial monitor before flashing…\r\n",
                cx,
            );
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        let chip = self.embedded.chip.to_string();
        let port = self.embedded.selected_port.clone();
        let release_build = self.embedded.release_build;
        let flash_baud_rate = self.embedded.flash_baud_rate;
        self.embedded.busy = Some(format!("{}…", operation.label()));
        self.embedded.message.clear();
        self.write_embedded_terminal_output(
            &format!("\r\n\x1b[1;36m[ESP] {}\x1b[0m\r\n", operation.label()),
            cx,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let (output_tx, mut output_rx) = futures::channel::mpsc::unbounded::<String>();
            let operation_task = cx.background_executor().spawn(async move {
                if stop_after_flash {
                    // The monitor's serial read has a 200 ms timeout; allow its dedicated
                    // thread to observe cancellation and release the COM port before opening it.
                    std::thread::sleep(Duration::from_millis(250));
                }
                execute_embedded_operation(
                    operation,
                    &root,
                    &chip,
                    port.as_deref(),
                    release_build,
                    flash_baud_rate,
                    |chunk| {
                        let _ = output_tx.unbounded_send(chunk.to_owned());
                    },
                )
            });
            while let Some(chunk) = output_rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.write_embedded_terminal_output(&chunk, cx);
                });
            }
            let result = operation_task.await;
            this.update(cx, |this, cx| {
                let start_monitor_after_flash = operation == EmbeddedOperation::Flash
                    && result.is_ok()
                    && this.embedded.monitor_after_flash;
                this.embedded.busy = None;
                this.embedded.message = match result {
                    Ok(output) => {
                        this.write_embedded_terminal_output(&format!("{output}\r\n"), cx);
                        concise_output(&output, "Completed")
                    }
                    Err(error) => {
                        this.write_embedded_terminal_output(
                            &format!("\x1b[1;31m[ESP error]\x1b[0m {error}\r\n"),
                            cx,
                        );
                        format!("Error: {}", concise_output(&error, "Operation failed"))
                    }
                };
                if start_monitor_after_flash {
                    this.toggle_embedded_monitor(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Tool output belongs in the same interactive terminal used by the user's shell.
    /// Opening the panel lazily preserves the normal editor layout until an ESP action runs.
    fn write_embedded_terminal_output(&mut self, output: &str, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        {
            let console = self
                .term
                .tabs
                .iter()
                .position(|tab| tab.read(cx).output_only);
            let console = match console {
                Some(index) => index,
                None => {
                    let view = cx.new(|cx| crate::terminal::TerminalView::output_only(cx));
                    self.term.tabs.push(view);
                    self.term.panel.on_tab_added(self.term.tabs.len());
                    self.term.tabs.len() - 1
                }
            };
            if self.term.panel.active != console || !self.term.panel.open {
                self.term.panel.active = console;
                self.term.panel.open = true;
                self.term.panel.focus_pending = true;
            }
            if let Some(view) = self.term.tabs.get(console).cloned() {
                view.update(cx, |terminal, cx| terminal.write_output(output, cx));
            }
        }
        #[cfg(not(feature = "terminal"))]
        let _ = (output, cx);
    }

    pub(crate) fn refresh_embedded_ports(&mut self, cx: &mut Context<Self>) {
        self.embedded.message = "Scanning serial ports…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ports = cx
                .background_executor()
                .spawn(async move {
                    serialport::available_ports()
                        .map(|ports| ports.into_iter().map(|p| p.port_name).collect::<Vec<_>>())
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                match ports {
                    Ok(ports) => {
                        this.embedded.ports = ports;
                        if this
                            .embedded
                            .selected_port
                            .as_ref()
                            .is_none_or(|p| !this.embedded.ports.contains(p))
                        {
                            this.embedded.selected_port = this.embedded.ports.first().cloned();
                        }
                        this.embedded.message = if this.embedded.ports.is_empty() {
                            "No serial ports found".into()
                        } else {
                            format!("{} serial port(s) found", this.embedded.ports.len())
                        };
                    }
                    Err(error) => this.embedded.message = format!("Port scan failed: {error}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_embedded_monitor(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = self.embedded.monitor_stop.take() {
            stop.store(true, Ordering::Release);
            self.embedded.message = "Serial monitor stopped".into();
            cx.notify();
            return;
        }
        let Some(port_name) = self.embedded.selected_port.clone() else {
            self.embedded.message = "Select a serial port first".into();
            cx.notify();
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (mut sender, mut receiver) =
            futures::channel::mpsc::channel::<Result<String, String>>(64);
        self.embedded.monitor_stop = Some(stop);
        self.embedded.message = format!("Connecting to {port_name}…");
        let baud_rate = self.embedded.monitor_baud_rate;
        self.write_embedded_terminal_output(
            &format!("\r\n\x1b[1;36m[Serial monitor: {port_name} @ {baud_rate} baud]\x1b[0m\r\n"),
            cx,
        );
        cx.notify();
        let monitor_thread = std::thread::Builder::new()
            .name("ferritemcu-serial-monitor".into())
            .spawn(move || {
                use futures::SinkExt;
                let mut serial = match serialport::new(port_name.clone(), baud_rate)
                    .timeout(Duration::from_millis(200))
                    .open()
                {
                    Ok(serial) => serial,
                    Err(error) => {
                        let _ = futures::executor::block_on(
                            sender.send(Err(format!("Could not open {port_name}: {error}"))),
                        );
                        return;
                    }
                };
                if futures::executor::block_on(sender.send(Ok(format!(
                    "Connected to {port_name} at {baud_rate} baud\r\n"
                ))))
                .is_err()
                {
                    return;
                }
                let mut buffer = [0u8; 1024];
                while !worker_stop.load(Ordering::Acquire) {
                    match serial.read(&mut buffer) {
                        Ok(0) => {}
                        Ok(count) => {
                            let text = String::from_utf8_lossy(&buffer[..count]).into_owned();
                            if futures::executor::block_on(sender.send(Ok(text))).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(error) => {
                            let _ = futures::executor::block_on(
                                sender.send(Err(format!("Serial read failed: {error}"))),
                            );
                            break;
                        }
                    }
                }
            });
        if let Err(error) = monitor_thread {
            self.embedded.monitor_stop = None;
            self.embedded.message = format!("Could not start serial monitor: {error}");
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let mut monitor_error = None;
            while let Some(event) = receiver.next().await {
                let keep_reading = this
                    .update(cx, |this, cx| {
                        if this.embedded.monitor_stop.is_none() {
                            return false;
                        }
                        let keep_reading = match event {
                            Ok(chunk) => {
                                this.write_embedded_terminal_output(&chunk, cx);
                                if this.embedded.message.starts_with("Connecting") {
                                    this.embedded.message = "Serial monitor connected".into();
                                }
                                true
                            }
                            Err(error) => {
                                this.embedded.message = format!("Serial monitor error: {error}");
                                this.write_embedded_terminal_output(
                                    &format!(
                                        "\r\n\x1b[1;31m[Serial monitor error]\x1b[0m {error}\r\n"
                                    ),
                                    cx,
                                );
                                monitor_error = Some(error);
                                false
                            }
                        };
                        cx.notify();
                        keep_reading
                    })
                    .unwrap_or(false);
                if !keep_reading {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if let Some(stop) = this.embedded.monitor_stop.take() {
                    stop.store(true, Ordering::Release);
                    if monitor_error.is_none() {
                        this.embedded.message = "Serial monitor disconnected".into();
                        this.write_embedded_terminal_output(
                            "\r\n\x1b[1;36m[Serial monitor disconnected]\x1b[0m\r\n",
                            cx,
                        );
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn stop_embedded_monitor(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = self.embedded.monitor_stop.take() {
            stop.store(true, Ordering::Release);
            self.embedded.message = "Serial monitor stopped".into();
            self.write_embedded_terminal_output(
                "\r\n\x1b[1;36m[Serial monitor stopped]\x1b[0m\r\n",
                cx,
            );
            cx.notify();
        }
    }

    pub(super) fn open_esp_generator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_name_prompt(NamePrompt::NewEspProject, window, cx);
    }

    pub(crate) fn generate_esp_project(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.repo_root.clone() else {
            return;
        };
        let destination = workspace.join(&name);
        let chip = self.embedded.chip.to_string();
        self.embedded.busy = Some("Generating embedded ESP-HAL project…".into());
        self.embedded.message.clear();
        self.write_embedded_terminal_output(
            &format!("\r\n\x1b[1;36m[ESP] Generating project `{name}` for {chip}…\x1b[0m\r\n"),
            cx,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    generate_esp_hal_project(&destination, &name, &chip)
                        .map(|()| format!("ESP-HAL project created: {}", destination.display()))
                })
                .await;
            this.update(cx, |this, cx| {
                this.embedded.busy = None;
                this.embedded.message = match result {
                    Ok(message) => {
                        this.refresh(cx);
                        this.write_embedded_terminal_output(&format!("{message}\r\n"), cx);
                        message
                    }
                    Err(error) => {
                        this.write_embedded_terminal_output(
                            &format!("\x1b[1;31mProject generation failed: {error}\x1b[0m\r\n"),
                            cx,
                        );
                        format!("Project generation failed: {error}")
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddedOperation {
    Build,
    Flash,
    Debug,
    Setup,
}

impl EmbeddedOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Build => "Building firmware",
            Self::Flash => "Building and flashing",
            Self::Debug => "Building and starting debugger",
            Self::Setup => "Installing Espressif toolchain",
        }
    }
}

fn execute_embedded_operation(
    operation: EmbeddedOperation,
    root: &std::path::Path,
    chip: &str,
    port: Option<&str>,
    release_build: bool,
    flash_baud_rate: u32,
    mut on_output: impl FnMut(&str),
) -> Result<String, String> {
    match operation {
        EmbeddedOperation::Build => cargo_build(root, release_build, &mut on_output),
        EmbeddedOperation::Setup => {
            on_output("\r\n[ESP] Installing the Espressif Rust toolchain…\r\n");
            setup_esp_toolchain()
        }
        EmbeddedOperation::Flash => {
            let port = port.ok_or("Select a serial port before flashing")?;
            cargo_build(root, release_build, &mut on_output)?;
            let elf = find_firmware_elf(root, release_build)?;
            on_output(&format!(
                "\r\n[ESP] Flashing {} to {port}…\r\n",
                elf.display()
            ));
            flash_firmware(&elf, chip, port, flash_baud_rate, &mut on_output)?;
            Ok(format!("Flashed {} to {port}", elf.display()))
        }
        EmbeddedOperation::Debug => {
            cargo_build(root, release_build, &mut on_output)?;
            let elf = find_firmware_elf(root, release_build)?;
            on_output(&format!(
                "\r\n[ESP] Connecting debugger to {}…\r\n",
                elf.display()
            ));
            debug_firmware(&elf, chip)
        }
    }
}

/// Cargo has no stable library API for driving builds; keep the process isolated
/// in the background executor, capture output, and never block GPUI's main thread.
fn cargo_build(
    root: &std::path::Path,
    release: bool,
    on_output: &mut impl FnMut(&str),
) -> Result<String, String> {
    run_cargo(root, &["build"], release, on_output)
}

fn run_cargo(
    root: &std::path::Path,
    args: &[&str],
    release: bool,
    on_output: &mut impl FnMut(&str),
) -> Result<String, String> {
    let mut command = Command::new("cargo");
    if requires_esp_toolchain(root) {
        // A process-level RUSTUP_TOOLCHAIN override (common in IDE launches) can mask the
        // project's rust-toolchain.toml. Cargo's explicit +toolchain selector takes precedence
        // and is required for Espressif's Xtensa compiler/sysroot.
        command.arg("+esp");
        on_output("[ESP] Using Espressif `esp` Rust toolchain for Xtensa target.\r\n");
    }
    command.args(args);
    if release {
        command.arg("--release");
    }
    let mut child = command
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start Cargo: {error}. Install Rust using rustup."))?;

    let stdout = child.stdout.take().ok_or("Cargo stdout was not captured")?;
    let stderr = child.stderr.take().ok_or("Cargo stderr was not captured")?;
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    let stdout_sender = sender.clone();
    let stdout_reader = std::thread::Builder::new()
        .name("ferritemcu-cargo-stdout".into())
        .spawn(move || forward_process_output(stdout, stdout_sender))
        .map_err(|error| format!("Could not read Cargo output: {error}"))?;
    let stderr_reader = std::thread::Builder::new()
        .name("ferritemcu-cargo-stderr".into())
        .spawn(move || forward_process_output(stderr, sender))
        .map_err(|error| format!("Could not read Cargo errors: {error}"))?;

    while let Ok(chunk) = receiver.recv() {
        on_output(&chunk);
    }
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let status = child
        .wait()
        .map_err(|error| format!("Could not wait for Cargo to finish: {error}"))?;
    if status.success() {
        Ok(if release {
            "Cargo release build completed successfully".into()
        } else {
            "Cargo debug build completed successfully".into()
        })
    } else {
        Err(format!("Cargo exited with {status}"))
    }
}

fn requires_esp_toolchain(root: &std::path::Path) -> bool {
    [
        root.join(".cargo/config.toml"),
        root.join(".cargo/config"),
        root.join("rust-toolchain.toml"),
        root.join("rust-toolchain"),
    ]
    .iter()
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .any(|config| {
        let config = config.to_ascii_lowercase();
        config.contains("xtensa-")
            || config.lines().any(|line| {
                let line = line.trim();
                line.starts_with("channel") && line.contains("esp")
            })
    })
}

fn forward_process_output(mut input: impl Read, sender: std::sync::mpsc::Sender<String>) {
    let mut buffer = [0u8; 4096];
    loop {
        match input.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if sender
                    .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn setup_esp_toolchain() -> Result<String, String> {
    // The vendored espup manifest uses reqwest without selecting a TLS crypto
    // provider; install ring explicitly once for reqwest/rustls before download.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let targets = espup::targets::parse_targets("all")
        .map_err(|e| format!("Invalid ESP target list: {e}"))?;
    let options = espup::cli::InstallOpts {
        default_host: None,
        esp_riscv_gcc: false,
        export_file: None,
        disable_timeouts: false,
        extended_llvm: false,
        log_level: "info".into(),
        name: "esp".into(),
        stable_version: "stable".into(),
        skip_version_parse: false,
        std: false,
        targets,
        toolchain_version: None,
        crosstool_toolchain_version: None,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Could not initialize ESP setup runtime: {e}"))?;
    runtime
        .block_on(espup::toolchain::install(
            options,
            espup::toolchain::InstallMode::Install,
        ))
        .map_err(|e| format!("espup toolchain setup failed: {e}"))?;
    Ok("Espressif Rust toolchain installed. Restart FerriteMCU or reopen the project to refresh environment variables.".into())
}

fn flash_firmware(
    elf_path: &std::path::Path,
    chip_name: &str,
    port_name: &str,
    baud_rate: u32,
    on_output: &mut impl FnMut(&str),
) -> Result<(), String> {
    use espflash::{
        connection::{Connection, ResetAfterOperation, ResetBeforeOperation},
        flasher::{FlashData, FlashSettings},
        image_format::idf::IdfBootloaderFormat,
        target::{Chip, XtalFrequency},
    };
    use std::str::FromStr;
    let chip = Chip::from_str(chip_name)
        .map_err(|e| format!("Unsupported ESP chip `{chip_name}`: {e}"))?;
    let port_info = serialport::available_ports()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|info| info.port_name == port_name)
        .ok_or_else(|| format!("Serial port {port_name} is no longer available"))?;
    let usb_info = match port_info.port_type {
        serialport::SerialPortType::UsbPort(info) => info,
        _ => return Err("espflash requires a USB serial port with USB VID/PID information".into()),
    };
    let serial = open_espflash_port(port_name)?;
    let connection = Connection::new(
        serial,
        usb_info,
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        115_200,
    );
    let mut flasher = espflash::flasher::Flasher::connect(
        connection,
        true,
        true,
        false,
        Some(chip),
        Some(baud_rate),
    )
    .map_err(|e| format!("Could not connect to the ESP device: {e}"))?;
    on_output(&format!(
        "[ESP] Connected to {chip_name} on {port_name} at {baud_rate} baud. Preparing image…\r\n"
    ));
    let bytes = std::fs::read(elf_path).map_err(|e| format!("Could not read firmware ELF: {e}"))?;
    let flash_data = FlashData::new(
        FlashSettings::default(),
        0,
        None,
        chip,
        XtalFrequency::default(),
    );
    let image = IdfBootloaderFormat::new(&bytes, &flash_data, None, None, None, None)
        .map_err(|e| format!("Could not prepare firmware image: {e}"))?;
    let mut progress = TerminalFlashProgress {
        on_output,
        total_bytes: 0,
        last_percentage: None,
    };
    flasher
        .load_image_to_flash(&mut progress, image.into())
        .map_err(|e| format!("Flash failed: {e}"))
}

/// Reports real espflash transfer/checksum callbacks in the application's terminal.
struct TerminalFlashProgress<'a> {
    on_output: &'a mut dyn FnMut(&str),
    total_bytes: usize,
    last_percentage: Option<u8>,
}

impl espflash::target::ProgressCallbacks for TerminalFlashProgress<'_> {
    fn init(&mut self, address: u32, total: usize) {
        self.total_bytes = total;
        self.last_percentage = Some(0);
        (self.on_output)(&format!(
            "[ESP Flash] Writing {total} bytes at 0x{address:08X}: 0%\r"
        ));
    }

    fn update(&mut self, current: usize) {
        let percentage = if self.total_bytes == 0 {
            100
        } else {
            current
                .saturating_mul(100)
                .saturating_div(self.total_bytes)
                .min(100) as u8
        };
        if self.last_percentage != Some(percentage) {
            self.last_percentage = Some(percentage);
            (self.on_output)(&format!(
                "\r[ESP Flash] {percentage}% ({current}/{} bytes)",
                self.total_bytes
            ));
        }
    }

    fn verifying(&mut self) {
        (self.on_output)("\r\n[ESP Flash] Verifying written data…\r\n");
    }

    fn finish(&mut self, skipped: bool) {
        let status = if skipped {
            "complete (identical data skipped)"
        } else {
            "complete"
        };
        (self.on_output)(&format!("\r\n[ESP Flash] Flash {status}.\r\n"));
    }
}

#[cfg(windows)]
fn open_espflash_port(name: &str) -> Result<espflash::connection::Port, String> {
    serialport::new(name, 115_200)
        .timeout(Duration::from_secs(2))
        .open_native()
        .map_err(|e| format!("Could not open {name}: {e}"))
}
#[cfg(unix)]
fn open_espflash_port(name: &str) -> Result<espflash::connection::Port, String> {
    serialport::new(name, 115_200)
        .timeout(Duration::from_secs(2))
        .open_native()
        .map_err(|e| format!("Could not open {name}: {e}"))
}

fn debug_firmware(elf_path: &std::path::Path, chip: &str) -> Result<String, String> {
    let lister = probe_rs::probe::list::Lister::new();
    let probes = lister.list_all();
    let probe_info = probes.first().ok_or(
        "No supported debug probe found. Connect a JTAG probe (USB-UART adapters cannot debug).",
    )?;
    let probe = lister
        .open(probe_info.clone())
        .map_err(|e| format!("Could not open debug probe: {e}"))?;
    let mut session = probe
        .attach(chip, probe_rs::Permissions::default())
        .map_err(|e| format!("Could not attach to {chip}: {e}"))?;
    let bytes = std::fs::read(elf_path).map_err(|e| format!("Could not read firmware ELF: {e}"))?;
    let mut loader = session.target().flash_loader();
    loader
        .load_elf_data(&mut &bytes[..])
        .map_err(|e| format!("Could not prepare firmware for probe-rs: {e}"))?;
    loader
        .commit(&mut session, probe_rs::flashing::DownloadOptions::default())
        .map_err(|e| format!("probe-rs could not flash the firmware: {e}"))?;
    let mut core = session
        .core(0)
        .map_err(|e| format!("Could not access target core: {e}"))?;
    core.reset()
        .map_err(|e| format!("Could not reset target core: {e}"))?;
    core.run()
        .map_err(|e| format!("Could not resume target core: {e}"))?;
    Ok(format!("probe-rs flashed and started {chip}. The board is running; source-level breakpoints are not yet exposed in the editor."))
}

fn generate_esp_hal_project(
    destination: &std::path::Path,
    project_name: &str,
    chip: &str,
) -> Result<(), String> {
    use esp_generate::{config, manifest::Emit, plugin, process, Loaded, TemplateSource};
    use std::collections::HashSet;
    if !valid_project_name(project_name) {
        return Err("Project names may contain ASCII letters, digits, `_`, and `-`, and must start with a letter or digit.".into());
    }
    if destination.exists() {
        return Err(format!(
            "Destination already exists: {}",
            destination.display()
        ));
    }
    let loaded = Loaded::open(TemplateSource::Bundled)
        .map_err(|e| format!("Could not load bundled esp-generate template: {e}"))?;
    let flat = config::flatten_options(&loaded.template.options);
    let selected = vec![chip.to_string()];
    let missing = loaded.template.missing_required_groups(&selected);
    if !missing.is_empty() {
        return Err(format!(
            "ESP project template requires selection(s): {}",
            missing.join(", ")
        ));
    }
    let mut facts = loaded
        .resolved
        .facts(&plugin::selection(selected.clone(), &flat))
        .map_err(|e| format!("Could not resolve ESP template facts: {e}"))?;
    let options = loaded
        .template
        .all_options()
        .into_iter()
        .map(|o| o.name.clone())
        .chain(flat.iter().map(|o| o.name.clone()))
        .collect::<HashSet<_>>();
    let groups = loaded
        .template
        .all_options()
        .into_iter()
        .map(|o| o.selection_group.clone())
        .chain(flat.iter().map(|o| o.selection_group.clone()))
        .filter(|group| !group.is_empty())
        .collect::<HashSet<_>>();
    facts.vocabulary.options = Some(options);
    facts.vocabulary.groups = Some(groups);
    facts.set_value("generate_version", env!("CARGO_PKG_VERSION"));
    facts.set_value("project_name", project_name);
    facts.set_value(
        "generate_parameters",
        format!("--option {chip} {project_name}"),
    );
    facts.set_value("esp_hal_version_full", "1.2.2");
    let toolchain = if matches!(chip, "esp32" | "esp32s2" | "esp32s3") {
        "esp"
    } else {
        "stable"
    };
    facts.set_value("rust_toolchain", toolchain);
    let mut groups_selected = Vec::new();
    for selected_name in &selected {
        if let Some((_, option)) = config::find_option(selected_name, &flat) {
            if !option.selection_group.is_empty()
                && !groups_selected.contains(&option.selection_group)
            {
                groups_selected.push(option.selection_group.clone());
            }
            for (key, value) in &option.sets {
                if let Some(value) = value.as_scalar() {
                    facts.set_value(key.clone(), value);
                }
            }
        }
    }
    let renderer = process::Renderer::new(&selected, &groups_selected, &facts);
    let mut files = Vec::new();
    for (source_path, contents) in loaded.source.files().map_err(|e| e.to_string())? {
        let Emit::When { condition, output } = loaded.manifest.emit(&source_path) else {
            continue;
        };
        if let Some(condition) = condition {
            if !renderer
                .evaluate(condition, &format!("emit condition for {source_path}"))
                .map_err(|e| format!("{source_path}: {e}"))?
            {
                continue;
            }
        }
        let mut include = |path: &str| loaded.source.read(path).map(std::borrow::Cow::into_owned);
        let rendered = renderer
            .render(&contents, &mut include)
            .map_err(|e| format!("{source_path}: {e}"))?;
        let out_path = match output {
            Some(path) => renderer
                .output_path(path, &format!("output path for {source_path}"))
                .map_err(|e| e.to_string())?,
            None => source_path.clone(),
        };
        if !process::is_safe_relative_path(&out_path) {
            return Err(format!("Unsafe generated path: {out_path}"));
        }
        files.push((out_path, rendered));
    }
    let parent = destination
        .parent()
        .ok_or("Project destination has no parent directory")?;
    let staging = parent.join(format!(
        ".{}.ferritemcu-staging-{}",
        project_name,
        std::process::id()
    ));
    if staging.exists() {
        return Err(format!(
            "A previous generation is using the staging folder {}",
            staging.display()
        ));
    }
    std::fs::create_dir(&staging)
        .map_err(|e| format!("Could not create staging folder {}: {e}", staging.display()))?;
    let write_result = (|| {
        for (relative, contents) in files {
            let path = staging.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, contents)
                .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "Could not finish generated project at {}: {error}",
            destination.display()
        ));
    }
    Ok(())
}

fn valid_project_name(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn find_firmware_elf(root: &std::path::Path, release: bool) -> Result<std::path::PathBuf, String> {
    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;
    if !metadata.status.success() {
        return Err(String::from_utf8_lossy(&metadata.stderr).trim().to_string());
    }
    let json: serde_json::Value = serde_json::from_slice(&metadata.stdout)
        .map_err(|e| format!("Invalid cargo metadata: {e}"))?;
    let target_dir = json
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or("Cargo metadata omitted target directory")?;
    let default_id = json
        .get("workspace_default_members")
        .and_then(serde_json::Value::as_array)
        .and_then(|v| v.first())
        .and_then(serde_json::Value::as_str);
    let packages = json
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or("Cargo metadata omitted packages")?;
    let package = packages
        .iter()
        .find(|p| p.get("id").and_then(serde_json::Value::as_str) == default_id)
        .or_else(|| packages.first())
        .ok_or("Cargo workspace has no package")?;
    let names = package
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .ok_or("Cargo package has no targets")?
        .iter()
        .filter(|target| {
            target
                .get("kind")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|kinds| kinds.iter().any(|k| k == "bin"))
        })
        .filter_map(|target| target.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Err("No binary firmware target exists in this Cargo package".into());
    }
    let profile = if release { "release" } else { "debug" };
    let mut directories = vec![std::path::Path::new(target_dir).join(profile)];
    if let Ok(entries) = std::fs::read_dir(target_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|ty| ty.is_dir()) {
                directories.push(entry.path().join(profile));
            }
        }
    }
    for directory in directories {
        for name in &names {
            for candidate in [directory.join(name), directory.join(format!("{name}.elf"))] {
                if std::fs::read(&candidate).is_ok_and(|bytes| bytes.starts_with(b"\x7fELF")) {
                    return Ok(candidate);
                }
            }
        }
    }
    Err("Build completed but no ELF artifact was found. Confirm the package target is configured for an ESP chip.".into())
}

fn concise_output(output: &str, fallback: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return fallback.into();
    }
    let lines = trimmed.lines().rev().take(3).collect::<Vec<_>>();
    let mut text = lines.into_iter().rev().collect::<Vec<_>>().join(" · ");
    if text.len() > 220 {
        text.truncate(text.floor_char_boundary(217));
        text.push('…');
    }
    text
}
