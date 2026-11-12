//! Diff of one file: unified and side-by-side views, with line selection for staging.

use super::detail::status_color;
use super::highlight::{highlight, lang_of, Lang};
use super::repo_view::{Panel, RepoView};
use super::theme::Theme;
use super::widgets::{btn, clip, icon, icon_btn, scrollbar, ScrollbarState, LEFT};
use super::{call, pref, set_pref};
use git2::Oid;
use gpui::prelude::*;
use gpui::{
    div, px, uniform_list, AnyElement, Div, FontWeight, HighlightStyle, ListHorizontalSizingBehavior, MouseDownEvent, SharedString,
    StyledText, UniformListScrollHandle, Window,
};
use graftline::repo::{DiffLine, FileChange, FileDiff};
use graftline::worktree::{HunkSelection, PatchOp, Side};
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

/// Height of one diff row.
pub const LINE: f32 = 20.;

#[derive(Clone, Copy, PartialEq)]
pub enum DiffSource {
    Commit(Oid),
    Work(Side),
}

#[derive(Clone)]
pub enum DiffRow {
    Hunk(usize),
    Line(usize, usize),
    /// Side by side: the old line and the new line of one row.
    Split(usize, Option<usize>, Option<usize>),
    Note(String),
}

pub struct DiffPanel {
    pub file: FileChange,
    pub subtitle: String,
    pub source: DiffSource,
    pub diff: Option<Arc<FileDiff>>,
    pub error: Option<String>,
    pub rows: Vec<DiffRow>,
    widest: Option<usize>,
    lang: Option<Lang>,
    pub split: bool,
    pub scroll: UniformListScrollHandle,
    pub bar: ScrollbarState,
    /// Lines selected for staging: (hunk, line).
    pub picked: BTreeSet<(usize, usize)>,
    last_pick: Option<(usize, usize)>,
}

/// Pairs removed and added lines of one hunk for the side-by-side view.
pub fn split_rows(lines: &[DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut rows = Vec::new();
    let (mut dels, mut adds): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    fn flush(rows: &mut Vec<(Option<usize>, Option<usize>)>, dels: &mut Vec<usize>, adds: &mut Vec<usize>) {
        for i in 0..dels.len().max(adds.len()) {
            rows.push((dels.get(i).copied(), adds.get(i).copied()));
        }
        dels.clear();
        adds.clear();
    }
    for (i, l) in lines.iter().enumerate() {
        match l.t {
            '-' => {
                if !adds.is_empty() {
                    flush(&mut rows, &mut dels, &mut adds);
                }
                dels.push(i);
            }
            '+' => adds.push(i),
            ' ' => {
                flush(&mut rows, &mut dels, &mut adds);
                rows.push((Some(i), Some(i)));
            }
            _ => {}
        }
    }
    flush(&mut rows, &mut dels, &mut adds);
    rows
}

impl DiffPanel {
    pub fn new(file: FileChange, subtitle: String, source: DiffSource) -> Self {
        DiffPanel {
            lang: lang_of(&file.path),
            file,
            subtitle,
            source,
            diff: None,
            error: None,
            rows: Vec::new(),
            widest: None,
            split: false,
            scroll: UniformListScrollHandle::new(),
            bar: ScrollbarState::default(),
            picked: BTreeSet::new(),
            last_pick: None,
        }
    }

    pub fn staging(&self) -> Option<Side> {
        match self.source {
            DiffSource::Work(side) => Some(side),
            DiffSource::Commit(_) => None,
        }
    }

    fn build_rows(&mut self) {
        self.rows.clear();
        self.widest = None;
        let Some(d) = self.diff.clone() else { return };
        if d.binary {
            self.rows.push(DiffRow::Note("Binary file. The content is not shown.".into()));
            return;
        }
        if d.hunks.is_empty() {
            let note = match (&d.old_mode, &d.new_mode) {
                (Some(a), Some(b)) if a != b => format!("File mode changed: {a} → {b}"),
                _ if d.status == 'R' => "Renamed without changes.".into(),
                _ => "No changes in the content.".into(),
            };
            self.rows.push(DiffRow::Note(note));
            return;
        }
        let mut longest = 0;
        for (hi, hunk) in d.hunks.iter().enumerate() {
            self.rows.push(DiffRow::Hunk(hi));
            if self.split {
                for (l, r) in split_rows(&hunk.lines) {
                    self.rows.push(DiffRow::Split(hi, l, r));
                }
            } else {
                for (li, line) in hunk.lines.iter().enumerate() {
                    let len = line.s.chars().count() + line.s.matches('\t').count() * 3;
                    if len > longest {
                        longest = len;
                        self.widest = Some(self.rows.len());
                    }
                    self.rows.push(DiffRow::Line(hi, li));
                }
            }
        }
        if d.truncated {
            self.rows.push(DiffRow::Note("The diff is too large. The rest is not shown.".into()));
        }
    }

    /// The selected lines as the request the core expects.
    pub fn picked_hunks(&self) -> Vec<HunkSelection> {
        let Some(d) = &self.diff else { return Vec::new() };
        let mut out: Vec<HunkSelection> = Vec::new();
        for (hi, li) in &self.picked {
            match out.last_mut().filter(|h| h.index == *hi) {
                Some(h) => h.lines.get_or_insert_with(Vec::new).push(*li),
                None => out.push(HunkSelection { index: *hi, header: d.hunks[*hi].header.clone(), lines: Some(vec![*li]) }),
            }
        }
        out
    }

    pub fn whole_hunk(&self, hi: usize) -> Vec<HunkSelection> {
        self.diff
            .as_ref()
            .and_then(|d| d.hunks.get(hi))
            .map(|h| vec![HunkSelection { index: hi, header: h.header.clone(), lines: None }])
            .unwrap_or_default()
    }

    fn toggle_pick(&mut self, hi: usize, li: usize, shift: bool) {
        let Some(d) = self.diff.clone() else { return };
        let on = !self.picked.contains(&(hi, li));
        // Shift+click selects the changed lines between the last click and this one.
        let range = match self.last_pick.filter(|(h, _)| shift && *h == hi) {
            Some((_, last)) => last.min(li)..=last.max(li),
            None => li..=li,
        };
        for i in range {
            if !matches!(d.hunks[hi].lines.get(i).map(|l| l.t), Some('+' | '-')) {
                continue;
            }
            if on {
                self.picked.insert((hi, i));
            } else {
                self.picked.remove(&(hi, i));
            }
        }
        self.last_pick = Some((hi, li));
    }
}

/// Text of a code line with syntax colors. A tab is shown as `tab` (spaces).
fn code_text(text: &str, lang: Option<Lang>, tab: &str, t: &Theme) -> AnyElement {
    if text.is_empty() {
        return div().into_any_element();
    }
    let shown = if text.contains('\t') { text.replace('\t', tab) } else { text.to_string() };
    let spans: Vec<(Range<usize>, HighlightStyle)> = match lang {
        Some(lang) => highlight(&shown, lang)
            .into_iter()
            .map(|(r, tok)| (r, HighlightStyle { color: Some(tok.color(t)), ..Default::default() }))
            .collect(),
        None => Vec::new(),
    };
    StyledText::new(SharedString::from(shown)).with_highlights(spans).into_any_element()
}

fn number_cell(n: Option<u32>, t: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(46.))
        .h_full()
        .pr_2()
        .flex()
        .items_center()
        .justify_end()
        .text_color(t.dim)
        .text_size(px(11.))
        .child(n.map(|n| n.to_string()).unwrap_or_default())
}

impl RepoView {
    pub fn diff_panel(&self) -> Option<&DiffPanel> {
        match &self.panel {
            Some(Panel::Diff(d)) => Some(d),
            // The file history shows the diff of its selected commit.
            Some(Panel::FileHistory(f)) => f.diff.as_ref(),
            _ => None,
        }
    }

    pub fn diff_panel_mut(&mut self) -> Option<&mut DiffPanel> {
        match &mut self.panel {
            Some(Panel::Diff(d)) => Some(d),
            Some(Panel::FileHistory(f)) => f.diff.as_mut(),
            _ => None,
        }
    }

    /// Reads the diff of the open panel again.
    pub fn load_diff(&mut self, cx: &mut Context<Self>) {
        let split = pref::<String>(cx, "gl.diffMode").as_deref() == Some("split");
        let id = self.id.clone();
        let Some(panel) = self.diff_panel_mut() else { return };
        panel.split = split;
        let source = panel.source;
        let (path, old_path) = (panel.file.path.clone(), panel.file.old_path.clone());
        let key = (path.clone(), source);
        let task = call(cx, move |st| async move {
            st.with_repo(&id, move |r| match source {
                DiffSource::Commit(oid) => graftline::repo::commit_file_diff(r, oid, &path, old_path.as_deref()),
                DiffSource::Work(side) => graftline::worktree::file_diff(r, side, &path, old_path.as_deref()),
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // The panel may show another file by now.
                let Some(panel) = this.diff_panel_mut().filter(|p| p.file.path == key.0 && p.source == key.1) else { return };
                match result {
                    Ok(d) => {
                        panel.diff = Some(Arc::new(d));
                        panel.error = None;
                    }
                    Err(e) => panel.error = Some(format!("{e:#}")),
                }
                panel.picked.clear();
                panel.last_pick = None;
                panel.build_rows();
                cx.notify();
            });
        })
        .detach();
    }

    fn set_diff_mode(&mut self, split: bool, cx: &mut Context<Self>) {
        set_pref(cx, "gl.diffMode", &if split { "split" } else { "unified" });
        if let Some(panel) = self.diff_panel_mut() {
            panel.split = split;
            panel.picked.clear();
            panel.build_rows();
        }
        cx.notify();
    }

    fn render_diff_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = super::theme::theme(cx);
        let Some(panel) = self.diff_panel() else { return Vec::new() };
        let Some(d) = panel.diff.clone() else { return Vec::new() };
        let (lang, staging) = (panel.lang, panel.staging());
        let tab = " ".repeat(super::tools::settings(cx).tab_size);
        let mut out = Vec::with_capacity(range.len());
        for i in range {
            let Some(row) = panel.rows.get(i).cloned() else { break };
            let base =
                div().flex().items_center().w_full().h(px(LINE)).font_family(super::MONO_FONT).text_size(px(12.5)).whitespace_nowrap();
            let el: AnyElement = match row {
                DiffRow::Note(text) => base.px_3().font_family(super::UI_FONT).text_color(t.muted).child(text).into_any_element(),
                DiffRow::Hunk(hi) => {
                    let mut el = base.gap_3().px_2().bg(t.panel2).text_color(t.dim).child(SharedString::from(d.hunks[hi].header.clone()));
                    let small = |id: (&'static str, usize), label: &'static str, name: &'static str| {
                        div()
                            .id(id)
                            .flex()
                            .items_center()
                            .gap_1()
                            .px(px(6.))
                            .h(px(17.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(t.border_strong)
                            .font_family(super::UI_FONT)
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(t.hover).text_color(t.text))
                            .child(icon(name, 11., t.muted))
                            .child(label)
                    };
                    match staging {
                        Some(Side::Unstaged) => {
                            el = el
                                .child(small(("hunk-stage", hi), "Stage hunk", "plus").on_click(cx.listener(move |this, _, _, cx| {
                                    let hunks = this.diff_panel().map(|p| p.whole_hunk(hi)).unwrap_or_default();
                                    this.apply_hunks(PatchOp::Stage, hunks, String::new(), cx);
                                })))
                                .child(small(("hunk-discard", hi), "Discard hunk", "undo").on_click(cx.listener(move |this, _, _, cx| {
                                    let hunks = this.diff_panel().map(|p| p.whole_hunk(hi)).unwrap_or_default();
                                    this.discard_hunks(hunks, "this hunk".into(), cx);
                                })));
                        }
                        Some(Side::Staged) => {
                            el = el.child(small(("hunk-unstage", hi), "Unstage hunk", "minus").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let hunks = this.diff_panel().map(|p| p.whole_hunk(hi)).unwrap_or_default();
                                    this.apply_hunks(PatchOp::Unstage, hunks, String::new(), cx);
                                },
                            )));
                        }
                        None => {}
                    }
                    el.into_any_element()
                }
                DiffRow::Line(hi, li) => {
                    let l = &d.hunks[hi].lines[li];
                    if l.t == '\\' {
                        base.pl(px(108.)).text_color(t.dim).child(format!("\\ {}", l.s)).into_any_element()
                    } else {
                        let changed = l.t == '+' || l.t == '-';
                        let pickable = staging.is_some() && changed;
                        let picked = pickable && panel.picked.contains(&(hi, li));
                        let bg = match l.t {
                            '+' => Some(t.new_bg),
                            '-' => Some(t.del_bg),
                            _ => None,
                        };
                        let sign = match l.t {
                            '+' => "+",
                            '-' => "−",
                            _ => "",
                        };
                        let mut gutter = div()
                            .id(("gutter", i))
                            .flex()
                            .flex_none()
                            .h_full()
                            .when(picked, |el| el.bg(t.accent_soft))
                            .child(number_cell(l.o, &t))
                            .child(number_cell(l.n, &t))
                            .child(div().flex_none().w(px(16.)).text_color(if l.t == '+' { t.ok } else { t.danger }).child(sign));
                        if pickable {
                            gutter = gutter.cursor_pointer().hover(|s| s.bg(t.accent_soft)).on_mouse_down(
                                LEFT,
                                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    if let Some(p) = this.diff_panel_mut() {
                                        p.toggle_pick(hi, li, e.modifiers.shift);
                                    }
                                    cx.notify();
                                }),
                            );
                        }
                        base.when_some(bg, |el, bg| el.bg(bg))
                            .when(picked, |el| el.border_l_2().border_color(t.accent))
                            .child(gutter)
                            .child(div().pr_4().child(code_text(&l.s, lang, &tab, &t)))
                            .into_any_element()
                    }
                }
                DiffRow::Split(hi, left, right) => {
                    let half = |li: Option<usize>, old: bool| {
                        let cell = div().flex().items_center().w_1_2().h_full().overflow_hidden();
                        let Some(l) = li.map(|li| &d.hunks[hi].lines[li]) else { return cell.bg(t.panel) };
                        // A context line is on both sides; a changed line only on its own side.
                        let (bg, sign) = match l.t {
                            '+' => (Some(t.new_bg), "+"),
                            '-' => (Some(t.del_bg), "−"),
                            _ => (None, ""),
                        };
                        cell.when_some(bg, |el, bg| el.bg(bg))
                            .child(number_cell(if old { l.o } else { l.n }, &t))
                            .child(div().flex_none().w(px(16.)).text_color(if l.t == '+' { t.ok } else { t.danger }).child(sign))
                            .child(div().flex_1().min_w_0().overflow_hidden().child(code_text(&l.s, lang, &tab, &t)))
                    };
                    base.child(half(left, true))
                        .child(div().flex_none().w(px(1.)).h_full().bg(t.border))
                        .child(half(right, false))
                        .into_any_element()
                }
            };
            out.push(el);
        }
        out
    }

    pub fn render_diff(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.diff_panel() else { return div().into_any_element() };
        let f = &panel.file;
        let path = match &f.old_path {
            Some(old) => format!("{old} → {}", f.path),
            None => f.path.clone(),
        };
        let staging = panel.staging();
        let n = panel.picked.len();
        let lines = |n: usize| format!("{n} line{}", if n == 1 { "" } else { "s" });
        let mut actions = div().flex().flex_none().items_center().gap_2();
        if let Some(side) = staging {
            if n == 0 {
                let hint = if panel.split { "Switch to Unified to select single lines" } else { "Click line numbers to select lines" };
                actions = actions.child(div().text_size(px(12.)).text_color(t.dim).child(hint));
            } else if side == Side::Unstaged {
                actions = actions
                    .child(btn("diff-discard-lines", format!("Discard {}", lines(n))).icon("undo").danger().on_click(cx.listener(
                        move |this, _, _, cx| {
                            let hunks = this.diff_panel().map(|p| p.picked_hunks()).unwrap_or_default();
                            this.discard_hunks(hunks, format!("{n} selected line{}", if n == 1 { "" } else { "s" }), cx);
                        },
                    )))
                    .child(btn("diff-stage-lines", format!("Stage {}", lines(n))).icon("plus").primary().on_click(cx.listener(
                        |this, _, _, cx| {
                            let hunks = this.diff_panel().map(|p| p.picked_hunks()).unwrap_or_default();
                            this.apply_hunks(PatchOp::Stage, hunks, String::new(), cx);
                        },
                    )));
            } else {
                actions = actions.child(btn("diff-unstage-lines", format!("Unstage {}", lines(n))).icon("minus").primary().on_click(
                    cx.listener(|this, _, _, cx| {
                        let hunks = this.diff_panel().map(|p| p.picked_hunks()).unwrap_or_default();
                        this.apply_hunks(PatchOp::Unstage, hunks, String::new(), cx);
                    }),
                ));
            }
        }
        let split = panel.split;
        let seg = div()
            .flex()
            .flex_none()
            .child(
                btn("diff-unified", "Unified")
                    .active(!split)
                    .tip("One column")
                    .on_click(cx.listener(|this, _, _, cx| this.set_diff_mode(false, cx))),
            )
            .child(
                btn("diff-split", "Split")
                    .active(split)
                    .tip("Old and new side by side")
                    .on_click(cx.listener(|this, _, _, cx| this.set_diff_mode(true, cx))),
            );
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .h(px(40.))
            .px_3()
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex_none()
                    .font_family(super::MONO_FONT)
                    .font_weight(FontWeight::BOLD)
                    .text_color(status_color(f.status, t))
                    .child(f.status.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(clip(path))
                    .child(div().flex_none().text_color(t.muted).text_size(px(12.)).child(panel.subtitle.clone())),
            )
            .child(actions)
            .child(seg)
            .child(icon_btn("diff-close", "x").tip("Close (Esc)").on_click(cx.listener(|this, _, window, cx| {
                this.panel = None;
                window.focus(&this.focus);
                cx.notify();
            })));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(div().flex_1().min_h_0().child(self.render_diff_body(t, cx)))
            .into_any_element()
    }

    /// The lines of the open diff, without the header.
    pub fn render_diff_body(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.diff_panel() else { return div().into_any_element() };
        let split = panel.split;
        if let Some(e) = &panel.error {
            div().p_4().flex().gap_2().text_color(t.danger).child(icon("alert", 15., t.danger)).child(e.clone()).into_any_element()
        } else if panel.diff.is_none() {
            div().p_4().text_color(t.muted).child("Loading…").into_any_element()
        } else {
            let (count, widest, handle) = (panel.rows.len(), panel.widest, panel.scroll.0.borrow().base_handle.clone());
            let mut list = uniform_list("diff", count, cx.processor(|this, range: Range<usize>, _, cx| this.render_diff_rows(range, cx)))
                .track_scroll(panel.scroll.clone())
                .size_full();
            if !split {
                list = list.with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained).with_width_from_item(widest);
            }
            div().relative().size_full().child(list).child(scrollbar(handle, panel.bar.clone(), t)).into_any_element()
        }
    }

    /// The panel that covers the graph.
    pub fn render_panel(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        match &self.panel {
            Some(Panel::Diff(_)) => self.render_diff(t, cx),
            Some(Panel::FileHistory(_)) => self.render_file_history(t, cx),
            Some(Panel::Blame(_)) => self.render_blame(t, cx),
            Some(Panel::Conflict(_)) => self.render_conflict(t, window, cx),
            None => div().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: char) -> DiffLine {
        DiffLine { t, o: None, n: None, s: String::new() }
    }

    #[test]
    fn removed_and_added_lines_are_paired() {
        let lines: Vec<DiffLine> = " --++ +".chars().map(line).collect();
        let rows = split_rows(&lines);
        assert_eq!(rows, vec![(Some(0), Some(0)), (Some(1), Some(3)), (Some(2), Some(4)), (Some(5), Some(5)), (None, Some(6))]);
    }

    #[test]
    fn selected_lines_are_grouped_by_hunk() {
        let hunk = |header: &str| graftline::repo::Hunk {
            header: header.into(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: vec![line('-'), line('+'), line('+')],
        };
        let file = FileChange { path: "a.txt".into(), old_path: None, status: 'M', additions: None, deletions: None, binary: false };
        let mut p = DiffPanel::new(file, String::new(), DiffSource::Work(Side::Unstaged));
        p.diff = Some(Arc::new(FileDiff { hunks: vec![hunk("@@ a @@"), hunk("@@ b @@")], ..Default::default() }));
        p.toggle_pick(0, 1, false);
        p.toggle_pick(1, 0, false);
        p.toggle_pick(1, 2, true);
        let sel = p.picked_hunks();
        assert_eq!(sel.len(), 2);
        assert_eq!((sel[0].index, sel[0].lines.clone()), (0, Some(vec![1])));
        assert_eq!((sel[1].header.as_str(), sel[1].lines.clone()), ("@@ b @@", Some(vec![0, 1, 2])));
    }
}
