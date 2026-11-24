//! The terminal panel under the graph: the log of the git commands that ran for this
//! repository, and a line where the user types a git command. The log is written by the
//! core whether or not the panel is open; opening the panel only shows it.

use super::input::{InputEvent, TextInput};
use super::overlay::{self, MenuItem};
use super::repo_view::{RepoView, Resize};
use super::theme::{theme, Theme};
use super::widgets::{btn, icon, icon_btn, scrollbar, splitter, tip, ScrollbarState, LEFT};
use super::{call, pref, set_pref, MONO_FONT};
use gpui::prelude::*;
use gpui::{
    div, font, px, uniform_list, AnyElement, App, Entity, FontWeight, MouseButton, MouseDownEvent, Pixels, Point, ScrollStrategy,
    SharedString, UniformListScrollHandle, Window,
};
use graftline::cmdlog::{self, Entry, Source};
use std::ops::Range;
use std::path::Path;

const LINE: f32 = 19.;
const FONT: f32 = 12.5;
/// Width of the column with the `$` sign.
const GUTTER: f32 = 18.;
/// Characters kept free at the end of a command line for its state (time, exit code).
const STATE_COLS: usize = 26;
const MIN_H: f32 = 110.;
const KEY_OPEN: &str = "gl.termOpen";
const KEY_H: &str = "gl.termH";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Command,
    Output,
    /// A remark of the app about the entry, not text from git.
    Remark,
    Gap,
}

struct Row {
    entry: usize,
    kind: Kind,
    /// The first row of a command that takes several rows.
    first: bool,
    text: SharedString,
}

pub struct Terminal {
    pub height: f32,
    pub input: Entity<TextInput>,
    scroll: UniformListScrollHandle,
    bar: ScrollbarState,
    entries: Vec<Entry>,
    rows: Vec<Row>,
    /// The log changed since `rows` was built.
    dirty: bool,
    /// Characters per row that `rows` was built for.
    cols: usize,
    /// Show the end of the log after the next build.
    follow: bool,
    /// Lines the user ran, oldest first, and the place of the Up/Down keys in them.
    typed: Vec<String>,
    at: Option<usize>,
    draft: String,
}

impl Terminal {
    pub fn new(cx: &mut Context<RepoView>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx).mono().placeholder("Type a git command and press Enter, for example: status"));
        cx.subscribe(&input, |this: &mut RepoView, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Enter | InputEvent::Submit) {
                this.term_submit(cx);
            }
        })
        .detach();
        Terminal {
            height: pref(cx, KEY_H).unwrap_or(220.),
            input,
            scroll: UniformListScrollHandle::new(),
            bar: ScrollbarState::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            dirty: true,
            cols: 0,
            follow: true,
            typed: Vec::new(),
            at: None,
            draft: String::new(),
        }
    }
}

/// Cuts text into rows of at most `cols` characters.
fn wrap(text: &str, cols: usize, mut push: impl FnMut(String)) {
    for line in text.split('\n') {
        let line = line.replace('\t', "    ");
        if line.chars().count() <= cols {
            push(line);
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for part in chars.chunks(cols) {
            push(part.iter().collect());
        }
    }
}

fn build_rows(entries: &[Entry], cols: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            rows.push(Row { entry: i, kind: Kind::Gap, first: false, text: SharedString::default() });
        }
        let mut first = true;
        wrap(&e.command, cols.saturating_sub(STATE_COLS).max(20), |text| {
            rows.push(Row { entry: i, kind: Kind::Command, first, text: text.into() });
            first = false;
        });
        if !e.output.is_empty() {
            wrap(&e.output, cols, |text| rows.push(Row { entry: i, kind: Kind::Output, first: false, text: text.into() }));
        }
        if e.truncated {
            rows.push(Row {
                entry: i,
                kind: Kind::Remark,
                first: false,
                text: "The rest of the output is not shown (256 KB limit).".into(),
            });
        }
    }
    rows
}

fn seconds(ms: u64) -> String {
    if ms < 10_000 {
        format!("{:.2} s", ms as f64 / 1000.)
    } else {
        format!("{:.0} s", ms as f64 / 1000.)
    }
}

pub fn is_open(cx: &App) -> bool {
    pref(cx, KEY_OPEN).unwrap_or(false)
}

impl RepoView {
    /// The command log of this repository changed.
    pub fn on_command_log(&mut self, cx: &mut Context<Self>) {
        self.term.dirty = true;
        // A closed panel is not opened and not drawn: the log itself is in the core.
        if is_open(cx) {
            cx.notify();
        }
    }

    pub fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = !is_open(cx);
        set_pref(cx, KEY_OPEN, &open);
        if open {
            self.term.follow = true;
            self.term.dirty = true;
            self.term.input.read(cx).focus(window);
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    fn term_submit(&mut self, cx: &mut Context<Self>) {
        let line = self.term.input.read(cx).text().trim().to_string();
        if line.is_empty() {
            return;
        }
        self.term.input.update(cx, |i, cx| i.set_text("", cx));
        if self.term.typed.last() != Some(&line) {
            self.term.typed.push(line.clone());
        }
        self.term.at = None;
        self.term.draft.clear();
        self.term.follow = true;
        if line == "clear" || line == "cls" {
            cmdlog::clear(Path::new(&self.path));
            return;
        }
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.run_command(&id, &line).await });
        cx.spawn(async move |_, cx| {
            if let Err(e) = task.await {
                let _ = cx.update(|cx| overlay::error(cx, format!("{e:#}")));
            }
        })
        .detach();
    }

    /// Up and Down in the command line walk through the lines that ran before.
    fn term_recall(&mut self, back: bool, cx: &mut Context<Self>) {
        let term = &mut self.term;
        if term.typed.is_empty() {
            return;
        }
        let last = term.typed.len() - 1;
        let next = match (term.at, back) {
            (None, true) => {
                term.draft = term.input.read(cx).text().to_string();
                Some(last)
            }
            (None, false) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i >= last => None,
            (Some(i), false) => Some(i + 1),
        };
        term.at = next;
        let text = match next {
            Some(i) => term.typed[i].clone(),
            None => std::mem::take(&mut term.draft),
        };
        term.input.update(cx, |i, cx| i.set_text(text, cx));
    }

    /// The user's command that runs now, if any.
    fn term_running(&self) -> Option<u64> {
        self.term.entries.iter().rev().find(|e| e.running && e.source == Source::User).map(|e| e.id)
    }

    fn term_menu(&mut self, entry: usize, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(e) = self.term.entries.get(entry).cloned() else { return };
        let w = cx.entity().downgrade();
        let (command, output, id) = (e.command.clone(), e.output.clone(), e.id);
        let both = if output.is_empty() { format!("$ {command}") } else { format!("$ {command}\n{output}") };
        let mut items = vec![
            MenuItem::new("copy", "Copy the command", move |_, cx| overlay::copy_text(cx, command.clone(), "Command")),
            MenuItem::new("copy", "Copy the output", move |_, cx| overlay::copy_text(cx, output.clone(), "Output"))
                .disabled(e.output.is_empty()),
            MenuItem::new("copy", "Copy the command and the output", move |_, cx| overlay::copy_text(cx, both.clone(), "Text")),
        ];
        // A command of the built-in library can be a shortened one: it is not offered again.
        if e.source != Source::BuiltIn {
            let (w, command) = (w.clone(), e.command.clone());
            items.push(MenuItem::sep());
            items.push(MenuItem::new("edit", "Put the command in the command line", move |window, cx| {
                let _ = w.update(cx, |this, cx| {
                    this.term.input.update(cx, |i, cx| i.set_text(command.clone(), cx));
                    this.term.input.read(cx).focus(window);
                });
            }));
        }
        if e.running && e.source == Source::User {
            items.push(MenuItem::sep());
            items.push(
                MenuItem::new("x", "Stop the command", move |_, _| {
                    cmdlog::cancel(id);
                })
                .danger(),
            );
        }
        overlay::open_menu(cx, pos, items);
    }

    /// Reads the log again when it changed or when the panel got another width.
    fn term_sync(&mut self, width: f32, window: &mut Window) {
        let ts = window.text_system();
        let advance: f32 = ts.em_advance(ts.resolve_font(&font(MONO_FONT)), px(FONT)).map(f32::from).unwrap_or(FONT * 0.55);
        // Padding on both sides, the `$` column and the scrollbar.
        let cols = (((width - 20. - GUTTER - 12.) / advance.max(1.)).floor() as usize).max(30);
        let term = &mut self.term;
        if !term.dirty && cols == term.cols {
            return;
        }
        // Stay at the end when the user is there; keep the place when they scrolled up.
        let handle = term.scroll.0.borrow().base_handle.clone();
        let from_end: f32 = (handle.max_offset().height + handle.offset().y).into();
        let at_end = from_end < LINE * 2.;
        if term.dirty {
            term.entries = cmdlog::list(Path::new(&self.path));
        }
        term.rows = build_rows(&term.entries, cols);
        term.cols = cols;
        term.dirty = false;
        if (term.follow || at_end) && !term.rows.is_empty() {
            term.scroll.scroll_to_item(term.rows.len() - 1, ScrollStrategy::Bottom);
        }
        term.follow = false;
    }

    fn render_term_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = theme(cx);
        let term = &self.term;
        range
            .filter_map(|i| {
                let row = term.rows.get(i)?;
                let e = term.entries.get(row.entry)?;
                let entry = row.entry;
                let mut el = div()
                    .id(("term-row", i))
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(LINE))
                    .px(px(10.))
                    .font_family(MONO_FONT)
                    .text_size(px(FONT))
                    .whitespace_nowrap()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.term_menu(entry, ev.position, cx);
                        }),
                    );
                el = match row.kind {
                    Kind::Gap => el,
                    Kind::Output => el.pl(px(10. + GUTTER)).text_color(t.muted).child(row.text.clone()),
                    Kind::Remark => el.pl(px(10. + GUTTER)).text_color(t.dim).child(row.text.clone()),
                    Kind::Command => {
                        let sign = match e.source {
                            Source::User => t.accent,
                            Source::App | Source::BuiltIn => t.dim,
                        };
                        let mut el =
                            el.child(div().flex_none().w(px(GUTTER)).text_color(sign).child(if row.first { "$" } else { "" })).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_color(t.text)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(row.text.clone()),
                            );
                        if row.first {
                            el = el.child(self.render_term_state(e, i, &t));
                        }
                        el
                    }
                };
                Some(el.into_any_element())
            })
            .collect()
    }

    /// The right end of a command row: where the command came from and how it ended.
    fn render_term_state(&self, e: &Entry, row: usize, t: &Theme) -> impl IntoElement {
        let mut el = div().flex().flex_none().items_center().gap_2().pl_2().pr(px(8.)).font_family(super::UI_FONT).text_size(px(11.5));
        if e.source == Source::BuiltIn {
            el = el.child(
                div()
                    .id(("term-builtin", row))
                    .px(px(5.))
                    .rounded(px(3.))
                    .bg(t.hover)
                    .text_color(t.muted)
                    .tooltip(tip("The built-in Git library did this. The line shows the git command that does the same."))
                    .child("built-in"),
            );
        }
        if e.running {
            return el.child(div().text_color(t.accent).child("running…"));
        }
        if !e.ok {
            let text = match (e.cancelled, e.code) {
                (true, _) => "stopped".to_string(),
                (false, Some(code)) => format!("exit code {code}"),
                (false, None) => "not run".to_string(),
            };
            el = el.child(div().text_color(t.danger).child(text));
        }
        // A built-in change has no run time worth showing.
        match e.ms.filter(|_| e.source != Source::BuiltIn) {
            Some(ms) => el.child(div().text_color(t.dim).child(seconds(ms))),
            None => el,
        }
    }

    /// The panel with its top border; `None` while it is hidden.
    pub fn render_terminal(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !is_open(cx) {
            return None;
        }
        let viewport = window.viewport_size();
        let sidebar: f32 = pref(cx, "gl.sidebarW").unwrap_or(248.);
        let width = (f32::from(viewport.width) - sidebar - 3.).max(240.);
        if !matches!(self.resize, Some(Resize::Term { .. })) {
            self.term.height = pref(cx, KEY_H).unwrap_or(self.term.height);
        }
        let height = self.term.height.clamp(MIN_H, (f32::from(viewport.height) - 260.).max(MIN_H));
        self.term_sync(width, window);

        let running = self.term_running();
        let count = self.term.rows.len();
        let handle = self.term.scroll.0.borrow().base_handle.clone();
        let header =
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_3()
                .h(px(30.))
                .pl_3()
                .pr_1()
                .bg(t.panel)
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .overflow_hidden()
                        .child(icon("terminal", 14., t.muted))
                        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child("Terminal"))
                        .child(div().text_size(px(12.)).text_color(t.muted).whitespace_nowrap().child(
                            "Git commands of the app and your own. One git command per line; editors and password prompts are off.",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .children(running.map(|id| {
                            btn("term-stop", "Stop").danger().tip("Stop the running command").on_click(move |_, _, _| {
                                cmdlog::cancel(id);
                            })
                        }))
                        .child(icon_btn("term-clear", "trash").tip("Clear the log").disabled(count == 0).on_click(cx.listener(
                            |this, _, _, _| {
                                cmdlog::clear(Path::new(&this.path));
                            },
                        )))
                        .child(
                            icon_btn("term-close", "x")
                                .tip("Hide the terminal (Ctrl+`). The log continues.")
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
                        ),
                );

        let body: AnyElement = if count == 0 {
            div()
                .size_full()
                .p_3()
                .text_color(t.muted)
                .child("No commands yet. The git commands of your actions in the app and the commands you type appear here.")
                .into_any_element()
        } else {
            let list = uniform_list("term", count, cx.processor(|this, range: Range<usize>, _, cx| this.render_term_rows(range, cx)))
                .track_scroll(self.term.scroll.clone())
                .size_full();
            div().relative().size_full().py(px(4.)).child(list).child(scrollbar(handle, self.term.bar.clone(), t)).into_any_element()
        };

        let line = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(32.))
            .px(px(10.))
            .bg(t.panel)
            .border_t_1()
            .border_color(t.border)
            .text_size(px(FONT))
            .on_action(cx.listener(|this, _: &super::input::Up, _, cx| this.term_recall(true, cx)))
            .on_action(cx.listener(|this, _: &super::input::Down, _, cx| this.term_recall(false, cx)))
            .child(div().flex_none().w(px(GUTTER)).font_family(MONO_FONT).text_color(t.accent).child("$"))
            .child(div().flex_1().min_w_0().overflow_hidden().child(self.term.input.clone()));

        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w_full()
                .child(splitter("term-resizer", true, t).on_mouse_down(
                    LEFT,
                    cx.listener(move |this, e: &MouseDownEvent, _, _| {
                        this.resize = Some(Resize::Term { y: e.position.y, height });
                    }),
                ))
                .child(
                    div()
                        .id("terminal")
                        .flex()
                        .flex_col()
                        .w_full()
                        .h(px(height))
                        .bg(t.bg)
                        .child(header)
                        .child(
                            div()
                                .id("term-log")
                                .flex_1()
                                .min_h_0()
                                // A click on the log puts the cursor in the command line.
                                .on_click(cx.listener(|this, _, window, cx| this.term.input.read(cx).focus(window)))
                                .child(body),
                        )
                        .child(line),
                )
                .into_any_element(),
        )
    }

    pub fn term_resize(&mut self, y: Pixels, height: f32, at: Pixels, window: &Window) {
        let dy: f32 = (at - y).into();
        let max = (f32::from(window.viewport_size().height) - 260.).max(MIN_H);
        self.term.height = (height - dy).clamp(MIN_H, max);
    }

    pub fn term_save_height(&mut self, cx: &mut Context<Self>) {
        let h = self.term.height;
        set_pref(cx, KEY_H, &h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str, output: &str) -> Entry {
        Entry {
            id: 1,
            command: command.into(),
            source: Source::User,
            output: output.into(),
            truncated: false,
            running: false,
            ok: true,
            code: Some(0),
            cancelled: false,
            ms: Some(5),
        }
    }

    #[test]
    fn long_lines_wrap_and_entries_are_separated() {
        let rows = build_rows(&[entry("git status", "one\n\ntwo"), entry("git log", &"x".repeat(70))], 60);
        let kinds: Vec<(usize, bool)> = rows.iter().map(|r| (r.entry, r.kind == Kind::Command)).collect();
        // Entry 0: command, three output rows (one is empty). A gap. Entry 1: command, two rows.
        assert_eq!(kinds, [(0, true), (0, false), (0, false), (0, false), (1, false), (1, true), (1, false), (1, false)]);
        assert_eq!(rows[6].text.len(), 60);
        assert_eq!(rows[7].text.len(), 10);
        assert!(rows[4].kind == Kind::Gap);
    }

    #[test]
    fn run_time_is_short_text() {
        assert_eq!(seconds(420), "0.42 s");
        assert_eq!(seconds(12_600), "13 s");
    }
}
