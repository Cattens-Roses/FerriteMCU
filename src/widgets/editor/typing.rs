//! Small editing transactions shared by typing, indentation and bracket handling.
use super::*;

#[derive(Clone)]
pub(super) struct AutoPair {
    open: usize,
    close: usize,
    closing: char,
}

fn closing_for(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' | '\'' | '`' => Some(ch),
        _ => None,
    }
}

impl CodeEditor {
    fn code_typing(&self) -> bool {
        !self.single_line
            && !matches!(
                self.lang,
                Lang::PlainText | Lang::Markdown | Lang::Gitignore | Lang::Env
            )
    }

    /// Bound inference work: typing into a very large file must stay responsive.
    pub(super) fn indent_unit(&self) -> String {
        let mut width = None;
        for line in self.content.lines().take(200) {
            let leading = line.len() - line.trim_start_matches([' ', '\t']).len();
            if line[..leading].contains('\t') {
                return "\t".into();
            }
            if leading >= 2 && !line.trim().is_empty() {
                width = Some(width.map_or(leading, |old: usize| old.min(leading)));
            }
        }
        match width {
            Some(width) => " ".repeat(width.min(8)),
            None if self.lang == Lang::Go => "\t".into(),
            None if matches!(self.lang, Lang::Rust | Lang::Python | Lang::Java) => "    ".into(),
            None => "  ".into(),
        }
    }

    pub(super) fn edit_text(
        &mut self,
        range: Range<usize>,
        text: &str,
        selection: Range<usize>,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) {
        if self.read_only || (range.is_empty() && text.is_empty()) {
            return;
        }
        self.record_undo(kind);
        self.auto_pairs.retain_mut(|pair| {
            if range.contains(&pair.open) || range.contains(&pair.close) {
                return false;
            }
            for offset in [&mut pair.open, &mut pair.close] {
                if *offset >= range.end {
                    *offset = *offset - range.len() + text.len();
                }
            }
            true
        });
        self.content.replace_range(range, text);
        self.selected_range = selection;
        self.selection_reversed = false;
        self.marked_range = None;
        self.dirty = true;
        self.reveal_pending = true;
        self.recompute_folds(cx);
        self.restart_blink(cx);
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }

    /// Avoid inserting pairs inside quoted text, escapes and line comments. This scans
    /// only the current line; Rust apostrophes remain literal so lifetimes stay untouched.
    fn literal_context(&self, caret: usize) -> bool {
        let start = self.content[..caret].rfind('\n').map_or(0, |i| i + 1);
        let mut chars = self.content[start..caret].chars().peekable();
        let mut quote = None;
        let mut block_comment = false;
        while let Some(ch) = chars.next() {
            if block_comment {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    block_comment = false;
                }
            } else if let Some(delimiter) = quote {
                if ch == '\\' {
                    chars.next();
                } else if ch == delimiter {
                    quote = None;
                }
            } else if ch == '/' && chars.peek() == Some(&'/') {
                return true;
            } else if ch == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_comment = true;
            } else if ch == '#'
                && matches!(
                    self.lang,
                    Lang::Python | Lang::Bash | Lang::Yaml | Lang::Toml | Lang::R
                )
            {
                return true;
            } else if ch == '"'
                || (ch == '\''
                    && (self.lang != Lang::Rust || {
                        let mut remaining = chars.clone();
                        remaining.peek().is_none()
                            || remaining.peek() == Some(&'\\')
                            || remaining.nth(1) == Some('\'')
                    }))
                || (ch == '`' && matches!(self.lang, Lang::Js | Lang::Ts | Lang::Tsx))
            {
                quote = Some(ch);
            }
        }
        quote.is_some() || block_comment
    }

    pub(super) fn smart_type(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.code_typing() || self.read_only || text.chars().count() != 1 {
            return false;
        }
        let Some(ch) = text.chars().next() else {
            return false;
        };
        let range = self.selected_range.clone();
        let caret = self.cursor();
        let escaped = self.content[..caret]
            .chars()
            .rev()
            .take_while(|&c| c == '\\')
            .count()
            % 2
            == 1;
        if range.is_empty() && !(escaped && matches!(ch, '"' | '\'' | '`')) {
            if let Some(index) = self
                .auto_pairs
                .iter()
                .position(|p| p.close == caret && p.closing == ch)
            {
                self.auto_pairs.remove(index);
                self.move_to(caret + ch.len_utf8(), cx);
                return true;
            }
        }
        let Some(closing) = closing_for(ch) else {
            // A closing delimiter typed on an indented blank line returns to the parent level.
            if range.is_empty() && matches!(ch, ')' | ']' | '}') && !self.literal_context(caret) {
                let start = self.content[..caret].rfind('\n').map_or(0, |i| i + 1);
                let prefix = &self.content[start..caret];
                if !prefix.is_empty() && prefix.chars().all(|c| c == ' ' || c == '\t') {
                    let unit = self.indent_unit();
                    let remove = if prefix.ends_with('\t') {
                        1
                    } else {
                        prefix.len().min(unit.len())
                    };
                    self.edit_text(
                        caret - remove..caret,
                        text,
                        caret - remove + 1..caret - remove + 1,
                        EditKind::Other,
                        cx,
                    );
                    return true;
                }
            }
            return false;
        };
        if ch == '\'' && self.lang == Lang::Rust {
            return false;
        }
        if ch == '`' && !matches!(self.lang, Lang::Js | Lang::Ts | Lang::Tsx | Lang::Bash) {
            return false;
        }
        if !range.is_empty() {
            let reversed = self.selection_reversed;
            let wrapped = format!("{ch}{}{closing}", &self.content[range.clone()]);
            let inside = range.start + 1..range.end + 1;
            self.edit_text(range.clone(), &wrapped, inside, EditKind::Other, cx);
            self.selection_reversed = reversed;
            self.auto_pairs.push(AutoPair {
                open: range.start,
                close: range.end + 1,
                closing,
            });
            return true;
        }
        let next = self.content[caret..].chars().next();
        if escaped
            || self.literal_context(caret)
            || next.is_some_and(|c| {
                !c.is_whitespace() && !matches!(c, ')' | ']' | '}' | ',' | ';' | ':' | '.')
            })
            || (ch == '\''
                && self.content[..caret]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_'))
        {
            return false;
        }
        let pair = format!("{ch}{closing}");
        self.edit_text(
            caret..caret,
            &pair,
            caret + 1..caret + 1,
            EditKind::Other,
            cx,
        );
        self.auto_pairs.push(AutoPair {
            open: caret,
            close: caret + 1,
            closing,
        });
        true
    }

    pub(super) fn smart_enter(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.code_typing() || self.read_only {
            return false;
        }
        let range = self.selected_range.clone();
        let start = self.content[..range.start].rfind('\n').map_or(0, |i| i + 1);
        let before = &self.content[start..range.start];
        let indent: String = before
            .chars()
            .take_while(|&c| c == ' ' || c == '\t')
            .collect();
        let eol = if self
            .content
            .find('\n')
            .is_some_and(|i| i > 0 && self.content.as_bytes()[i - 1] == b'\r')
        {
            "\r\n"
        } else {
            "\n"
        };
        let previous = before.trim_end().chars().next_back();
        let opens = !self.literal_context(range.start)
            && (previous.is_some_and(|c| matches!(c, '{' | '[' | '('))
                || (self.lang == Lang::Python && previous == Some(':')));
        let unit = if opens {
            self.indent_unit()
        } else {
            String::new()
        };
        let following = self.content[range.end..].trim_start_matches([' ', '\t']);
        let between = opens
            && previous
                .and_then(closing_for)
                .is_some_and(|ch| following.starts_with(ch));
        let mut replacement = format!("{eol}{indent}{unit}");
        let caret = range.start + replacement.len();
        let mut end = range.end;
        if between {
            end += self.content[range.end..].len() - following.len();
            replacement.push_str(eol);
            replacement.push_str(&indent);
        }
        self.edit_text(
            range.start..end,
            &replacement,
            caret..caret,
            EditKind::Other,
            cx,
        );
        true
    }

    pub(super) fn smart_backspace(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.code_typing() || self.read_only || !self.selected_range.is_empty() {
            return false;
        }
        let caret = self.cursor();
        if let Some(pair) = self
            .auto_pairs
            .iter()
            .find(|p| p.open + 1 == caret && p.close == caret)
            .cloned()
        {
            self.edit_text(
                pair.open..pair.close + 1,
                "",
                pair.open..pair.open,
                EditKind::Other,
                cx,
            );
            return true;
        }
        let start = self.content[..caret].rfind('\n').map_or(0, |i| i + 1);
        let prefix = &self.content[start..caret];
        if !prefix.is_empty() && prefix.chars().all(|c| c == ' ' || c == '\t') {
            let unit = self.indent_unit();
            let remove = if prefix.ends_with('\t') {
                1
            } else {
                (prefix.len() - 1) % unit.len() + 1
            };
            self.edit_text(
                caret - remove..caret,
                "",
                caret - remove..caret - remove,
                EditKind::Other,
                cx,
            );
            return true;
        }
        false
    }

    pub(super) fn change_line_indent(&mut self, indent: bool, cx: &mut Context<Self>) {
        let unit = self.indent_unit();
        let selection = self.selected_range.clone();
        let reversed = self.selection_reversed;
        let start = self.content[..selection.start]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        // A selection ending at column zero does not include the following line.
        let last = if !selection.is_empty() && self.content[..selection.end].ends_with('\n') {
            selection.end - 1
        } else {
            selection.end
        };
        let end = self.content[last..]
            .find('\n')
            .map_or(self.content.len(), |i| last + i);
        let mut output = String::new();
        let mut at = start;
        let mut edits = Vec::new();
        for line in self.content[start..end].split_inclusive('\n') {
            let remove = if indent {
                0
            } else if line.starts_with('\t') {
                1
            } else {
                line.bytes()
                    .take(unit.len())
                    .take_while(|&b| b == b' ')
                    .count()
            };
            let inserted = if indent { unit.as_str() } else { "" };
            edits.push((at, remove, inserted.len()));
            output.push_str(inserted);
            output.push_str(&line[remove..]);
            at += line.len();
        }
        // The final empty line has no split_inclusive entry.
        if start == end {
            if !indent {
                return;
            }
            output.push_str(&unit);
            edits.push((start, 0, unit.len()));
        }
        if output == self.content[start..end] {
            return;
        }
        let map = |position: usize| {
            let mut mapped = position;
            for &(at, removed, inserted) in &edits {
                if position >= at {
                    mapped = mapped - (position - at).min(removed) + inserted;
                }
            }
            mapped
        };
        let mapped = map(selection.start)..map(selection.end);
        self.edit_text(start..end, &output, mapped, EditKind::Other, cx);
        self.selection_reversed = reversed;
    }

    pub(super) fn smart_line_start(&self) -> usize {
        let caret = self.cursor();
        let start = self.content[..caret].rfind('\n').map_or(0, |i| i + 1);
        let line = self.content[start..].split('\n').next().unwrap_or("");
        let first = start + line.len() - line.trim_start_matches([' ', '\t']).len();
        if caret == first {
            start
        } else {
            first
        }
    }

    pub(super) fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.smart_line_start(), cx);
    }
    pub(super) fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let caret = self.cursor();
        let end = self.content[caret..]
            .find(['\r', '\n'])
            .map_or(self.content.len(), |i| caret + i);
        self.select_to(end, cx);
    }
    pub(super) fn buffer_home(&mut self, _: &BufferHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    pub(super) fn buffer_end(&mut self, _: &BufferEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }
    pub(super) fn select_buffer_home(
        &mut self,
        _: &SelectBufferHome,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }
    pub(super) fn select_buffer_end(
        &mut self,
        _: &SelectBufferEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn enter_between_braces_indents_and_undo_restores_the_pair(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, "fn main() ".into(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            ed.move_to(ed.text().len(), cx);
            ed.replace_text_in_range(None, "{", w, cx);
            assert_eq!(ed.text(), "fn main() {}");
            ed.enter(&Enter, w, cx);
            assert_eq!(ed.text(), "fn main() {\n    \n}");
            assert_eq!(ed.cursor(), "fn main() {\n    ".len());
            assert!(ed.reveal_pending);
            ed.undo(&Undo, w, cx);
            assert_eq!(ed.text(), "fn main() {}");
            ed.backspace(&Backspace, w, cx);
            assert_eq!(ed.text(), "fn main() ");
        })
        .unwrap();
    }

    #[gpui::test]
    fn enter_respects_spaces_tabs_python_and_windows_line_endings(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            for (lang, before, expected) in [
                (
                    Lang::Rust,
                    "fn f() {\n  let x = 1;",
                    "fn f() {\n  let x = 1;\n  ",
                ),
                (Lang::Go, "func f() {\n\tx := 1", "func f() {\n\tx := 1\n\t"),
                (Lang::Python, "if ready:", "if ready:\n    "),
                (
                    Lang::Rust,
                    "fn f() {\r\n    let x = 1;",
                    "fn f() {\r\n    let x = 1;\r\n    ",
                ),
                (Lang::Rust, "// example {", "// example {\n"),
            ] {
                ed.set_content(before.into(), lang, cx);
                ed.move_to(before.len(), cx);
                ed.enter(&Enter, w, cx);
                assert_eq!(ed.text(), expected, "{lang:?}");
            }
        })
        .unwrap();
    }

    #[gpui::test]
    fn nested_pairs_skip_only_generated_closers(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            for typed in ["(", "[", "x", "]", ")"] {
                ed.replace_text_in_range(None, typed, w, cx);
            }
            assert_eq!(ed.text(), "([x])");
            assert_eq!(ed.cursor(), 5);
            ed.set_content(")".into(), Lang::Rust, cx);
            ed.replace_text_in_range(None, ")", w, cx);
            assert_eq!(ed.text(), "))", "existing source is never silently skipped");
        })
        .unwrap();
    }

    #[gpui::test]
    fn escaped_quotes_and_rust_lifetimes_are_literal(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            for typed in ["\"", "\\", "\"", "x", "\""] {
                ed.replace_text_in_range(None, typed, w, cx);
            }
            assert_eq!(ed.text(), "\"\\\"x\"");
            assert_eq!(ed.cursor(), ed.text().len());
            for literal in ["&'a str", "'static", "'('", "'{'", "'\\n'"] {
                ed.set_content(String::new(), Lang::Rust, cx);
                for ch in literal.chars() {
                    ed.replace_text_in_range(None, &ch.to_string(), w, cx);
                }
                assert_eq!(ed.text(), literal);
            }
        })
        .unwrap();
    }

    #[gpui::test]
    fn pairs_do_not_expand_inside_comments_or_strings(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            for (lang, prefix) in [
                (Lang::Rust, "// "),
                (Lang::Rust, "let s = \""),
                (Lang::Python, "# "),
                (Lang::Rust, "/* "),
            ] {
                ed.set_content(prefix.into(), lang, cx);
                ed.move_to(prefix.len(), cx);
                ed.replace_text_in_range(None, "(", w, cx);
                assert_eq!(ed.text(), format!("{prefix}("));
            }
        })
        .unwrap();
    }

    #[gpui::test]
    fn surround_preserves_unicode_selection_direction_and_undo(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, "żółć".into(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            let len = ed.text().len();
            ed.select_range(0..len, cx);
            ed.selection_reversed = true;
            ed.replace_text_in_range(None, "\"", w, cx);
            assert_eq!(ed.text(), "\"żółć\"");
            assert_eq!(&ed.content[ed.selection()], "żółć");
            assert!(ed.selection_reversed);
            ed.undo(&Undo, w, cx);
            assert_eq!(ed.text(), "żółć");
            assert!(ed.selection_reversed);
            ed.redo(&Redo, w, cx);
            assert_eq!(ed.text(), "\"żółć\"");
        })
        .unwrap();
    }

    #[gpui::test]
    fn tab_and_shift_tab_preserve_selection_and_exclude_next_line(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, "a\nb\nc".into(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            ed.select_range(0..4, cx);
            ed.selection_reversed = true;
            let generation = ed.compute_gen;
            ed.indent(&Indent, w, cx);
            assert_eq!(ed.text(), "    a\n    b\nc");
            assert_eq!(ed.selection(), 4..12);
            assert!(ed.selection_reversed);
            assert!(
                ed.compute_gen > generation,
                "indentation refreshes syntax and LSP generations"
            );
            ed.outdent(&Outdent, w, cx);
            assert_eq!(ed.text(), "a\nb\nc");
            assert_eq!(ed.selection(), 0..4);
            ed.undo(&Undo, w, cx);
            assert_eq!(ed.text(), "    a\n    b\nc");
        })
        .unwrap();
    }

    #[gpui::test]
    fn tab_and_backspace_use_indentation_stops(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, "      ".into(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            ed.move_to(6, cx);
            ed.backspace(&Backspace, w, cx);
            assert_eq!(ed.text(), "    ");
            ed.backspace(&Backspace, w, cx);
            assert_eq!(ed.text(), "");
            ed.replace_text_in_range(None, "x", w, cx);
            ed.indent(&Indent, w, cx);
            assert_eq!(ed.text(), "x   ");
            ed.set_content("\tvalue".into(), Lang::Go, cx);
            ed.move_to(1, cx);
            ed.backspace(&Backspace, w, cx);
            assert_eq!(ed.text(), "value");
        })
        .unwrap();
    }

    #[gpui::test]
    fn paste_and_ime_do_not_generate_pairs(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("(".into()));
            ed.paste(&Paste, w, cx);
            assert_eq!(ed.text(), "(");
            ed.set_content(String::new(), Lang::Rust, cx);
            ed.replace_and_mark_text_in_range(None, "\"", None, w, cx);
            ed.replace_text_in_range(None, "\"", w, cx);
            assert_eq!(ed.text(), "\"");
            assert!(ed.auto_pairs.is_empty());
        })
        .unwrap();
    }

    #[gpui::test]
    fn plain_text_single_line_and_read_only_keep_their_input_behavior(
        cx: &mut gpui::TestAppContext,
    ) {
        let win = cx.add_window(|_, cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        win.update(cx, |ed, w, cx| {
            ed.replace_text_in_range(None, "(", w, cx);
            assert_eq!(ed.text(), "(");
            ed.set_content(String::new(), Lang::Rust, cx);
            ed.single_line = true;
            ed.replace_text_in_range(None, "\"", w, cx);
            assert_eq!(ed.text(), "\"");
            ed.single_line = false;
            ed.read_only = true;
            ed.enter(&Enter, w, cx);
            ed.indent(&Indent, w, cx);
            ed.backspace(&Backspace, w, cx);
            assert_eq!(ed.text(), "\"");
        })
        .unwrap();
    }

    #[gpui::test]
    fn home_and_selection_keys_work_through_real_bindings(cx: &mut gpui::TestAppContext) {
        cx.update(bind_keys);
        let win =
            cx.add_window(|_, cx| CodeEditor::new(cx, "    αβ\nsecond".into(), Lang::Rust, ""));
        win.update(cx, |ed, w, cx| {
            ed.move_to(8, cx);
            w.focus(&ed.focus_handle);
        })
        .unwrap();
        cx.run_until_parked();
        let press = |cx: &mut gpui::TestAppContext, key: &str| {
            cx.update_window(win.into(), |_, w, cx| {
                w.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), cx);
            })
            .unwrap();
        };
        press(cx, "home");
        win.update(cx, |ed, _, _| assert_eq!(ed.cursor(), 4))
            .unwrap();
        press(cx, "home");
        win.update(cx, |ed, _, _| assert_eq!(ed.cursor(), 0))
            .unwrap();
        press(cx, "shift-end");
        win.update(cx, |ed, _, _| assert_eq!(ed.selection(), 0..8))
            .unwrap();
        press(cx, "ctrl-shift-end");
        win.update(cx, |ed, _, _| {
            assert_eq!(ed.selection(), 0..ed.text().len());
        })
        .unwrap();
    }
}
