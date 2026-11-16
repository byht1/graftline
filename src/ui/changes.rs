//! The working tree panel: unstaged and staged files, staging by file, hunk and line,
//! discard, the commit form and the list of safety snapshots.

use super::detail::file_row;
use super::diff_view::{DiffPanel, DiffSource};
use super::input::{field, InputEvent, TextInput};
use super::overlay::{self, close_modal, modal_frame, ConfirmOpts, MenuItem, Para, ToastKind};
use super::repo_view::{Panel, RepoView, Resize};
use super::theme::{theme, Theme};
use super::widgets::{btn, checkbox, fmt_date, fmt_num, icon, icon_btn, scrollbar, splitter, ScrollbarState, LEFT, RIGHT};
use super::{call, pref};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Entity, FocusHandle, Focusable, FontWeight, MouseDownEvent, Task, WeakEntity, Window};
use graftline::repo::FileChange;
use graftline::safety::Snapshot;
use graftline::session::Session;
use graftline::worktree::{self, HunkSelection, PatchOp, Side, WorkStatus};
use std::time::Duration;

pub fn plural(n: usize, word: &str) -> String {
    format!("{} {word}{}", fmt_num(n), if n == 1 { "" } else { "s" })
}

pub struct Changes {
    pub status: Option<WorkStatus>,
    pub error: Option<String>,
    pub summary: Entity<TextInput>,
    pub body: Entity<TextInput>,
    pub amend: bool,
    pub busy: bool,
    seq: u64,
    timer: Option<Task<()>>,
    /// Height of the commit form.
    pub box_h: f32,
    pub scroll: gpui::ScrollHandle,
    pub bar: ScrollbarState,
}

impl Changes {
    pub fn new(cx: &mut Context<RepoView>) -> Self {
        let summary = cx.new(|cx| TextInput::new(cx).placeholder("Summary"));
        let body = cx.new(|cx| TextInput::new(cx).placeholder("Description (optional)").multiline(3));
        for input in [&summary, &body] {
            cx.subscribe(input, |this: &mut RepoView, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.do_commit(cx),
                _ => cx.notify(),
            })
            .detach();
        }
        Changes {
            status: None,
            error: None,
            summary,
            body,
            amend: false,
            busy: false,
            seq: 0,
            timer: None,
            box_h: pref(cx, "gl.commitBoxH").unwrap_or(190.),
            scroll: gpui::ScrollHandle::new(),
            bar: ScrollbarState::default(),
        }
    }

    /// Files with changes, staged and unstaged.
    pub fn count(&self) -> usize {
        self.status.as_ref().map(|s| s.staged.len() + s.unstaged.len()).unwrap_or(0)
    }
}

fn paths_of(files: &[FileChange]) -> Vec<String> {
    files.iter().flat_map(|f| std::iter::once(f.path.clone()).chain(f.old_path.clone())).collect()
}

impl RepoView {
    pub fn reload_changes(&mut self, cx: &mut Context<Self>) {
        self.changes.seq += 1;
        let (mine, id) = (self.changes.seq, self.id.clone());
        let task = call(cx, move |st| async move { st.with_repo(&id, worktree::status).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.changes.seq != mine {
                    return;
                }
                match result {
                    Ok(status) => {
                        this.changes.status = Some(status);
                        this.changes.error = None;
                    }
                    Err(e) => this.changes.error = Some(format!("{e:#}")),
                }
                // Keep the open diff in step with the file, or close it when the file has
                // no changes left.
                let open = this.diff_panel().and_then(|p| p.staging().map(|side| (side, p.file.path.clone())));
                if let (Some((side, path)), Some(s)) = (open, &this.changes.status) {
                    let list = if side == Side::Staged { &s.staged } else { &s.unstaged };
                    if list.iter().any(|f| f.path == path) {
                        this.load_diff(cx);
                    } else {
                        this.panel = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn reload_changes_soon(&mut self, cx: &mut Context<Self>) {
        self.changes.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(80)).await;
            let _ = this.update(cx, |this, cx| this.reload_changes(cx));
        }));
    }

    /// Runs a change of the repository in the core. `done` gets the result on success; an
    /// error is shown. The status is read again in both cases.
    pub fn mutate<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&git2::Repository, &Session) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.mutate(&id, f).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(value) => done(this, value, cx),
                    Err(e) => overlay::show_error(cx, &e),
                }
                this.reload_changes(cx);
            });
        })
        .detach();
    }

    pub fn stage_files(&mut self, files: Vec<FileChange>, cx: &mut Context<Self>) {
        let paths = paths_of(&files);
        self.mutate(cx, move |r, _| worktree::stage(r, &paths), |_, _, _| {});
    }

    pub fn unstage_files(&mut self, files: Vec<FileChange>, cx: &mut Context<Self>) {
        let paths = paths_of(&files);
        self.mutate(cx, move |r, _| worktree::unstage(r, &paths), |_, _, _| {});
    }

    /// A success notification with **Undo** when a safety snapshot exists.
    pub fn undo_toast(&self, text: String, snapshot: Option<Snapshot>, cx: &mut Context<Self>) {
        let Some(snapshot) = snapshot else {
            overlay::success(cx, text);
            return;
        };
        let this = cx.entity().downgrade();
        overlay::toast_action(cx, text, ToastKind::Success, "Undo", move |_, cx| {
            let name = snapshot.name.clone();
            let _ = this.update(cx, |this, cx| {
                this.mutate(cx, move |r, _| graftline::safety::restore(r, &name), |_, _, cx| overlay::success(cx, "Restored"));
            });
        });
    }

    pub fn discard_files(&mut self, files: Vec<FileChange>, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let untracked = files.iter().filter(|f| f.status == '?').count();
        let mut list: Vec<String> = files.iter().take(12).map(|f| format!("{}  {}", f.status, f.path)).collect();
        if files.len() > 12 {
            list.push(format!("… and {} more", files.len() - 12));
        }
        let mut body =
            vec![Para::Text("The unstaged changes of these files are removed from the working tree:".into()), Para::Code(list.join("\n"))];
        if untracked > 0 {
            body.push(Para::Warn(format!("{} will be deleted.", plural(untracked, "untracked file"))));
        }
        body.push(Para::Muted(
            "The current content is saved in a safety snapshot first. You can undo the discard from the notification or from Safety snapshots.".into(),
        ));
        let title = if files.len() == 1 {
            format!("Discard changes in {}?", files[0].path)
        } else {
            format!("Discard changes in {}?", plural(files.len(), "file"))
        };
        let this = cx.entity().downgrade();
        overlay::confirm(cx, ConfirmOpts { title, body, confirm: "Discard".into(), danger: true }, move |_, cx| {
            let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
            let n = paths.len();
            let _ = this.update(cx, |this, cx| {
                this.mutate(
                    cx,
                    move |r, _| worktree::discard(r, &paths),
                    move |this, snapshot, cx| this.undo_toast(format!("Discarded {}", plural(n, "file")), snapshot, cx),
                );
            });
        });
    }

    /// Stages, unstages or discards hunks or lines of the file in the diff panel.
    pub fn apply_hunks(&mut self, op: PatchOp, hunks: Vec<HunkSelection>, label: String, cx: &mut Context<Self>) {
        let Some(path) = self.diff_panel().map(|p| p.file.path.clone()) else { return };
        if hunks.is_empty() {
            return;
        }
        self.mutate(
            cx,
            move |r, _| worktree::apply_selection(r, op, &path, &hunks),
            move |this, snapshot, cx| {
                if op == PatchOp::Discard {
                    this.undo_toast(label, snapshot, cx);
                }
            },
        );
    }

    pub fn discard_hunks(&mut self, hunks: Vec<HunkSelection>, what: String, cx: &mut Context<Self>) {
        let Some(path) = self.diff_panel().map(|p| p.file.path.clone()) else { return };
        let mut upper = what.clone();
        if let Some(first) = upper.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: format!("Discard {what}?"),
                body: vec![
                    Para::Text(format!("{upper} of {path} will be removed from the working tree.")),
                    Para::Muted("The file is saved in a safety snapshot first. You can undo the discard from the notification.".into()),
                ],
                confirm: "Discard".into(),
                danger: true,
            },
            move |_, cx| {
                let _ = this.update(cx, |this, cx| this.apply_hunks(PatchOp::Discard, hunks.clone(), format!("Discarded {what}"), cx));
            },
        );
    }

    fn open_work_file(&mut self, f: &FileChange, side: Side, cx: &mut Context<Self>) {
        let subtitle = if side == Side::Staged { "staged" } else { "unstaged" };
        self.panel = Some(Panel::Diff(DiffPanel::new(f.clone(), subtitle.into(), DiffSource::Work(side))));
        self.load_diff(cx);
        cx.notify();
    }

    // ------------------------------------------------------------ commit

    /// The current branch when its last commit is already on the upstream.
    fn published_upstream(&self) -> Option<String> {
        let b = self.refs.as_ref()?.branches.iter().find(|b| b.current)?;
        (b.upstream.is_some() && !b.gone && b.ahead == 0).then(|| b.upstream.clone()).flatten()
    }

    fn toggle_amend(&mut self, on: bool, cx: &mut Context<Self>) {
        self.changes.amend = on;
        cx.notify();
        let empty = self.changes.summary.read(cx).text().is_empty() && self.changes.body.read(cx).text().is_empty();
        let head = self.refs.as_ref().and_then(|r| r.head.oid.clone()).and_then(|o| git2::Oid::from_str(&o).ok());
        let (Some(oid), true, true) = (head, on, empty) else { return };
        // Start from the message of the commit that is amended.
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::repo::commit_detail(r, oid)).await });
        cx.spawn(async move |this, cx| {
            if let Ok(c) = task.await {
                let _ = this.update(cx, |this, cx| {
                    let still_empty = this.changes.summary.read(cx).text().is_empty() && this.changes.body.read(cx).text().is_empty();
                    if this.changes.amend && still_empty {
                        this.changes.summary.update(cx, |i, cx| i.set_text(c.summary.clone(), cx));
                        this.changes.body.update(cx, |i, cx| i.set_text(c.body.clone(), cx));
                    }
                });
            }
        })
        .detach();
    }

    fn can_commit(&self, cx: &App) -> bool {
        let Some(s) = &self.changes.status else { return false };
        !self.changes.summary.read(cx).text().trim().is_empty()
            && (!s.staged.is_empty() || self.changes.amend)
            && s.conflicted.is_empty()
            && !self.changes.busy
    }

    pub fn do_commit(&mut self, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        match self.published_upstream().filter(|_| self.changes.amend) {
            Some(upstream) => {
                let this = cx.entity().downgrade();
                overlay::confirm(
                    cx,
                    ConfirmOpts {
                        title: "Amend a published commit?".into(),
                        body: vec![
                            Para::Text(format!(
                                "The last commit is already on {upstream}. After the amend, your branch and {upstream} have different histories: a push needs force, and others who pulled the commit must repair their branches."
                            )),
                            Para::Muted("The old commit stays in the reflog. You can undo the amend from the notification.".into()),
                        ],
                        confirm: "Amend anyway".into(),
                        danger: true,
                    },
                    move |_, cx| {
                        let _ = this.update(cx, |this, cx| this.commit_now(cx));
                    },
                );
            }
            None => self.commit_now(cx),
        }
    }

    fn commit_now(&mut self, cx: &mut Context<Self>) {
        let summary = self.changes.summary.read(cx).text().trim().to_string();
        let body = self.changes.body.read(cx).text().trim().to_string();
        let message = if body.is_empty() { summary } else { format!("{summary}\n\n{body}") };
        let amend = self.changes.amend;
        let has_git = super::core(cx).git.is_some();
        let req = worktree::CommitRequest { message, amend };
        self.changes.busy = true;
        cx.notify();
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.mutate(&id, move |r, s| worktree::commit(r, &s.root, has_git, &req)).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.changes.busy = false;
                match result {
                    Ok(res) => {
                        this.changes.summary.update(cx, |i, cx| i.set_text("", cx));
                        this.changes.body.update(cx, |i, cx| i.set_text("", cx));
                        this.changes.amend = false;
                        let text = format!("{} {}", if amend { "Amended" } else { "Committed" }, &res.oid[..7.min(res.oid.len())]);
                        match res.previous.clone() {
                            Some(previous) => {
                                let (weak, head) = (cx.entity().downgrade(), res.oid.clone());
                                overlay::toast_action(cx, text, ToastKind::Success, "Undo", move |_, cx| {
                                    let (previous, head) = (previous.clone(), head.clone());
                                    let _ = weak.update(cx, |this, cx| {
                                        this.mutate(
                                            cx,
                                            move |r, _| worktree::reset_soft(r, &previous, &head),
                                            |_, _, cx| overlay::success(cx, "Commit undone. The changes are staged again."),
                                        );
                                    });
                                });
                            }
                            None => overlay::success(cx, text),
                        }
                    }
                    Err(e) => overlay::show_error(cx, &e),
                }
                this.reload_changes(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ render

    fn work_row(&self, i: usize, f: &FileChange, side: Side, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let conflict = f.status == 'U';
        let active = self.diff_panel().is_some_and(|p| p.staging() == Some(side) && p.file.path == f.path)
            || matches!(&self.panel, Some(Panel::Conflict(c)) if conflict && c.path == f.path);
        let tag = if side == Side::Staged { "staged" } else { "unstaged" };
        let small = |id: (&'static str, usize), label: &'static str| {
            div()
                .id(id)
                .px(px(6.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(t.border_strong)
                .text_size(px(11.))
                .cursor_pointer()
                .hover(|s| s.bg(t.hover))
                .child(label)
        };
        let mut actions = div().flex().flex_none().items_center().gap_1();
        let file = f.clone();
        if conflict {
            let (p1, p2, f3) = (f.path.clone(), f.path.clone(), f.clone());
            actions = actions
                .child(small(("ours", i), "Ours").tooltip(super::widgets::tip("Use our version of the whole file")).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.resolve_side(p1.clone(), "ours", cx);
                    },
                )))
                .child(small(("theirs", i), "Theirs").tooltip(super::widgets::tip("Use their version of the whole file")).on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.resolve_side(p2.clone(), "theirs", cx);
                    }),
                ))
                .child(icon_btn(("resolved", i), "check").icon_size(14.).tip("Mark as resolved: stage the file as it is").on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stage_files(vec![f3.clone()], cx);
                    }),
                ));
        } else if side == Side::Unstaged {
            let (f1, f2) = (f.clone(), f.clone());
            actions = actions
                .child(icon_btn(("discard", i), "undo").icon_size(13.).tip("Discard changes").on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.discard_files(vec![f1.clone()], cx);
                    },
                )))
                .child(icon_btn(("stage", i), "plus").icon_size(14.).tip("Stage").on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.stage_files(vec![f2.clone()], cx);
                })));
        } else {
            let f1 = f.clone();
            actions = actions.child(icon_btn(("unstage", i), "minus").icon_size(14.).tip("Unstage").on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.unstage_files(vec![f1.clone()], cx);
                },
            )));
        }
        let menu_file = f.clone();
        file_row((tag, i), f, active, t)
            .child(actions)
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if conflict {
                    this.open_conflict(file.path.clone(), cx);
                } else if e.click_count() >= 2 {
                    // Double click moves the file to the other list.
                    if side == Side::Unstaged {
                        this.stage_files(vec![file.clone()], cx);
                    } else {
                        this.unstage_files(vec![file.clone()], cx);
                    }
                } else {
                    this.open_work_file(&file, side, cx);
                }
            }))
            .on_mouse_down(
                RIGHT,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    let weak = cx.entity().downgrade();
                    let f = menu_file.clone();
                    let mut items = Vec::new();
                    let (w, file) = (weak.clone(), f.clone());
                    if side == Side::Unstaged {
                        items.push(MenuItem::new("plus", "Stage", move |_, cx| {
                            let _ = w.update(cx, |this, cx| this.stage_files(vec![file.clone()], cx));
                        }));
                        let (w, file) = (weak.clone(), f.clone());
                        items.push(
                            MenuItem::new("undo", "Discard changes…", move |_, cx| {
                                let _ = w.update(cx, |this, cx| this.discard_files(vec![file.clone()], cx));
                            })
                            .danger(),
                        );
                    } else {
                        items.push(MenuItem::new("minus", "Unstage", move |_, cx| {
                            let _ = w.update(cx, |this, cx| this.unstage_files(vec![file.clone()], cx));
                        }));
                    }
                    items.push(MenuItem::sep());
                    if f.status != '?' {
                        items.extend(this.file_tools(&f.path, None, cx));
                    }
                    let path = f.path.clone();
                    items.push(MenuItem::new("copy", "Copy path", move |_, cx| overlay::copy_text(cx, path.clone(), "")));
                    overlay::open_menu(cx, e.position, items);
                }),
            )
            .into_any_element()
    }

    pub fn render_changes(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(40.))
            .px_3()
            .border_b_1()
            .border_color(t.border)
            .child(icon("edit", 15., t.accent))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Uncommitted changes"))
            .child(div().flex_1())
            .child(
                icon_btn("snapshots", "history")
                    .tip("Safety snapshots: files saved before a discard")
                    .on_click(cx.listener(|this, _, _, cx| this.open_snapshots(cx))),
            );

        let mut lists = div().flex().flex_col();
        if let Some(e) = &self.changes.error {
            lists = lists.child(div().p_3().flex().gap_2().text_color(t.danger).child(icon("alert", 15., t.danger)).child(e.clone()));
        } else if let Some(s) = self.changes.status.clone() {
            if !s.conflicted.is_empty() {
                lists = lists.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .mt_2()
                        .p_2()
                        .rounded(px(6.))
                        .bg(t.mod_bg)
                        .border_1()
                        .border_color(t.mod_border)
                        .child(icon("alert", 14., t.text))
                        .child(format!("{} with conflicts", plural(s.conflicted.len(), "file"))),
                );
            }
            let section = |title: &'static str, n: usize| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_3()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.muted)
                    .child(title)
                    .child(div().font_weight(FontWeight::NORMAL).text_color(t.dim).child(fmt_num(n)))
                    .child(div().flex_1())
            };
            let empty = |text: &'static str| div().px_3().py_2().text_size(px(12.)).text_color(t.dim).child(text);
            let small_btn = |id: &'static str, label: &'static str, name: &'static str| btn(id, label).icon(name).icon_size(12.);

            let (all_u, all_u2) = (s.unstaged.clone(), s.unstaged.clone());
            let mut unstaged = section("Unstaged", s.unstaged.len());
            if !s.unstaged.is_empty() {
                unstaged = unstaged
                    .child(
                        small_btn("discard-all", "Discard all", "undo")
                            .ghost()
                            .tip("Discard all unstaged changes")
                            .on_click(cx.listener(move |this, _, _, cx| this.discard_files(all_u.clone(), cx))),
                    )
                    .child(
                        small_btn("stage-all", "Stage all", "plus")
                            .tip("Stage all changes")
                            .on_click(cx.listener(move |this, _, _, cx| this.stage_files(all_u2.clone(), cx))),
                    );
            }
            lists = lists.child(unstaged);
            if s.unstaged.is_empty() {
                lists = lists.child(empty("No unstaged changes"));
            }
            for (i, f) in s.unstaged.iter().enumerate() {
                lists = lists.child(self.work_row(i, f, Side::Unstaged, t, cx));
            }
            let all_s = s.staged.clone();
            let mut staged = section("Staged", s.staged.len());
            if !s.staged.is_empty() {
                staged = staged.child(
                    small_btn("unstage-all", "Unstage all", "minus")
                        .tip("Unstage all changes")
                        .on_click(cx.listener(move |this, _, _, cx| this.unstage_files(all_s.clone(), cx))),
                );
            }
            lists = lists.child(staged);
            if s.staged.is_empty() {
                lists = lists.child(empty("Nothing is staged"));
            }
            for (i, f) in s.staged.iter().enumerate() {
                lists = lists.child(self.work_row(i, f, Side::Staged, t, cx));
            }
        } else {
            lists = lists.child(div().p_3().text_color(t.muted).child("Loading…"));
        }

        let n = self.changes.status.as_ref().map(|s| s.staged.len()).unwrap_or(0);
        let len = self.changes.summary.read(cx).text().chars().count();
        let amend = self.changes.amend;
        let label = if amend {
            "Amend commit".to_string()
        } else if n > 0 {
            format!("Commit {}", plural(n, "file"))
        } else {
            "Commit".to_string()
        };
        let warn = self.published_upstream().filter(|_| amend).map(|upstream| {
            div()
                .flex()
                .gap_2()
                .p_2()
                .rounded(px(6.))
                .bg(t.mod_bg)
                .border_1()
                .border_color(t.mod_border)
                .text_size(px(12.))
                .child(icon("alert", 13., t.text))
                .child(div().flex_1().child(format!("The last commit is already on {upstream}. Amending it rewrites published history.")))
        });
        let commit_box = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_2()
            .h(px(self.changes.box_h))
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(field(&self.changes.summary, window, cx)))
                    .children((len > 0).then(|| {
                        div()
                            .id("summary-len")
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(if len > 72 { t.danger } else { t.dim })
                            .when(len > 72, |el| {
                                el.tooltip(super::widgets::tip("Summaries longer than 72 characters are cut in many tools"))
                            })
                            .child(len.to_string())
                    })),
            )
            .child(div().id("body-box").flex_1().min_h_0().overflow_y_scroll().child(field(&self.changes.body, window, cx)))
            .children(warn)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("amend-box")
                            .text_size(px(12.))
                            .tooltip(super::widgets::tip("Replace the last commit with the staged changes and this message"))
                            .child(checkbox(
                                "amend",
                                amend,
                                "Amend last commit",
                                t,
                                cx.listener(|this, on: &bool, _, cx| this.toggle_amend(*on, cx)).into_toggle(),
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        btn("commit", label)
                            .icon("check")
                            .primary()
                            .suffix("Ctrl+Enter")
                            .disabled(!self.can_commit(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.do_commit(cx))),
                    ),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(div().id("wip-lists").size_full().overflow_y_scroll().track_scroll(&self.changes.scroll).child(lists))
                    .child(scrollbar(self.changes.scroll.clone(), self.changes.bar.clone(), t)),
            )
            .child(splitter("commit-resizer", true, t).on_mouse_down(
                LEFT,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.resize = Some(Resize::CommitBox { y: e.position.y, height: this.changes.box_h });
                }),
            ))
            .child(commit_box)
            .into_any_element()
    }

    // ------------------------------------------------------------ snapshots

    fn open_snapshots(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, graftline::safety::list).await });
        let repo = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = task.await;
            let _ = cx.update(|cx| match result {
                Ok(list) => overlay::open_modal(cx, move |_, cx| SnapshotsDialog { repo, list, focus: cx.focus_handle() }),
                Err(e) => overlay::show_error(cx, &e),
            });
        })
        .detach();
    }
}

use super::overlay::IntoToggle;

struct SnapshotsDialog {
    repo: WeakEntity<RepoView>,
    list: Vec<Snapshot>,
    focus: FocusHandle,
}

impl Focusable for SnapshotsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SnapshotsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let mut body = div().flex().flex_col().gap_2();
        if self.list.is_empty() {
            body = body.child(
                div()
                    .text_color(t.muted)
                    .child("No snapshots yet. Graftline saves one each time it discards or overwrites files in the working tree."),
            );
        }
        for (i, s) in self.list.iter().enumerate() {
            let mut files = s.files.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            if s.files.len() > 3 {
                files.push_str(&format!(" and {} more", s.files.len() - 3));
            }
            let (restore, drop_name, n, repo, repo2) =
                (s.name.clone(), s.name.clone(), s.files.len(), self.repo.clone(), self.repo.clone());
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().flex().gap_2().child(div().font_weight(FontWeight::SEMIBOLD).child(s.reason.clone())).child(div().text_color(t.muted).child(fmt_date(s.time))))
                            .child(div().id(("snap-files", i)).font_family(super::MONO_FONT).text_size(px(11.)).text_color(t.muted).tooltip(super::widgets::tip(s.files.join("\n"))).child(super::widgets::clip(files))),
                    )
                    .child(btn(("snap-restore", i), "Restore").tip("Write these files back into the working tree").on_click(move |_, _, cx| {
                        let (repo, name) = (repo.clone(), restore.clone());
                        overlay::confirm(
                            cx,
                            ConfirmOpts {
                                title: "Restore this snapshot?".into(),
                                body: vec![Para::Text(format!(
                                    "{} in the working tree will be replaced with the saved content. The current content is saved in a new snapshot first.",
                                    plural(n, "file")
                                ))],
                                confirm: "Restore".into(),
                                danger: false,
                            },
                            move |_, cx| {
                                let name = name.clone();
                                let _ = repo.update(cx, |this, cx| {
                                    this.mutate(cx, move |r, _| graftline::safety::restore(r, &name), |_, files, cx| {
                                        overlay::success(cx, format!("Restored {}", plural(files.len(), "file")));
                                        // The list of snapshots is behind the confirmation.
                                        close_modal(cx);
                                    });
                                });
                            },
                        );
                    }))
                    .child(icon_btn(("snap-drop", i), "trash").icon_size(14.).tip("Delete this snapshot").on_click(cx.listener(move |this, _, _, cx| {
                        let name = drop_name.clone();
                        let Some(id) = repo2.upgrade().map(|r| r.read(cx).id.clone()) else { return };
                        let gone = name.clone();
                        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::safety::drop(r, &name)).await });
                        cx.spawn(async move |this, cx| {
                            let result = task.await;
                            let _ = this.update(cx, |this: &mut SnapshotsDialog, cx| match result {
                                Ok(()) => {
                                    this.list.retain(|s| s.name != gone);
                                    cx.notify();
                                }
                                Err(e) => overlay::show_error(cx, &e),
                            });
                        })
                        .detach();
                        let _ = this;
                    }))),
            );
        }
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame("Safety snapshots", 720., body, Vec::new(), cx))
    }
}
