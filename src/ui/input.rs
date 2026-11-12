//! A text field. GPUI has no ready one; this follows the `input` example of GPUI and adds
//! several lines with soft wrap, word movement, undo and a horizontal scroll for one line.

use super::theme::theme;
use gpui::prelude::*;
use gpui::{
    actions, div, fill, point, px, relative, size, App, Bounds, ClipboardItem, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine,
};
use std::ops::Range;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        Paste,
        Cut,
        Copy,
        Undo,
        Enter,
        ShiftEnter,
        Submit,
        FocusNext,
        FocusPrev,
    ]
);

const CONTEXT: &str = "TextInput";

pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("secondary-left", WordLeft, c),
        KeyBinding::new("secondary-right", WordRight, c),
        KeyBinding::new("secondary-shift-left", SelectWordLeft, c),
        KeyBinding::new("secondary-shift-right", SelectWordRight, c),
        KeyBinding::new("secondary-backspace", DeleteWordLeft, c),
        KeyBinding::new("secondary-a", SelectAll, c),
        KeyBinding::new("secondary-v", Paste, c),
        KeyBinding::new("secondary-c", Copy, c),
        KeyBinding::new("secondary-x", Cut, c),
        KeyBinding::new("secondary-z", Undo, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", ShiftEnter, c),
        KeyBinding::new("secondary-enter", Submit, c),
        KeyBinding::new("tab", FocusNext, c),
        KeyBinding::new("shift-tab", FocusPrev, c),
    ]);
}

pub enum InputEvent {
    /// The text changed.
    Change,
    /// Enter in a one-line field.
    Enter,
    /// Shift+Enter in a one-line field.
    ShiftEnter,
    /// Ctrl+Enter (Cmd+Enter on macOS) in any field.
    Submit,
}

pub struct TextInput {
    focus: FocusHandle,
    content: String,
    placeholder: SharedString,
    multiline: bool,
    /// Lines the field shows at least (several-line fields).
    min_rows: usize,
    mono: bool,
    sel: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    /// Layout of the last frame: one entry per line of the text, with its first byte.
    lines: Vec<(usize, WrappedLine)>,
    bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    wrap_width: Option<Pixels>,
    scroll_x: Pixels,
    selecting: bool,
    undo: Vec<(String, Range<usize>)>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        TextInput {
            // Tab moves between the text fields of the window.
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            content: String::new(),
            placeholder: SharedString::default(),
            multiline: false,
            min_rows: 1,
            mono: false,
            sel: 0..0,
            reversed: false,
            marked: None,
            lines: Vec::new(),
            bounds: None,
            line_height: px(20.),
            wrap_width: None,
            scroll_x: px(0.),
            selecting: false,
            undo: Vec::new(),
        }
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn multiline(mut self, min_rows: usize) -> Self {
        self.multiline = true;
        self.min_rows = min_rows.max(1);
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.content = text.into();
        self.sel = self.content.len()..self.content.len();
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.sel = self.content.len()..self.content.len();
        self.marked = None;
        self.scroll_x = px(0.);
        self.undo.clear();
        cx.notify();
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.sel = 0..self.content.len();
        self.reversed = false;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.sel.start
        } else {
            self.sel.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.sel = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.sel.start = offset;
        } else {
            self.sel.end = offset;
        }
        if self.sel.end < self.sel.start {
            self.reversed = !self.reversed;
            self.sel = self.sel.end..self.sel.start;
        }
        cx.notify();
    }

    fn prev_char(&self, offset: usize) -> usize {
        self.content[..offset].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_char(&self, offset: usize) -> usize {
        self.content[offset..].chars().next().map(|c| offset + c.len_utf8()).unwrap_or(self.content.len())
    }

    fn prev_word(&self, offset: usize) -> usize {
        let mut at = offset;
        let mut seen_word = false;
        for (i, c) in self.content[..offset].char_indices().rev() {
            if c.is_alphanumeric() || c == '_' {
                seen_word = true;
            } else if seen_word {
                break;
            }
            at = i;
        }
        at
    }

    fn next_word(&self, offset: usize) -> usize {
        let mut seen_word = false;
        for (i, c) in self.content[offset..].char_indices() {
            if c.is_alphanumeric() || c == '_' {
                seen_word = true;
            } else if seen_word {
                return offset + i;
            }
        }
        self.content.len()
    }

    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..].find('\n').map(|i| offset + i).unwrap_or(self.content.len())
    }

    /// Position of a byte offset, relative to the top left corner of the text.
    fn position_of(&self, offset: usize) -> Point<Pixels> {
        let mut y = px(0.);
        for (start, line) in &self.lines {
            let end = start + line.len();
            if offset <= end {
                let p = line.position_for_index(offset - start, self.line_height).unwrap_or_default();
                return point(p.x, y + p.y);
            }
            y += line.size(self.line_height).height;
        }
        point(px(0.), y)
    }

    /// The byte offset closest to a position relative to the top left corner of the text.
    fn offset_at(&self, pos: Point<Pixels>) -> usize {
        let mut y = px(0.);
        for (start, line) in &self.lines {
            let h = line.size(self.line_height).height;
            if pos.y < y + h {
                let local = point(pos.x, (pos.y - y).max(px(0.)));
                let i = match line.closest_index_for_position(local, self.line_height) {
                    Ok(i) | Err(i) => i,
                };
                return start + i.min(line.len());
            }
            y += h;
        }
        self.content.len()
    }

    fn vertical(&self, down: bool) -> usize {
        let p = self.position_of(self.cursor());
        let y = if down { p.y + self.line_height * 1.5 } else { p.y - self.line_height * 0.5 };
        if y < px(0.) {
            return 0;
        }
        self.offset_at(point(p.x, y))
    }

    fn mouse_offset(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.bounds else { return 0 };
        if self.content.is_empty() {
            return 0;
        }
        self.offset_at(point(position.x - bounds.left() + self.scroll_x, position.y - bounds.top()))
    }

    fn edit(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if self.undo.len() >= 200 {
            self.undo.remove(0);
        }
        self.undo.push((self.content.clone(), self.sel.clone()));
        self.content.replace_range(range.clone(), text);
        let at = range.start + text.len();
        self.sel = at..at;
        self.reversed = false;
        self.marked = None;
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Replaces the selection with `text`, as typing does.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = if self.multiline { text.replace("\r\n", "\n") } else { text.replace(['\r', '\n'], " ") };
        self.edit(self.sel.clone(), &text, cx);
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    fn utf16_to_offset(&self, offset: usize) -> usize {
        let mut units = 0;
        for (i, c) in self.content.char_indices() {
            if units >= offset {
                return i;
            }
            units += c.len_utf16();
        }
        self.content.len()
    }

    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.utf16_to_offset(r.start)..self.utf16_to_offset(r.end)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let at = self.mouse_offset(event.position);
        match event.click_count {
            2 => {
                self.sel = self.prev_word(self.next_char(at).min(self.content.len()))..self.next_word(at);
                self.reversed = false;
                cx.notify();
            }
            n if n >= 3 => {
                self.sel = self.line_start(at)..self.line_end(at);
                self.reversed = false;
                cx.notify();
            }
            _ => {
                self.selecting = true;
                if event.modifiers.shift {
                    self.select_to(at, cx);
                } else {
                    self.move_to(at, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.mouse_offset(event.position), cx);
        }
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.sel), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked.clone()).unwrap_or(self.sel.clone());
        let text = if self.multiline { new_text.replace("\r\n", "\n").replace('\r', "\n") } else { new_text.replace(['\r', '\n'], "") };
        if text.is_empty() && !new_text.is_empty() {
            return;
        }
        self.edit(range, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked.clone()).unwrap_or(self.sel.clone());
        self.content.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.sel = new_selected
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (a, b) = (self.position_of(range.start), self.position_of(range.end));
        Some(Bounds::from_corners(
            point(bounds.left() + a.x - self.scroll_x, bounds.top() + a.y),
            point(bounds.left() + b.x - self.scroll_x, bounds.top() + b.y + self.line_height),
        ))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.offset_to_utf16(self.mouse_offset(p)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    lines: Vec<(usize, WrappedLine)>,
    selection: Vec<Bounds<Pixels>>,
    cursor: Option<Bounds<Pixels>>,
    text_color: Hsla,
    accent: Hsla,
    selection_color: Hsla,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl TextElement {
    fn shape(&self, window: &mut Window, cx: &mut App) -> (Vec<(usize, WrappedLine)>, Hsla) {
        let input = self.input.read(cx);
        let t = theme(cx);
        let style = window.text_style();
        let (text, color): (SharedString, Hsla) =
            if input.content.is_empty() { (input.placeholder.clone(), t.dim) } else { (input.content.clone().into(), style.color) };
        let base = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
        let runs = match input.marked.as_ref().filter(|_| !input.content.is_empty()) {
            Some(m) => vec![
                TextRun { len: m.start, ..base.clone() },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }),
                    ..base.clone()
                },
                TextRun { len: text.len() - m.end, ..base },
            ],
            None => vec![base],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let wrap = if input.multiline { input.wrap_width } else { None };
        let shaped = window.text_system().shape_text(text.clone(), font_size, &runs, wrap, None).unwrap_or_default();
        let mut start = 0;
        let mut lines = Vec::with_capacity(shaped.len());
        for line in shaped {
            let len = line.len();
            lines.push((start, line));
            start += len + 1;
        }
        (lines, color)
    }
}

impl Element for TextElement {
    type RequestLayoutState = Vec<(usize, WrappedLine)>;
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (lines, _) = self.shape(window, cx);
        let line_height = window.line_height();
        let input = self.input.read(cx);
        let mut height = px(0.);
        for (_, line) in &lines {
            height += line.size(line_height).height;
        }
        let rows = if input.multiline { input.min_rows } else { 1 };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = height.max(line_height * rows as f32).into();
        (window.request_layout(style, [], cx), lines)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        lines: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let line_height = window.line_height();
        let t = theme(cx);
        let text_color = window.text_style().color;
        let lines = std::mem::take(lines);
        let focused = self.input.read(cx).focus.is_focused(window);
        // Remember the layout: mouse and keyboard handlers use it.
        let rewrap = self.input.update(cx, |input, _| {
            input.lines = lines.clone();
            input.bounds = Some(bounds);
            input.line_height = line_height;
            let changed = input.multiline && input.wrap_width != Some(bounds.size.width);
            input.wrap_width = Some(bounds.size.width);
            if !input.multiline {
                // Keep the cursor in view.
                let x = input.position_of(input.cursor()).x;
                let width = bounds.size.width - px(2.);
                if x - input.scroll_x > width {
                    input.scroll_x = x - width;
                } else if x < input.scroll_x {
                    input.scroll_x = x;
                }
                if input.content.is_empty() {
                    input.scroll_x = px(0.);
                }
            }
            changed
        });
        if rewrap {
            // The width changed: wrap again in the next frame.
            window.request_animation_frame();
        }
        let input = self.input.read(cx);
        let origin = point(bounds.left() - input.scroll_x, bounds.top());
        let mut selection = Vec::new();
        let mut cursor = None;
        if input.sel.is_empty() || input.content.is_empty() {
            if focused {
                let p = if input.content.is_empty() { point(px(0.), px(0.)) } else { input.position_of(input.cursor()) };
                cursor = Some(Bounds::new(point(origin.x + p.x, origin.y + p.y), size(px(1.5), line_height)));
            }
        } else {
            // One rectangle per visual row of the selection.
            let (a, b) = (input.position_of(input.sel.start), input.position_of(input.sel.end));
            if a.y == b.y {
                selection
                    .push(Bounds::from_corners(point(origin.x + a.x, origin.y + a.y), point(origin.x + b.x, origin.y + a.y + line_height)));
            } else {
                selection
                    .push(Bounds::from_corners(point(origin.x + a.x, origin.y + a.y), point(bounds.right(), origin.y + a.y + line_height)));
                if b.y - a.y > line_height {
                    selection.push(Bounds::from_corners(
                        point(bounds.left(), origin.y + a.y + line_height),
                        point(bounds.right(), origin.y + b.y),
                    ));
                }
                selection
                    .push(Bounds::from_corners(point(bounds.left(), origin.y + b.y), point(origin.x + b.x, origin.y + b.y + line_height)));
            }
        }
        Prepaint { lines: input.lines.clone(), selection, cursor, text_color, accent: t.accent, selection_color: t.accent_soft }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, scroll_x) = {
            let input = self.input.read(cx);
            (input.focus.clone(), input.scroll_x)
        };
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let line_height = window.line_height();
        let _ = state.text_color;
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for r in state.selection.drain(..) {
                window.paint_quad(fill(r, state.selection_color));
            }
            let mut y = bounds.top();
            for (_, line) in &state.lines {
                let _ = line.paint(point(bounds.left() - scroll_x, y), line_height, TextAlign::Left, None, window, cx);
                y += line.size(line_height).height;
            }
            if let Some(c) = state.cursor.take() {
                window.paint_quad(fill(c, state.accent));
            }
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let multiline = self.multiline;
        let mut el = div()
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .line_height(px(20.))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                if this.sel.is_empty() {
                    let at = this.prev_char(this.cursor());
                    this.select_to(at, cx);
                }
                if !this.sel.is_empty() {
                    this.edit(this.sel.clone(), "", cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                if this.sel.is_empty() {
                    let at = this.next_char(this.cursor());
                    this.select_to(at, cx);
                }
                if !this.sel.is_empty() {
                    this.edit(this.sel.clone(), "", cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                if this.sel.is_empty() {
                    let at = this.prev_word(this.cursor());
                    this.select_to(at, cx);
                }
                if !this.sel.is_empty() {
                    this.edit(this.sel.clone(), "", cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Left, _, cx| {
                let at = if this.sel.is_empty() { this.prev_char(this.cursor()) } else { this.sel.start };
                this.move_to(at, cx);
            }))
            .on_action(cx.listener(|this, _: &Right, _, cx| {
                let at = if this.sel.is_empty() { this.next_char(this.cursor()) } else { this.sel.end };
                this.move_to(at, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.select_to(this.prev_char(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.select_to(this.next_char(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &WordLeft, _, cx| this.move_to(this.prev_word(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &WordRight, _, cx| this.move_to(this.next_word(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| this.select_to(this.prev_word(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| this.select_to(this.next_word(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all_text(cx)))
            .on_action(cx.listener(|this, _: &Home, _, cx| this.move_to(this.line_start(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &End, _, cx| this.move_to(this.line_end(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.select_to(this.line_start(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| this.select_to(this.line_end(this.cursor()), cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    this.insert(&text, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                if !this.sel.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(this.content[this.sel.clone()].to_string()));
                }
            }))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                if !this.sel.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(this.content[this.sel.clone()].to_string()));
                    this.edit(this.sel.clone(), "", cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                if let Some((text, sel)) = this.undo.pop() {
                    this.content = text;
                    this.sel = sel;
                    this.marked = None;
                    cx.emit(InputEvent::Change);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|_, _: &Submit, _, cx| cx.emit(InputEvent::Submit)))
            .on_action(|_: &FocusNext, window, _| window.focus_next())
            .on_action(|_: &FocusPrev, window, _| window.focus_prev())
            .on_action(cx.listener(move |this, _: &Enter, _, cx| {
                if this.multiline {
                    this.edit(this.sel.clone(), "\n", cx);
                } else {
                    cx.emit(InputEvent::Enter);
                }
            }))
            .on_action(cx.listener(move |this, _: &ShiftEnter, _, cx| {
                if this.multiline {
                    this.edit(
                        this.sel.clone(),
                        "
",
                        cx,
                    );
                } else {
                    cx.emit(InputEvent::ShiftEnter);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move));
        if multiline {
            el = el
                .on_action(cx.listener(|this, _: &Up, _, cx| this.move_to(this.vertical(false), cx)))
                .on_action(cx.listener(|this, _: &Down, _, cx| this.move_to(this.vertical(true), cx)))
                .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.select_to(this.vertical(false), cx)))
                .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.select_to(this.vertical(true), cx)));
        }
        if self.mono {
            el = el.font_family(super::MONO_FONT);
        }
        el.child(TextElement { input: cx.entity() })
    }
}

/// The usual frame around a text field: border, padding, accent border when focused.
pub fn field(input: &Entity<TextInput>, window: &Window, cx: &App) -> gpui::Div {
    let t = theme(cx);
    let focused = input.read(cx).is_focused(window);
    div()
        .w_full()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .border_1()
        .border_color(if focused { t.accent } else { t.border_strong })
        .bg(t.panel)
        .text_color(t.text)
        .text_size(px(13.))
        .overflow_hidden()
        .child(input.clone())
}
