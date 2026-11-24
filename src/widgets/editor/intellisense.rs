use super::*;
use std::time::Duration;

impl CodeEditor {
    pub(super) fn dismiss_intellisense(&mut self) {
        self.lsp_completions.clear();
        self.completion_request = None;
        self.lsp_hover = None;
        self.lsp_hover_offset = None;
        self.diagnostic_hover = None;
        self.hover_epoch = self.hover_epoch.wrapping_add(1);
    }

    pub(super) fn dismiss_suggestions(
        &mut self,
        _: &DismissSuggestions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.completion_request.is_some()
            || self.lsp_hover.is_some()
            || self.diagnostic_hover.is_some()
        {
            self.dismiss_intellisense();
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    pub(super) fn request_completion(&mut self, cx: &mut Context<Self>) {
        if self.read_only
            || self.single_line
            || self.lang != Lang::Rust
            || !self.selected_range.is_empty()
        {
            return;
        }
        self.lsp_hover = None;
        self.lsp_completions.clear();
        self.completion_request = Some((self.compute_gen, self.cursor()));
        cx.emit(EditorEvent::CompletionRequested(self.cursor()));
        cx.notify();
    }

    pub(super) fn schedule_completion(&mut self, typed: &str, cx: &mut Context<Self>) {
        if self.lang != Lang::Rust || self.single_line || self.read_only {
            return;
        }
        let caret = self.cursor();
        let preceding = self.content[..caret].chars().next_back();
        let trigger = preceding.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            || self.content[..caret].ends_with("::");
        // Only typing and deleting a prefix open suggestions; paste/newline do not.
        if !trigger || typed.chars().count() > 1 || typed.chars().any(char::is_whitespace) {
            return;
        }
        let generation = self.compute_gen;
        self.completion_request = Some((generation, caret));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(140))
                .await;
            let _ = this.update(cx, |editor, cx| {
                if editor.completion_request == Some((generation, caret))
                    && editor.compute_gen == generation
                    && editor.cursor() == caret
                    && editor.marked_range.is_none()
                {
                    editor.request_completion(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn navigate_completion(&mut self, direction: i32, cx: &mut Context<Self>) -> bool {
        if self.lsp_completions.is_empty() {
            return false;
        }
        self.completion_selected = if direction < 0 {
            self.completion_selected.saturating_sub(1)
        } else {
            (self.completion_selected + 1).min(self.lsp_completions.len() - 1)
        };
        self.completion_scroll
            .scroll_to_item(self.completion_selected);
        cx.notify();
        true
    }

    pub(super) fn accept_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((_, _, insert, range)) =
            self.lsp_completions.get(self.completion_selected).cloned()
        else {
            return false;
        };
        if self.read_only || self.completion_request != Some((self.compute_gen, self.cursor())) {
            self.dismiss_intellisense();
            return false;
        }
        self.replace_range_text(range, &insert, cx);
        true
    }

    pub(super) fn schedule_hover(&mut self, offset: Option<usize>, cx: &mut Context<Self>) {
        self.hover_epoch = self.hover_epoch.wrapping_add(1);
        let epoch = self.hover_epoch;
        let generation = self.compute_gen;
        // Invalidate a previous server response immediately, before the delay.
        cx.emit(EditorEvent::HoverRequested(None));
        cx.notify();
        let Some(offset) = offset else { return };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(450))
                .await;
            let _ = this.update(cx, |editor, cx| {
                if editor.hover_epoch == epoch
                    && editor.compute_gen == generation
                    && editor.lsp_hover_offset == Some(offset)
                    && editor.lsp_completions.is_empty()
                    && editor.diagnostic_hover.is_none()
                    && !editor.is_selecting
                {
                    cx.emit(EditorEvent::HoverRequested(Some(offset)));
                }
            });
        })
        .detach();
    }

    /// Window coordinates for the symbol's row, including folds and horizontal scrolling.
    fn popup_anchor(&self, offset: usize) -> Option<Point<Pixels>> {
        let bounds = self.bounds?;
        let row = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let start = *self.line_starts.get(row)?;
        let line = self.line_layouts.get(&row)?;
        let gutter = if self.gutter_right {
            px(0.0)
        } else {
            self.gutter_w
        };
        let anchor = point(
            bounds.left() + gutter + line.x_for_index(offset.saturating_sub(start)),
            bounds.top() + self.line_height * (row + 1) as f32 + px(4.0),
        );
        let viewport = self.viewport?;
        (anchor.y >= viewport.top() && anchor.y <= viewport.bottom() + self.line_height)
            .then_some(anchor)
    }

    pub(super) fn render_intellisense(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let completing = !self.lsp_completions.is_empty() && self.focus_handle.is_focused(window);
        let message = self.diagnostic_hover.as_ref().or(self.lsp_hover.as_ref());
        if !completing && message.is_none() {
            return None;
        }
        let offset = if completing {
            self.cursor()
        } else if self.diagnostic_hover.is_some() {
            self.offset_for_position(self.last_mouse?)
        } else {
            self.lsp_hover_offset?
        };
        let mut anchor = self.popup_anchor(offset)?;
        let viewport = self.viewport?;
        let width = px(480.0).min((viewport.size.width - px(24.0)).max(px(120.0)));
        let height = px(300.0).min((viewport.size.height - px(32.0)).max(px(60.0)));
        anchor.x = anchor
            .x
            .max(viewport.left() + px(8.0))
            .min((viewport.right() - width - px(8.0)).max(viewport.left() + px(8.0)));
        let above = anchor.y + height > viewport.bottom()
            && anchor.y - viewport.top() > viewport.bottom() - anchor.y;
        if above {
            anchor.y -= self.line_height + px(8.0);
        }
        let t = theme::get();
        let panel = div()
            .id("editor-intellisense")
            .flex()
            .flex_col()
            .w(width)
            .max_h(height)
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .bg(t.bg_mid)
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(13.0))
            .line_height(px(20.0))
            .text_color(t.text)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        let panel = if completing {
            panel
                .child(
                    div()
                        .id("completion-items")
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.completion_scroll)
                        .children(self.lsp_completions.iter().enumerate().map(
                            |(index, (label, detail, _, _))| {
                                div()
                                    .id(index)
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_3()
                                    .py_1()
                                    .min_h(px(30.0))
                                    .flex_shrink_0()
                                    .when(index == self.completion_selected, |d| {
                                        d.bg(t.selected_bg)
                                    })
                                    .hover(|d| d.bg(t.selected_bg))
                                    .cursor_pointer()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(theme::font::family())
                                            .child(label.clone()),
                                    )
                                    .child(
                                        div()
                                            .max_w(px(180.0))
                                            .truncate()
                                            .text_color(t.secondary_text)
                                            .child(detail.clone()),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |editor, _, _, cx| {
                                            cx.stop_propagation();
                                            editor.completion_selected = index;
                                            editor.accept_completion(cx);
                                        }),
                                    )
                            },
                        )),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(t.divider)
                        .text_size(px(11.0))
                        .text_color(t.secondary_text)
                        .child("↑ ↓ select   Enter / Tab insert   Esc close"),
                )
        } else {
            let title = if self.diagnostic_hover.is_some() {
                "Problem"
            } else {
                "Documentation"
            };
            let mut code_block = false;
            let lines = message?.lines().filter_map(|line| {
                if line.trim_start().starts_with("```") {
                    code_block = !code_block;
                    return None;
                }
                if line.trim().is_empty() {
                    return Some(div().h(px(8.0)).flex_shrink_0());
                }
                let heading = !code_block && line.starts_with('#');
                let text = if code_block {
                    line.to_owned()
                } else {
                    line.trim_start_matches('#').trim().replace('`', "")
                };
                Some(
                    div()
                        .flex_shrink_0()
                        .whitespace_normal()
                        .when(heading, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                        .when(code_block, |d| {
                            d.font_family(theme::font::family()).bg(t.bg_light).px_2()
                        })
                        .child(text),
                )
            });
            panel
                .child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(t.divider)
                        .text_size(px(11.0))
                        .text_color(t.secondary_text)
                        .child(title),
                )
                .child(
                    div()
                        .id("hover-documentation")
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_3()
                        .whitespace_normal()
                        .children(lines),
                )
        };
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(anchor)
                    .anchor(if above {
                        gpui::Corner::BottomLeft
                    } else {
                        gpui::Corner::TopLeft
                    })
                    .snap_to_window_with_margin(px(8.0))
                    .child(panel),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn completion_navigation_inserts_selected_item_and_preserves_undo(
        cx: &mut gpui::TestAppContext,
    ) {
        let editor = cx.new(|cx| CodeEditor::new(cx, "Dur".into(), Lang::Rust, ""));
        editor.update(cx, |editor, cx| {
            editor.move_to(3, cx);
            editor.request_completion(cx);
            editor.set_lsp_completions(
                vec![
                    ("Duration".into(), String::new(), "Duration".into(), 0..3),
                    (
                        "DurationError".into(),
                        String::new(),
                        "DurationError".into(),
                        0..3,
                    ),
                ],
                cx,
            );
            assert!(editor.navigate_completion(1, cx));
            assert!(editor.accept_completion(cx));
            assert_eq!(editor.text(), "DurationError");
            assert_eq!(editor.cursor(), "DurationError".len());
            assert!(editor.lsp_completions.is_empty());
            let snapshot = editor.undo_stack.pop_back().unwrap();
            editor.restore(snapshot, cx);
            assert_eq!(editor.text(), "Dur");
        });
    }

    #[gpui::test]
    fn late_completions_are_rejected_after_navigation_and_edits(cx: &mut gpui::TestAppContext) {
        let editor = cx.new(|cx| CodeEditor::new(cx, "Dur".into(), Lang::Rust, ""));
        editor.update(cx, |editor, cx| {
            editor.move_to(3, cx);
            editor.request_completion(cx);
            editor.move_to(0, cx);
            editor.set_lsp_completions(
                vec![("Duration".into(), String::new(), "Duration".into(), 0..3)],
                cx,
            );
            assert!(editor.lsp_completions.is_empty());
            editor.move_to(3, cx);
            editor.request_completion(cx);
            editor.replace_range_text(0..3, "let", cx);
            editor.set_lsp_completions(
                vec![("Duration".into(), String::new(), "Duration".into(), 0..3)],
                cx,
            );
            assert!(!editor.accept_completion(cx));
            assert_eq!(editor.text(), "let");
        });
    }

    #[gpui::test]
    fn automatic_completion_debounces_and_cancels_on_navigation(cx: &mut gpui::TestAppContext) {
        use std::{cell::RefCell, rc::Rc};
        let requests = Rc::new(RefCell::new(Vec::new()));
        let observed = requests.clone();
        let editor = cx.new(|cx| {
            cx.subscribe(&cx.entity(), move |_, _, event, _| {
                if let EditorEvent::CompletionRequested(offset) = event {
                    observed.borrow_mut().push(*offset);
                }
            })
            .detach();
            CodeEditor::new(cx, "Dur".into(), Lang::Rust, "")
        });
        editor.update(cx, |editor, cx| {
            editor.move_to(3, cx);
            editor.schedule_completion("r", cx);
        });
        cx.run_until_parked();
        assert!(requests.borrow().is_empty());
        cx.background_executor
            .advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        assert_eq!(&*requests.borrow(), &[3]);
        editor.update(cx, |editor, cx| {
            editor.schedule_completion("r", cx);
            editor.move_to(0, cx);
        });
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        assert_eq!(&*requests.borrow(), &[3]);
    }
}
