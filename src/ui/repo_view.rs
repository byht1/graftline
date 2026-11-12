//! One open repository: state, loading, toolbar and layout. The parts are in their own
//! files as further `impl RepoView` blocks: graph, details, diff, changes, actions, history.

use super::diff_view::DiffPanel;
use super::input::{field, InputEvent, TextInput};
use super::overlay;
use super::theme::{theme, Theme};
use super::widgets::{badge, btn, icon, icon_btn, splitter, tip, ScrollbarState, LEFT};
use super::{call, pref, set_pref};
use git2::Oid;
use gpui::prelude::*;
use gpui::{
    div, point, px, App, Entity, FocusHandle, FontWeight, MouseMoveEvent, Pixels, ScrollStrategy, Task, UniformListScrollHandle, Window,
};
use graftline::extras::Extras;
use graftline::history::{History, Kind};
use graftline::repo::{CommitDetail, Refs};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Height of a graph row.
pub const ROW: f32 = 28.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sel {
    /// The row of uncommitted changes.
    Wip,
    Commit(Oid),
}

pub enum Detail {
    None,
    Loading,
    Error(String),
    Commit(Arc<CommitDetail>),
    /// The changes panel.
    Wip,
}

/// Widths of the graph columns; `graph` 0 means "as wide as ten lanes".
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(default)]
pub struct Cols {
    pub refs: f32,
    pub graph: f32,
    pub author: f32,
    pub date: f32,
    pub sha: f32,
}

impl Default for Cols {
    fn default() -> Self {
        Cols { refs: 150., graph: 0., author: 140., date: 96., sha: 68. }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum ColKey {
    Refs,
    Graph,
    Author,
}

pub enum Resize {
    Detail {
        x: Pixels,
        width: f32,
    },
    Col {
        key: ColKey,
        x: Pixels,
        width: f32,
    },
    /// The border between the file lists and the commit form of the changes panel.
    CommitBox {
        y: Pixels,
        height: f32,
    },
}

pub struct Matches {
    pub list: Vec<u32>,
    pub set: HashSet<u32>,
}

/// What covers the graph: a diff, a file history, a blame, a conflict editor.
pub enum Panel {
    Diff(DiffPanel),
    FileHistory(super::tools::FileHistoryPanel),
    Blame(super::tools::BlamePanel),
    Conflict(super::history_ui::ConflictPanel),
}

pub struct RepoView {
    pub id: String,
    pub path: String,
    pub name: String,
    /// Keyboard focus of the graph.
    pub focus: FocusHandle,
    pub refs: Option<Refs>,
    pub refs_error: Option<String>,
    pub extras: Option<Extras>,
    pub history: Option<Arc<History>>,
    pub graph_error: Option<String>,
    pub graph_scroll: UniformListScrollHandle,
    pub graph_bar: ScrollbarState,
    pub selected: Option<Sel>,
    pub matches: Option<Matches>,
    pub search: Entity<TextInput>,
    pub search_label: String,
    search_task: Option<Task<()>>,
    pub cols: Cols,
    pub detail: Detail,
    detail_req: u64,
    pub detail_w: f32,
    pub detail_scroll: gpui::ScrollHandle,
    pub detail_bar: ScrollbarState,
    pub panel: Option<Panel>,
    pub resize: Option<Resize>,
    pub changes: super::changes::Changes,
    pub jobs: super::actions::JobState,
    pub opened_at: std::time::Instant,
    pub last_fetch: Option<std::time::Instant>,
    refs_task: Option<Task<()>>,
    /// Sidebar sections that show all their items.
    pub expanded: HashSet<&'static str>,
}

impl RepoView {
    pub fn new(id: String, path: String, name: String, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx).placeholder("Search commits: message, author, hash"));
        cx.subscribe(&search, |this: &mut RepoView, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                // Wait for a pause in typing.
                this.search_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(180)).await;
                    let _ = this.update(cx, |this, cx| this.run_search(false, cx));
                }));
            }
            InputEvent::ShiftEnter => this.step_match(-1, cx),
            InputEvent::Submit | InputEvent::Enter => {
                this.search_task = None;
                let q = this.search.read(cx).text().trim().to_string();
                if this.matches.is_none() && !q.is_empty() {
                    this.run_search(true, cx);
                } else {
                    this.step_match(1, cx);
                }
            }
        })
        .detach();
        let mut this = RepoView {
            changes: super::changes::Changes::new(cx),
            id,
            path,
            name,
            focus: cx.focus_handle(),
            refs: None,
            refs_error: None,
            extras: None,
            history: None,
            graph_error: None,
            graph_scroll: UniformListScrollHandle::new(),
            graph_bar: ScrollbarState::default(),
            selected: None,
            matches: None,
            search,
            search_label: String::new(),
            search_task: None,
            cols: pref(cx, "gl.graphCols").unwrap_or_default(),
            detail: Detail::None,
            detail_req: 0,
            detail_w: pref(cx, "gl.detailW").unwrap_or(380.),
            detail_scroll: gpui::ScrollHandle::new(),
            detail_bar: ScrollbarState::default(),
            panel: None,
            resize: None,
            jobs: Default::default(),
            opened_at: std::time::Instant::now(),
            last_fetch: None,
            refs_task: None,
            expanded: HashSet::new(),
        };
        this.load_refs(cx);
        this.load_history(cx);
        this.reload_changes(cx);
        this.start_auto_fetch(cx);
        this
    }

    // ------------------------------------------------------------ loading

    pub fn load_refs(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let task = call(cx, move |st| async move {
            let refs = st.with_repo(&id, graftline::repo::refs).await?;
            let extras = st.with_repo(&id, graftline::extras::extras).await.ok();
            Ok((refs, extras))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((refs, extras)) => {
                        this.refs = Some(refs);
                        this.extras = extras;
                        this.refs_error = None;
                    }
                    Err(e) => this.refs_error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.session(&id).await?.history().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(h) => this.set_history(h, cx),
                    Err(e) => this.graph_error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Replaces the history after the repository changed. The row at the top of the
    /// viewport stays where it is: its commit is found in the new history and the scroll
    /// position follows.
    fn set_history(&mut self, h: Arc<History>, cx: &mut Context<Self>) {
        self.graph_error = None;
        if let Some(old) = self.history.as_ref().filter(|old| old.fingerprint != h.fingerprint) {
            let handle = self.graph_scroll.0.borrow().base_handle.clone();
            let offset = handle.offset();
            let top: f32 = (-offset.y).into();
            let row = (top / ROW).floor().max(0.) as usize;
            let within = top - row as f32 * ROW;
            // At the top of the list, stay at the top: new commits appear there.
            if top > 0.5 {
                let anchor = old.commits.get(row).filter(|c| c.kind != Kind::Wip).and_then(|c| h.index.get(&c.oid));
                if let Some(new_row) = anchor {
                    handle.set_offset(point(offset.x, px(-(*new_row as f32 * ROW + within))));
                }
            }
        }
        let lost = match self.selected {
            Some(Sel::Commit(oid)) => !h.index.contains_key(&oid),
            Some(Sel::Wip) => h.commits.first().map(|c| c.kind) != Some(Kind::Wip),
            None => false,
        };
        self.history = Some(h);
        if lost {
            self.selected = None;
            self.detail = Detail::None;
            self.close_diff_panel();
        }
        if self.matches.is_some() {
            self.run_search(false, cx);
        }
    }

    pub fn refresh(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.load_refs(cx);
        self.reload_changes(cx);
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.session(&id).await?.rebuild(false).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(h) => {
                    this.set_history(h, cx);
                    if manual {
                        overlay::info(cx, "Refreshed");
                    }
                    cx.notify();
                }
                Err(e) => overlay::error(cx, format!("{e:#}")),
            });
        })
        .detach();
    }

    /// The repository changed on disk or by an operation of the app.
    pub fn on_repo_event(&mut self, git: bool, tree: bool, graph: bool, cx: &mut Context<Self>) {
        if tree || git {
            self.reload_changes_soon(cx);
        }
        if git {
            // Several events arrive together; read the refs once.
            self.refs_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(60)).await;
                let _ = this.update(cx, |this, cx| this.load_refs(cx));
            }));
        }
        if graph {
            self.load_history(cx);
        }
    }

    // ------------------------------------------------------------ selection

    pub fn sel_row(&self) -> Option<usize> {
        let h = self.history.as_ref()?;
        match self.selected? {
            Sel::Wip => (h.commits.first()?.kind == Kind::Wip).then_some(0),
            Sel::Commit(oid) => h.index.get(&oid).map(|r| *r as usize),
        }
    }

    pub fn has_wip(&self) -> bool {
        self.history.as_ref().and_then(|h| h.commits.first()).is_some_and(|c| c.kind == Kind::Wip)
    }

    pub fn select_row(&mut self, row: usize, reveal: bool, cx: &mut Context<Self>) {
        let Some(c) = self.history.as_ref().and_then(|h| h.commits.get(row)) else { return };
        let sel = if c.kind == Kind::Wip { Sel::Wip } else { Sel::Commit(c.oid) };
        if reveal {
            self.graph_scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
        self.show(Some(sel), cx);
    }

    /// Selects a commit by id and scrolls to it. Shows a message when the graph has no such commit.
    pub fn select_oid(&mut self, oid: &str, cx: &mut Context<Self>) {
        let row = Oid::from_str(oid).ok().and_then(|o| self.history.as_ref()?.index.get(&o).copied());
        match row {
            Some(row) => self.select_row(row as usize, true, cx),
            None => overlay::error(cx, "This commit is not in the graph"),
        }
    }

    /// Shows the details of a selection; `None` clears the panel.
    pub fn show(&mut self, sel: Option<Sel>, cx: &mut Context<Self>) {
        self.close_diff_panel();
        self.detail_req += 1;
        let req = self.detail_req;
        let same = self.selected == sel && matches!(self.detail, Detail::Commit(_) | Detail::Wip);
        self.selected = sel;
        cx.notify();
        match sel {
            None => self.detail = Detail::None,
            Some(Sel::Wip) => {
                self.detail = Detail::Wip;
                self.reload_changes(cx);
            }
            Some(Sel::Commit(oid)) => {
                if same {
                    return;
                }
                self.detail = Detail::Loading;
                let id = self.id.clone();
                let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::repo::commit_detail(r, oid)).await });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        if this.detail_req != req {
                            return;
                        }
                        this.detail = match result {
                            Ok(d) => Detail::Commit(Arc::new(d)),
                            Err(e) => Detail::Error(format!("{e:#}")),
                        };
                        this.detail_scroll.set_offset(point(px(0.), px(0.)));
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Selects the working-tree row. The graph may not have that row yet right after an
    /// operation, so the history is read again first.
    pub fn show_changes(&mut self, cx: &mut Context<Self>) {
        if self.has_wip() {
            self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.show(Some(Sel::Wip), cx);
            return;
        }
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.session(&id).await?.rebuild(false).await });
        cx.spawn(async move |this, cx| {
            if let Ok(h) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.set_history(h, cx);
                    if this.has_wip() {
                        this.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                        this.show(Some(Sel::Wip), cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Closes the panel over the graph when it is a diff.
    pub fn close_diff_panel(&mut self) {
        self.panel = None;
    }

    /// Escape: closes the panel over the graph. Returns false when there was nothing to close.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.search.read(cx).is_focused(window) {
            self.search.update(cx, |s, cx| s.set_text("", cx));
            self.run_search(false, cx);
            window.focus(&self.focus);
            return true;
        }
        let open = self.panel.is_some();
        self.panel = None;
        window.focus(&self.focus);
        cx.notify();
        open
    }

    // ------------------------------------------------------------ search

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| {
            s.focus(window);
            s.select_all_text(cx);
        });
    }

    fn run_search(&mut self, then_step: bool, cx: &mut Context<Self>) {
        let q = self.search.read(cx).text().trim().to_string();
        let Some(h) = self.history.clone().filter(|_| !q.is_empty()) else {
            self.matches = None;
            self.search_label.clear();
            cx.notify();
            return;
        };
        let found = cx.background_executor().spawn(async move { h.search(&q) });
        cx.spawn(async move |this, cx| {
            let list = found.await;
            let _ = this.update(cx, |this, cx| {
                this.search_label =
                    if list.is_empty() { "No matches".into() } else { format!("{} found", super::widgets::fmt_num(list.len())) };
                this.matches = Some(Matches { set: list.iter().copied().collect(), list });
                if then_step {
                    this.step_match(1, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves to the next (dir = 1) or previous (dir = -1) match.
    pub fn step_match(&mut self, dir: i32, cx: &mut Context<Self>) {
        let Some(m) = &self.matches else { return };
        if m.list.is_empty() {
            return;
        }
        let cur = self.sel_row().map(|r| r as i64).unwrap_or(-1);
        let idx = if dir > 0 {
            m.list.iter().position(|r| *r as i64 > cur).unwrap_or(0)
        } else {
            m.list.iter().rposition(|r| (*r as i64) < cur).unwrap_or(m.list.len() - 1)
        };
        let (row, total) = (m.list[idx] as usize, m.list.len());
        self.search_label = format!("{} / {}", super::widgets::fmt_num(idx + 1), super::widgets::fmt_num(total));
        self.select_row(row, true, cx);
    }

    // ------------------------------------------------------------ layout

    fn render_toolbar(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head = self.refs.as_ref().map(|r| &r.head);
        let mut title = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(icon("repo", 15., t.accent))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(self.name.clone()));
        if let Some(head) = head {
            let (text, name, tooltip, warn) = if head.detached {
                (
                    head.oid.clone().unwrap_or_default().chars().take(7).collect::<String>(),
                    "commit",
                    "HEAD is detached: it points to a commit, not a branch",
                    true,
                )
            } else {
                (head.branch.clone().unwrap_or_else(|| "?".into()), "branch", "Current branch", false)
            };
            let (fg, bg) = if warn { (t.text, t.mod_bg) } else { (t.accent, t.accent_soft) };
            title = title.child(
                div()
                    .id("head-badge")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px(px(7.))
                    .h(px(20.))
                    .rounded(px(10.))
                    .bg(bg)
                    .text_color(fg)
                    .text_size(px(12.))
                    .tooltip(tip(tooltip))
                    .child(icon(name, 11., fg))
                    .child(text),
            );
        }
        if let Some(state) = self.refs.as_ref().and_then(|r| r.state) {
            title = title.child(badge(state, t.text, t.mod_bg));
        }
        let n = self.changes.count();
        let has_matches = self.matches.as_ref().is_some_and(|m| !m.list.is_empty());
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .h(px(44.))
            .px_3()
            .border_b_1()
            .border_color(t.border)
            .child(div().flex().items_center().gap_3().min_w_0().child(title).child(self.render_actions(t, cx)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .children((n > 0).then(|| {
                        btn("tb-changes", "Changes")
                            .icon("edit")
                            .suffix(super::widgets::fmt_num(n))
                            .tip("Show the uncommitted changes (Ctrl+Shift+C)")
                            .on_click(cx.listener(|this, _, _, cx| this.show_changes(cx)))
                    }))
                    .child(div().w(px(250.)).child(field(&self.search, window, cx)))
                    .children(
                        (!self.search_label.is_empty()).then(|| {
                            div().px_1().text_size(px(12.)).text_color(t.muted).whitespace_nowrap().child(self.search_label.clone())
                        }),
                    )
                    .child(
                        icon_btn("search-prev", "up")
                            .tip("Previous match")
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, _, cx| this.step_match(-1, cx))),
                    )
                    .child(
                        icon_btn("search-next", "down")
                            .tip("Next match (Enter)")
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, _, cx| this.step_match(1, cx))),
                    )
                    .child(
                        icon_btn("tb-refresh", "refresh")
                            .tip("Refresh (F5)")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(true, cx))),
                    ),
            )
    }

    /// Width of the area left of the details panel, for layouts that depend on it.
    pub fn center_width(&self, window: &Window, cx: &App) -> f32 {
        let total: f32 = window.viewport_size().width.into();
        let sidebar: f32 = pref(cx, "gl.sidebarW").unwrap_or(248.);
        (total - sidebar - self.detail_w - 6.).max(200.)
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        match &self.resize {
            Some(Resize::Detail { x, width }) => {
                let dx: f32 = (e.position.x - *x).into();
                let max: f32 = f32::from(window.viewport_size().width) - 420.;
                self.detail_w = (width - dx).clamp(260., max.max(260.));
                cx.notify();
            }
            Some(Resize::Col { key, x, width }) => {
                let dx: f32 = (e.position.x - *x).into();
                match key {
                    ColKey::Refs => self.cols.refs = (width + dx).clamp(40., 900.),
                    ColKey::Graph => self.cols.graph = (width + dx).clamp(30., 900.),
                    ColKey::Author => self.cols.author = (width - dx).clamp(60., 900.),
                }
                cx.notify();
            }
            Some(Resize::CommitBox { y, height }) => {
                let dy: f32 = (e.position.y - *y).into();
                self.changes.box_h = (height - dy).clamp(120., 600.);
                cx.notify();
            }
            None => {}
        }
    }

    fn end_resize(&mut self, cx: &mut Context<Self>) {
        match self.resize.take() {
            Some(Resize::Detail { .. }) => set_pref(cx, "gl.detailW", &self.detail_w.clone()),
            Some(Resize::Col { .. }) => set_pref(cx, "gl.graphCols", &self.cols.clone()),
            Some(Resize::CommitBox { .. }) => set_pref(cx, "gl.commitBoxH", &self.changes.box_h.clone()),
            None => {}
        }
    }
}

impl Render for RepoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let center =
            div().relative().flex_1().min_w_0().h_full().child(self.render_graph(&t, window, cx)).children(
                self.panel.is_some().then(|| div().absolute().inset_0().occlude().bg(t.bg).child(self.render_panel(&t, window, cx))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(LEFT, cx.listener(|this, _, _, cx| this.end_resize(cx)))
            .child(self.render_toolbar(&t, window, cx))
            .children(self.render_banner(&t, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(center)
                    .child(splitter("detail-resizer", false, &t).on_mouse_down(
                        LEFT,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _, _| {
                            this.resize = Some(Resize::Detail { x: e.position.x, width: this.detail_w });
                        }),
                    ))
                    .child(div().flex_none().w(px(self.detail_w)).h_full().bg(t.panel).child(self.render_detail(&t, window, cx))),
            )
    }
}
