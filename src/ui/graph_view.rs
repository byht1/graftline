//! The commit graph: a virtual list of rows. The core computes the layout (`graph.rs`);
//! this file draws the lanes and the node of each visible row.

use super::repo_view::{ColKey, RepoView, Resize, ROW};
use super::theme::Theme;
use super::widgets::{avatar, clip, fmt_date, icon, rel_time, scrollbar, tip, LEFT, RIGHT};
use gpui::prelude::*;
use gpui::{
    actions, canvas, div, fill, point, px, quad, size, uniform_list, AnyElement, BorderStyle, Bounds, FontWeight, Hsla, KeyBinding,
    MouseDownEvent, PathBuilder, Pixels, Point, ScrollStrategy, SharedString, Window,
};
use graftline::graph::{EdgeKind, RowGraph};
use graftline::history::{Kind, RefLabel};
use std::ops::Range;

const LANE: f32 = 14.;
const PAD: f32 = 12.;
/// Lanes the graph column shows before the user resizes it.
const AUTO_LANES: u16 = 10;

actions!(graph, [RowDown, RowUp, PageDown, PageUp, First, Last]);

pub fn bind_keys(cx: &mut gpui::App) {
    let c = Some("Graph");
    cx.bind_keys([
        KeyBinding::new("down", RowDown, c),
        KeyBinding::new("up", RowUp, c),
        KeyBinding::new("pagedown", PageDown, c),
        KeyBinding::new("pageup", PageUp, c),
        KeyBinding::new("home", First, c),
        KeyBinding::new("end", Last, c),
    ]);
}

/// A ref label that is being dragged onto another one (merge or rebase).
#[derive(Clone)]
pub struct DraggedRef {
    pub name: String,
    pub kind: &'static str,
}

pub struct DragChip(pub String);

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = super::theme::theme(cx);
        div()
            .px_2()
            .py_1()
            .rounded(px(5.))
            .bg(t.elev)
            .border_1()
            .border_color(t.accent)
            .text_color(t.text)
            .text_size(px(12.))
            .child(self.0.clone())
    }
}

fn label_icon(l: &RefLabel) -> &'static str {
    match l.kind {
        "tag" => "tag",
        "remote" => "cloud",
        "stash" => "stash",
        "head" => "commit",
        _ if l.current => "check",
        _ => "branch",
    }
}

fn circle(center: Point<Pixels>, r: f32, bg: Hsla, border: f32, color: Hsla, style: BorderStyle) -> gpui::PaintQuad {
    let b = Bounds::new(point(center.x - px(r), center.y - px(r)), size(px(r * 2.), px(r * 2.)));
    quad(b, px(r), bg, px(border), color, style)
}

/// Draws the lanes and the node of one row inside `bounds` (the graph cell).
fn paint_row(bounds: Bounds<Pixels>, g: &RowGraph, kind: Kind, merge: bool, head: bool, t: &Theme, window: &mut Window) {
    let (x0, y0) = (bounds.left(), bounds.top());
    let w: f32 = bounds.size.width.into();
    let lane = |col: u16| PAD + col as f32 * LANE;
    let (mid, radius): (f32, f32) = (ROW / 2., 7.);
    let nx = lane(g.col);
    let at = |x: f32, y: f32| point(x0 + px(x), y0 + px(y));
    for e in &g.edges {
        let x = lane(e.col);
        // Lanes right of the visible column are clipped; skip them.
        if x.min(nx) - LANE > w {
            continue;
        }
        let mut p = PathBuilder::stroke(px(2.));
        let r = radius.min((nx - x).abs());
        let dir = if nx > x { 1. } else { -1. };
        match e.kind {
            EdgeKind::Through => {
                p.move_to(at(x, 0.));
                p.line_to(at(x, ROW));
            }
            EdgeKind::In => {
                p.move_to(at(x, 0.));
                if x == nx {
                    p.line_to(at(nx, mid));
                } else {
                    p.line_to(at(x, mid - r));
                    p.curve_to(at(x + dir * r, mid), at(x, mid));
                    p.line_to(at(nx, mid));
                }
            }
            EdgeKind::Out => {
                p.move_to(at(nx, mid));
                if x == nx {
                    p.line_to(at(x, ROW));
                } else {
                    p.line_to(at(x + dir * r, mid));
                    p.curve_to(at(x, mid + r), at(x, mid));
                    p.line_to(at(x, ROW));
                }
            }
        }
        if let Ok(path) = p.build() {
            window.paint_path(path, t.graph[e.color as usize % 8]);
        }
    }
    let color = t.graph[g.color as usize % 8];
    let c = at(nx, mid);
    if nx > w - 6. {
        // The node lies right of the visible column: show an arrow at the edge instead.
        window.paint_quad(fill(Bounds::new(at(w - 13., mid - 7.), size(px(13.), px(14.))), t.bg));
        let mut p = PathBuilder::stroke(px(2.));
        p.move_to(at(w - 10., mid - 4.5));
        p.line_to(at(w - 4., mid));
        p.line_to(at(w - 10., mid + 4.5));
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }
        return;
    }
    match kind {
        Kind::Wip => window.paint_quad(circle(c, 5., t.bg, 1.6, t.muted, BorderStyle::Dashed)),
        Kind::Stash => {
            let b = Bounds::new(point(c.x - px(4.5), c.y - px(4.5)), size(px(9.), px(9.)));
            window.paint_quad(quad(b, px(1.), t.bg, px(2.), color, BorderStyle::Solid));
        }
        Kind::Commit if head => {
            window.paint_quad(circle(c, 6., t.bg, 2., color, BorderStyle::Solid));
            window.paint_quad(circle(c, 2.6, color, 0., color, BorderStyle::Solid));
        }
        Kind::Commit => {
            let r = if merge { 3.4 } else { 4.6 };
            window.paint_quad(circle(c, r + 2., t.bg, 0., t.bg, BorderStyle::Solid));
            window.paint_quad(circle(c, r, color, 0., color, BorderStyle::Solid));
        }
    }
}

impl RepoView {
    pub fn graph_width(&self) -> f32 {
        if self.cols.graph > 0. {
            return self.cols.graph;
        }
        let width = self.history.as_ref().map(|h| h.layout.width).unwrap_or(2);
        width.clamp(2, AUTO_LANES) as f32 * LANE + PAD * 2. - LANE + 8.
    }

    fn move_selection(&mut self, to: impl Fn(i64, i64) -> i64, window: &Window, cx: &mut Context<Self>) {
        let Some(total) = self.history.as_ref().map(|h| h.commits.len() as i64).filter(|n| *n > 0) else { return };
        let view: f32 = self.graph_scroll.0.borrow().base_handle.bounds().size.height.into();
        let _ = window;
        let page = ((view / ROW).floor() as i64 - 1).max(1);
        let cur = match self.sel_row() {
            Some(r) => r as i64,
            None => {
                let top: f32 = (-self.graph_scroll.0.borrow().base_handle.offset().y).into();
                (top / ROW).floor() as i64 - 1
            }
        };
        let row = to(cur, page).clamp(0, total - 1) as usize;
        self.select_row(row, false, cx);
        // Keep the row in view without centering it on every step.
        let handle = self.graph_scroll.0.borrow().base_handle.clone();
        let top: f32 = (-handle.offset().y).into();
        let y = row as f32 * ROW;
        if y < top {
            self.graph_scroll.scroll_to_item(row, ScrollStrategy::Top);
        } else if y + ROW > top + view {
            self.graph_scroll.scroll_to_item(row, ScrollStrategy::Bottom);
        }
    }

    fn render_graph_head(&self, t: &Theme, show_date: bool, show_sha: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let cell = |label: &'static str| {
            div().flex().items_center().h_full().px_2().text_size(px(11.)).text_color(t.dim).font_weight(FontWeight::SEMIBOLD).child(label)
        };
        let handle = |id: &'static str, key: ColKey, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex_none()
                .w(px(5.))
                .h_full()
                .cursor_col_resize()
                .border_r_1()
                .border_color(t.border)
                .hover(|s| s.border_color(t.accent))
                .on_mouse_down(
                    LEFT,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        if e.click_count >= 2 {
                            // Double click: back to the default width.
                            let d = super::repo_view::Cols::default();
                            match key {
                                ColKey::Refs => this.cols.refs = d.refs,
                                ColKey::Graph => this.cols.graph = d.graph,
                                ColKey::Author => this.cols.author = d.author,
                            }
                            super::set_pref(cx, "gl.graphCols", &this.cols.clone());
                            cx.notify();
                            return;
                        }
                        let width = match key {
                            ColKey::Refs => this.cols.refs,
                            ColKey::Graph => this.graph_width(),
                            ColKey::Author => this.cols.author,
                        };
                        this.resize = Some(Resize::Col { key, x: e.position.x, width });
                    }),
                )
        };
        div()
            .flex()
            .flex_none()
            .h(px(26.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(self.cols.refs))
                    .child(cell("Branch / Tag").flex_1().min_w_0().overflow_hidden())
                    .child(handle("col-refs", ColKey::Refs, cx)),
            )
            .child(
                div().flex().flex_none().w(px(self.graph_width())).child(cell("Graph").flex_1().min_w_0().overflow_hidden()).child(handle(
                    "col-graph",
                    ColKey::Graph,
                    cx,
                )),
            )
            .child(cell("Message").flex_1().min_w_0())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(self.cols.author))
                    .child(handle("col-author", ColKey::Author, cx))
                    .child(cell("Author").flex_1().min_w_0().overflow_hidden()),
            )
            .children(show_date.then(|| cell("Date").flex_none().w(px(self.cols.date))))
            .children(show_sha.then(|| cell("Hash").flex_none().w(px(self.cols.sha))))
    }

    fn render_rows(&mut self, range: Range<usize>, show_date: bool, show_sha: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = super::theme::theme(cx);
        let Some(h) = self.history.clone() else { return Vec::new() };
        let end = range.end.min(h.commits.len());
        let graphs = h.layout.rows(range.start, end);
        let sel = self.sel_row();
        let max_labels = if self.cols.refs >= 230. { 2 } else { 1 };
        let mut out = Vec::with_capacity(graphs.len());
        for (i, g) in graphs.into_iter().enumerate() {
            let row = range.start + i;
            let c = &h.commits[row];
            let wip = c.kind == Kind::Wip;
            let color = t.graph[g.color as usize % 8];
            let labels: &[RefLabel] = h.labels.get(&c.oid).map(Vec::as_slice).unwrap_or(&[]);
            let head = labels.iter().any(|l| l.current);
            let merge = h.parents[row].len() > 1;
            let (name, email) = h.author(c.author);
            let matched = self.matches.as_ref().map(|m| m.set.contains(&(row as u32)));

            let mut refs = div().flex().flex_none().items_center().gap_1().w(px(self.cols.refs)).h_full().px_1().overflow_hidden();
            for (li, l) in labels.iter().take(max_labels).enumerate() {
                let (ref_name, ref_kind) = (l.name.clone(), l.kind);
                let drag = DraggedRef { name: l.name.clone(), kind: l.kind };
                let drop_name = l.name.clone();
                let can_drag = matches!(l.kind, "branch" | "remote");
                let mut chip = div()
                    .id(("ref", row * 4 + li))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .max_w_full()
                    .h(px(18.))
                    .px(px(5.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(color)
                    .text_size(px(11.))
                    .text_color(if l.current { t.bg } else { color })
                    .when(l.current, |el| el.bg(color).font_weight(FontWeight::SEMIBOLD))
                    .tooltip(tip(l.name.clone()))
                    .child(icon(label_icon(l), 11., if l.current { t.bg } else { color }))
                    .child(clip(l.name.clone()))
                    .on_mouse_down(
                        RIGHT,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.select_row(row, false, cx);
                            this.commit_menu(row, Some((ref_name.clone(), ref_kind)), e.position, cx);
                        }),
                    );
                if can_drag {
                    chip = chip
                        .on_drag(drag, |d: &DraggedRef, _, _, cx| cx.new(|_| DragChip(d.name.clone())))
                        .drag_over::<DraggedRef>(|s, _, _, _| s.border_2())
                        .on_drop(cx.listener(move |this, d: &DraggedRef, window, cx| {
                            this.ref_dropped(d.clone(), drop_name.clone(), ref_kind, window.mouse_position(), cx)
                        }));
                }
                refs = refs.child(chip);
            }
            if labels.len() > max_labels {
                let rest = labels[max_labels..].iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join("\n");
                refs = refs.child(
                    div()
                        .id(("ref-more", row))
                        .flex_none()
                        .px_1()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(t.border_strong)
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .tooltip(tip(rest))
                        .child(format!("+{}", labels.len() - max_labels)),
                );
            }

            let theme_for_paint = t.clone();
            let kind = c.kind;
            let graph_cell = div().flex_none().w(px(self.graph_width())).h_full().overflow_hidden().child(
                canvas(|_, _, _| (), move |bounds, _, window, _| paint_row(bounds, &g, kind, merge, head, &theme_for_paint, window))
                    .size_full(),
            );

            let summary: SharedString = if wip {
                "Uncommitted changes".into()
            } else if c.summary.is_empty() {
                "(no message)".into()
            } else {
                SharedString::from(c.summary.to_string())
            };
            let selected = sel == Some(row);
            let mut el = div()
                .id(("row", row))
                .flex()
                .items_center()
                .h(px(ROW))
                .w_full()
                .text_size(px(13.))
                .when(selected, |el| el.bg(t.row_sel))
                .when(!selected, |el| el.hover(|s| s.bg(t.hover)))
                .when(matched == Some(false), |el| el.opacity(0.4))
                .child(refs)
                .child(graph_cell)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .when(wip, |el| el.text_color(t.muted).italic())
                        .when(matched == Some(true), |el| el.text_color(t.accent))
                        .when(head && !wip, |el| el.font_weight(FontWeight::SEMIBOLD))
                        .child(clip(summary)),
                )
                .child(
                    div()
                        .id(("author", row))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .w(px(self.cols.author))
                        .px_2()
                        .text_color(t.muted)
                        .when(!wip, |el| el.child(avatar(name, email, 18.)).child(clip(name.to_string())).tooltip(tip(email.to_string()))),
                );
            if show_date {
                el = el.child(
                    div()
                        .id(("date", row))
                        .flex_none()
                        .w(px(self.cols.date))
                        .px_2()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .when(!wip, |el| el.child(clip(rel_time(c.time))).tooltip(tip(fmt_date(c.time)))),
                );
            }
            if show_sha {
                let sha = if wip { String::new() } else { c.oid.to_string()[..7].to_string() };
                el = el.child(
                    div()
                        .flex_none()
                        .w(px(self.cols.sha))
                        .px_2()
                        .font_family(super::MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .child(sha),
                );
            }
            let el = el
                .on_click(cx.listener(move |this, _, window, cx| {
                    window.focus(&this.focus);
                    this.select_row(row, false, cx);
                }))
                .on_mouse_down(
                    RIGHT,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.select_row(row, false, cx);
                        this.commit_menu(row, None, e.position, cx);
                    }),
                );
            out.push(el.into_any_element());
        }
        out
    }

    pub fn render_graph(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(e) = &self.graph_error {
            return div().p_4().flex().gap_2().text_color(t.danger).child(icon("alert", 16., t.danger)).child(e.clone()).into_any_element();
        }
        let Some(h) = self.history.clone() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.muted)
                .child("Loading the history…")
                .into_any_element();
        };
        // A narrow pane drops the hash column first, then the date.
        let width = self.center_width(window, cx);
        let (show_sha, show_date) = (width >= 860., width >= 700.);
        let total = h.commits.len();
        let list = uniform_list(
            "graph",
            total,
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_rows(range, show_date, show_sha, cx)),
        )
        .track_scroll(self.graph_scroll.clone())
        .size_full();
        let handle = self.graph_scroll.0.borrow().base_handle.clone();
        let empty = (total == 0).then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.muted)
                .child("This repository has no commits yet.")
        });
        div()
            .id("graph-pane")
            .key_context("Graph")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &RowDown, window, cx| this.move_selection(|cur, _| cur + 1, window, cx)))
            .on_action(cx.listener(|this, _: &RowUp, window, cx| this.move_selection(|cur, _| cur - 1, window, cx)))
            .on_action(cx.listener(|this, _: &PageDown, window, cx| this.move_selection(|cur, page| cur + page, window, cx)))
            .on_action(cx.listener(|this, _: &PageUp, window, cx| this.move_selection(|cur, page| cur - page, window, cx)))
            .on_action(cx.listener(|this, _: &First, window, cx| this.move_selection(|_, _| 0, window, cx)))
            .on_action(cx.listener(|this, _: &Last, window, cx| this.move_selection(|_, _| i64::MAX, window, cx)))
            .child(self.render_graph_head(t, show_date, show_sha, cx))
            .child(div().relative().flex_1().min_h_0().child(list).children(empty).child(scrollbar(handle, self.graph_bar.clone(), t)))
            .into_any_element()
    }
}
