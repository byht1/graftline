//! Operations that change history (merge, rebase, cherry-pick, revert, reset, interactive
//! rebase), the banner of an operation in progress, and the conflict editor.

use super::actions::item;
use super::call;
use super::changes::plural;
use super::input::{field, InputEvent, TextInput};
use super::overlay::{self, close_modal, modal_frame, ConfirmOpts, MenuItem, Para, ToastKind};
use super::repo_view::{Panel, RepoView};
use super::theme::{theme, Theme};
use super::widgets::{badge, btn, clip, icon, icon_btn, scrollbar, tip, ScrollbarState};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Entity, FocusHandle, Focusable, FontWeight, SharedString, WeakEntity, Window};
use graftline::conflicts::{Choice, ConflictDetail};
use graftline::ops::Op;
use graftline::rewrite::{HistoryOp, PlanCommit, Step};
use serde_json::Value;
use std::collections::BTreeSet;

fn state_name(state: &str) -> &'static str {
    match state {
        "merge" => "Merge",
        "rebase" => "Rebase",
        "cherry-pick" => "Cherry-pick",
        "revert" => "Revert",
        "bisect" => "Bisect",
        "am" => "Patch apply",
        _ => "The operation",
    }
}

// ---------------------------------------------------------------- conflict text

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block {
    pub ours: Vec<String>,
    pub base: Vec<String>,
    pub theirs: Vec<String>,
    /// Lines of each side that go into the result.
    pub pick_ours: BTreeSet<usize>,
    pub pick_theirs: BTreeSet<usize>,
    /// The user chose something for this block.
    pub decided: bool,
    pub theirs_first: bool,
}

impl Block {
    fn lines(&self) -> Vec<&str> {
        let ours = self.ours.iter().enumerate().filter(|(i, _)| self.pick_ours.contains(i)).map(|(_, l)| l.as_str());
        let theirs = self.theirs.iter().enumerate().filter(|(i, _)| self.pick_theirs.contains(i)).map(|(_, l)| l.as_str());
        if self.theirs_first {
            theirs.chain(ours).collect()
        } else {
            ours.chain(theirs).collect()
        }
    }

    fn set_all(&mut self, ours: bool, theirs: bool) {
        self.pick_ours = if ours { (0..self.ours.len()).collect() } else { BTreeSet::new() };
        self.pick_theirs = if theirs { (0..self.theirs.len()).collect() } else { BTreeSet::new() };
        self.decided = true;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Text(Vec<String>),
    Conflict(Block),
}

/// Splits a file with conflict markers into plain text and conflict blocks.
pub fn parse_conflicts(text: &str) -> Vec<Segment> {
    #[derive(PartialEq)]
    enum Side {
        Ours,
        Base,
        Theirs,
    }
    let mut out = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    let mut block: Option<(Block, Side)> = None;
    for line in text.split('\n') {
        match &mut block {
            None if line.starts_with("<<<<<<<") => {
                if !plain.is_empty() {
                    out.push(Segment::Text(std::mem::take(&mut plain)));
                }
                block = Some((Block::default(), Side::Ours));
            }
            None => plain.push(line.to_string()),
            Some((_, side)) if *side == Side::Ours && line.starts_with("|||||||") => *side = Side::Base,
            Some((_, side)) if *side != Side::Theirs && line.starts_with("=======") => *side = Side::Theirs,
            Some((_, side)) if *side == Side::Theirs && line.starts_with(">>>>>>>") => {
                if let Some((b, _)) = block.take() {
                    out.push(Segment::Conflict(b));
                }
            }
            Some((b, side)) => match side {
                Side::Ours => b.ours.push(line.to_string()),
                Side::Base => b.base.push(line.to_string()),
                Side::Theirs => b.theirs.push(line.to_string()),
            },
        }
    }
    // An unfinished block is not a conflict: give its lines back as text.
    if let Some((b, _)) = block {
        plain.push("<<<<<<<".into());
        plain.extend(b.ours);
        plain.extend(b.theirs);
    }
    if !plain.is_empty() {
        out.push(Segment::Text(plain));
    }
    out
}

fn has_markers(text: &str, with_separator: bool) -> bool {
    text.lines().any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>") || (with_separator && l.starts_with("=======")))
}

pub struct ConflictPanel {
    pub path: String,
    detail: Option<ConflictDetail>,
    error: Option<String>,
    segments: Vec<Segment>,
    /// The hand-edited result; `None` while the blocks decide.
    manual: Option<Entity<TextInput>>,
    scroll: gpui::ScrollHandle,
    bar: ScrollbarState,
}

impl ConflictPanel {
    fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Conflict(b) => Some(b),
            Segment::Text(_) => None,
        })
    }

    fn undecided(&self) -> usize {
        self.blocks().filter(|b| !b.decided).count()
    }

    fn result(&self) -> String {
        let mut lines: Vec<&str> = Vec::new();
        for s in &self.segments {
            match s {
                Segment::Text(t) => lines.extend(t.iter().map(String::as_str)),
                Segment::Conflict(b) => lines.extend(b.lines()),
            }
        }
        lines.join("\n")
    }
}

impl RepoView {
    fn history_op(&mut self, op: HistoryOp, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static) {
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.run_history(&id, op).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(v) => done(this, v, cx),
                    Err(e) => overlay::show_error(cx, &e),
                }
                this.reload_changes(cx);
            });
        })
        .detach();
    }

    /// Offers to undo an operation by moving the branch back to the commit it had before.
    fn done_toast(&mut self, text: String, before: Option<String>, cx: &mut Context<Self>) {
        let Some(before) = before else {
            overlay::success(cx, text);
            return;
        };
        let this = cx.entity().downgrade();
        overlay::toast_action(cx, text, ToastKind::Success, "Undo", move |_, cx| {
            let op = HistoryOp::Reset { to: before.clone(), mode: "hard".into() };
            let _ = this.update(cx, |this, cx| this.history_op(op, cx, |_, _, cx| overlay::success(cx, "Undone")));
        });
    }

    /// Handles the answer of an operation that can stop for conflicts.
    fn finish(&mut self, res: Value, text: String, cx: &mut Context<Self>) {
        match res["stopped"].as_str() {
            Some(state) => {
                overlay::error(cx, format!("{} stopped because of conflicts. Resolve them, then continue.", state_name(state)));
                self.show_changes(cx);
            }
            None => self.done_toast(text, res["before"].as_str().map(str::to_string), cx),
        }
    }

    pub fn merge(&mut self, branch: String, into: String, cx: &mut Context<Self>) {
        self.op(Op::Merge { branch: branch.clone(), into: Some(into.clone()) }, cx, move |this, res, cx| {
            if res["stopped"].is_null() && res["output"].as_str().is_some_and(|o| o.contains("Already up to date")) {
                overlay::info(cx, format!("{into} already contains {branch}"));
            } else {
                this.finish(res, format!("Merged {branch} into {into}"), cx);
            }
        });
    }

    pub fn rebase(&mut self, branch: String, onto: String, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let (b, o) = (branch.clone(), onto.clone());
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::rewrite::rebase_info(r, Some(&b), &o)).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let info = match result {
                    Ok(info) => info,
                    Err(e) => return overlay::show_error(cx, &e),
                };
                if info["commits"].as_u64().unwrap_or(0) == 0 {
                    overlay::info(cx, format!("{branch} is already based on {onto}"));
                    return;
                }
                let published = info["published"].as_u64().unwrap_or(0) as usize;
                if published == 0 {
                    this.rebase_now(branch, onto, cx);
                    return;
                }
                let weak = cx.entity().downgrade();
                overlay::confirm(
                    cx,
                    ConfirmOpts {
                        title: format!("Rebase {}?", plural(published, "published commit")),
                        body: vec![
                            Para::Text(format!(
                                "{} of {branch} {} already on its upstream. A rebase replaces them with new commits.",
                                plural(published, "commit"),
                                if published == 1 { "is" } else { "are" }
                            )),
                            Para::Text(
                                "After the rebase the branch and its upstream have different histories: a push needs force, and others who use the branch must repair their copies.".into(),
                            ),
                            Para::Muted("The old commits stay in the reflog. You can undo the rebase from the notification.".into()),
                        ],
                        confirm: "Rebase anyway".into(),
                        danger: true,
                    },
                    move |_, cx| {
                        let _ = weak.update(cx, |this, cx| this.rebase_now(branch.clone(), onto.clone(), cx));
                    },
                );
            });
        })
        .detach();
    }

    fn rebase_now(&mut self, branch: String, onto: String, cx: &mut Context<Self>) {
        self.op(Op::Rebase { branch: Some(branch.clone()), onto: onto.clone() }, cx, move |this, res, cx| {
            this.finish(res, format!("Rebased {branch} onto {onto}"), cx)
        });
    }

    fn reset_to(&mut self, oid: String, mode: &'static str, cx: &mut Context<Self>) {
        let branch = self.current_branch().map(|b| b.name.clone()).unwrap_or_else(|| "HEAD".into());
        let dirty = self
            .changes
            .status
            .as_ref()
            .map(|s| s.staged.iter().chain(&s.unstaged).map(|f| f.path.as_str()).collect::<BTreeSet<_>>().len())
            .unwrap_or(0);
        let id = self.id.clone();
        let target = oid.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::rewrite::rebase_info(r, None, &target)).await });
        cx.spawn(async move |this, cx| {
            // Without the count the dialog still describes the reset.
            let lost = task.await.ok().and_then(|v| v["commits"].as_u64()).unwrap_or(0) as usize;
            let _ = this.update(cx, |this, cx| {
                if mode != "hard" && lost == 0 {
                    this.reset_now(oid, mode, branch, cx);
                    return;
                }
                let what = match mode {
                    "soft" => "The changes of the removed commits stay staged.",
                    "mixed" => "The changes of the removed commits stay in the working tree, unstaged.",
                    _ => "The index and the working tree are made equal to this commit.",
                };
                let mut body = vec![Para::Text(what.into())];
                if lost > 0 {
                    body.push(Para::Warn(format!(
                        "{} will no longer be on {branch}. They stay reachable through the reflog, and you can undo the reset from the notification.",
                        plural(lost, "commit")
                    )));
                }
                if mode == "hard" && dirty > 0 {
                    body.push(Para::Warn(format!("Uncommitted changes in {} will be overwritten. They are saved in a safety snapshot first.", plural(dirty, "file"))));
                }
                let weak = cx.entity().downgrade();
                let title = format!("Reset {branch} to {} ({mode})?", &oid[..7.min(oid.len())]);
                overlay::confirm(cx, ConfirmOpts { title, body, confirm: format!("Reset ({mode})"), danger: mode == "hard" }, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.reset_now(oid.clone(), mode, branch.clone(), cx));
                });
            });
        })
        .detach();
    }

    fn reset_now(&mut self, oid: String, mode: &'static str, branch: String, cx: &mut Context<Self>) {
        let short = oid[..7.min(oid.len())].to_string();
        self.history_op(HistoryOp::Reset { to: oid, mode: mode.into() }, cx, move |_, res, cx| {
            let before = res["before"].as_str().unwrap_or("").to_string();
            let snapshot = res["snapshot"]["name"].as_str().map(str::to_string);
            let weak = cx.entity().downgrade();
            overlay::toast_action(cx, format!("Reset {branch} to {short}"), ToastKind::Success, "Undo", move |_, cx| {
                let (before, snapshot) = (before.clone(), snapshot.clone());
                let _ = weak.update(cx, |this, cx| {
                    this.history_op(HistoryOp::Reset { to: before, mode: "hard".into() }, cx, move |this, _, cx| match snapshot {
                        // The reset saved the changed files: bring them back too.
                        Some(name) => {
                            this.mutate(cx, move |r, _| graftline::safety::restore(r, &name), |_, _, cx| overlay::success(cx, "Undone"))
                        }
                        None => overlay::success(cx, "Undone"),
                    });
                });
            });
        });
    }

    /// Extra items for the context menu of a commit.
    pub fn history_items(&self, oid: &str, is_head: bool, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let branch = self.current_branch().map(|b| b.name.clone());
        let short = oid[..7.min(oid.len())].to_string();
        let o = oid.to_string();
        let mut items = vec![
            MenuItem::sep(),
            item(&w, "download", "Cherry-pick onto the current branch", {
                let (o, s) = (o.clone(), short.clone());
                move |this, cx| {
                    let text = format!("Cherry-picked {s}");
                    this.history_op(HistoryOp::CherryPick { oids: vec![o.clone()] }, cx, move |this, res, cx| this.finish(res, text, cx));
                }
            })
            .disabled(is_head),
            item(&w, "undo", "Revert this commit", {
                let (o, s) = (o.clone(), short.clone());
                move |this, cx| {
                    let text = format!("Reverted {s}");
                    this.history_op(HistoryOp::Revert { oid: o.clone() }, cx, move |this, res, cx| this.finish(res, text, cx));
                }
            })
            .sub("Adds a commit that undoes it"),
        ];
        if branch.is_some() {
            items.push(
                item(&w, "edit", "Rebase interactively from here…", {
                    let o = o.clone();
                    move |this, cx| this.interactive_rebase(o.clone(), cx)
                })
                .sub("Reorder, squash, reword or drop the commits after this one")
                .disabled(is_head),
            );
        }
        items.push(MenuItem::sep());
        items.push(MenuItem::title(format!("Reset {} to this commit", branch.as_deref().unwrap_or("HEAD"))));
        items.push(
            item(&w, "left", "Soft", {
                let o = o.clone();
                move |this, cx| this.reset_to(o.clone(), "soft", cx)
            })
            .sub("Keep the changes staged")
            .disabled(is_head),
        );
        items.push(
            item(&w, "left", "Mixed", {
                let o = o.clone();
                move |this, cx| this.reset_to(o.clone(), "mixed", cx)
            })
            .sub("Keep the changes in the working tree")
            .disabled(is_head),
        );
        items.push(
            item(&w, "left", "Hard…", {
                let o = o.clone();
                move |this, cx| this.reset_to(o.clone(), "hard", cx)
            })
            .sub("Discard the changes")
            .danger(),
        );
        items
    }

    // ------------------------------------------------------------ interactive rebase

    fn interactive_rebase(&mut self, base: String, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let b = base.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::rewrite::rebase_plan(r, Some(&b))).await });
        let repo = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = task.await;
            let _ = cx.update(|cx| match result {
                Err(e) => overlay::show_error(cx, &e),
                Ok(plan) if plan.is_empty() => overlay::info(cx, "There are no commits after this one on the current branch"),
                Ok(plan) if plan.iter().any(|c| c.merge) => overlay::error(
                    cx,
                    "The range contains merge commits. An interactive rebase would flatten them, so it is not offered here.",
                ),
                Ok(plan) => overlay::open_modal(cx, move |_, cx| RebaseDialog {
                    repo,
                    base,
                    steps: plan.into_iter().map(|commit| RebaseStep { commit, action: "pick", message: None }).collect(),
                    focus: cx.focus_handle(),
                }),
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ operation in progress

    fn abort_op(&mut self, cx: &mut Context<Self>) {
        let state = self.refs.as_ref().and_then(|r| r.state).unwrap_or("");
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: format!("Abort the {}?", state_name(state).to_lowercase()),
                body: vec![
                    Para::Text(
                        "The repository returns to the state before the operation started. Conflict resolutions made so far are discarded."
                            .into(),
                    ),
                    Para::Muted("The changed files are saved in a safety snapshot first.".into()),
                ],
                confirm: "Abort".into(),
                danger: true,
            },
            move |_, cx| {
                let _ = this.update(cx, |this, cx| this.history_op(HistoryOp::Abort, cx, |_, _, cx| overlay::success(cx, "Aborted")));
            },
        );
    }

    /// The banner of an operation in progress; `None` when the repository is idle.
    pub fn render_banner(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.refs.as_ref()?.state?;
        let conflicted = self.changes.status.as_ref().map(|s| s.conflicted.clone()).unwrap_or_default();
        let can_skip = matches!(state, "rebase" | "cherry-pick" | "revert");
        let text = if conflicted.is_empty() {
            "All conflicts are resolved. Continue to finish.".to_string()
        } else {
            format!("{} with conflicts: resolve them, then continue.", plural(conflicted.len(), "file"))
        };
        let first = conflicted.first().cloned();
        let el = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(40.))
            .bg(t.mod_bg)
            .border_b_1()
            .border_color(t.mod_border)
            .child(icon("alert", 15., t.text))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} in progress.", state_name(state))))
            .child(div().flex_1().min_w_0().child(clip(text)))
            .children(first.map(|path| {
                btn("banner-resolve", "Resolve").on_click(cx.listener(move |this, _, _, cx| this.open_conflict(path.clone(), cx)))
            }))
            .child(
                btn("banner-continue", "Continue")
                    .primary()
                    .disabled(!conflicted.is_empty())
                    .tip(if conflicted.is_empty() { "Finish the operation" } else { "Resolve all conflicts first" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.history_op(HistoryOp::Continue, cx, |this, res, cx| this.finish(res, "Finished".into(), cx))
                    })),
            )
            .children(can_skip.then(|| {
                btn("banner-skip", "Skip").tip("Leave this commit out and go on").on_click(cx.listener(|this, _, _, cx| {
                    this.history_op(HistoryOp::Skip, cx, |this, res, cx| this.finish(res, "Skipped the commit".into(), cx))
                }))
            }))
            .child(btn("banner-abort", "Abort").danger().on_click(cx.listener(|this, _, _, cx| this.abort_op(cx))));
        Some(el.into_any_element())
    }

    // ------------------------------------------------------------ conflicts

    /// Takes one side for the whole file.
    pub fn resolve_side(&mut self, path: String, side: &'static str, cx: &mut Context<Self>) {
        let choice = if side == "ours" { Choice::Ours } else { Choice::Theirs };
        let p = path.clone();
        self.mutate(
            cx,
            move |r, _| graftline::conflicts::resolve(r, &p, choice),
            move |this, _, cx| {
                overlay::success(cx, format!("{path}: took {} version", if side == "ours" { "our" } else { "their" }));
                if matches!(&this.panel, Some(Panel::Conflict(c)) if c.path == path) {
                    this.panel = None;
                }
            },
        );
    }

    pub fn open_conflict(&mut self, path: String, cx: &mut Context<Self>) {
        self.panel = Some(Panel::Conflict(ConflictPanel {
            path: path.clone(),
            detail: None,
            error: None,
            segments: Vec::new(),
            manual: None,
            scroll: gpui::ScrollHandle::new(),
            bar: ScrollbarState::default(),
        }));
        cx.notify();
        let id = self.id.clone();
        let p = path.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::conflicts::detail(r, &p)).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(Panel::Conflict(panel)) = &mut this.panel else { return };
                if panel.path != path {
                    return;
                }
                match result {
                    Ok(d) => {
                        panel.segments = match &d.merged {
                            Some(text) if !d.binary => parse_conflicts(text),
                            _ => Vec::new(),
                        };
                        panel.detail = Some(d);
                    }
                    Err(e) => panel.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn conflict_mut(&mut self) -> Option<&mut ConflictPanel> {
        match &mut self.panel {
            Some(Panel::Conflict(c)) => Some(c),
            _ => None,
        }
    }

    fn save_conflict(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.conflict_mut() else { return };
        let path = panel.path.clone();
        let content = match &panel.manual {
            Some(input) => input.read(cx).text().to_string(),
            None => self.conflict_mut().map(|p| p.result()).unwrap_or_default(),
        };
        if !force && has_markers(&content, false) {
            let this = cx.entity().downgrade();
            overlay::confirm(
                cx,
                ConfirmOpts {
                    title: "Save with conflict markers?".into(),
                    body: vec![Para::Text("The text still contains conflict markers. The file is staged as it is.".into())],
                    confirm: "Save anyway".into(),
                    danger: true,
                },
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.save_conflict(true, cx));
                },
            );
            return;
        }
        let p = path.clone();
        self.mutate(
            cx,
            move |r, _| graftline::conflicts::resolve(r, &p, Choice::Content { content }),
            move |this, _, cx| {
                overlay::success(cx, format!("{path} resolved"));
                this.panel = None;
                // Go on with the next file that has conflicts.
                let next = this.changes.status.as_ref().and_then(|s| s.conflicted.iter().find(|p| **p != path).cloned());
                if let Some(next) = next {
                    this.open_conflict(next, cx);
                }
            },
        );
    }

    fn edit_conflict_by_hand(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.conflict_mut() else { return };
        if panel.manual.is_some() {
            return;
        }
        let text =
            if panel.undecided() > 0 { panel.detail.as_ref().and_then(|d| d.merged.clone()).unwrap_or_default() } else { panel.result() };
        let input = cx.new(|cx| TextInput::new(cx).multiline(12).mono().with_text(text));
        cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        if let Some(panel) = self.conflict_mut() {
            panel.manual = Some(input);
        }
        cx.notify();
    }

    fn back_to_blocks(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.conflict_mut() else { return };
        let Some(input) = panel.manual.clone() else { return };
        let (edited, result) = (input.read(cx).text().to_string(), self.conflict_mut().map(|p| p.result()).unwrap_or_default());
        if edited == result {
            if let Some(p) = self.conflict_mut() {
                p.manual = None;
            }
            cx.notify();
            return;
        }
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: "Go back to the conflict blocks?".into(),
                body: vec![Para::Text("Your manual edits of the result are replaced by the selection in the blocks.".into())],
                confirm: "Discard my edits".into(),
                danger: true,
            },
            move |_, cx| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(p) = this.conflict_mut() {
                        p.manual = None;
                    }
                    cx.notify();
                });
            },
        );
    }

    pub fn render_conflict(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(Panel::Conflict(panel)) = &self.panel else { return div().into_any_element() };
        let path = panel.path.clone();
        let close = icon_btn("conflict-close", "x").tip("Close (Esc)").on_click(cx.listener(|this, _, window, cx| {
            this.panel = None;
            window.focus(&this.focus);
            cx.notify();
        }));
        let title = |kind: String| {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(div().flex_none().font_family(super::MONO_FONT).font_weight(FontWeight::BOLD).text_color(t.danger).child("U"))
                .child(clip(path.clone()))
                .child(div().flex_none().text_color(t.muted).text_size(px(12.)).child(kind))
        };
        let head = |children: Vec<AnyElement>| {
            div().flex().flex_none().items_center().gap_2().h(px(40.)).px_3().border_b_1().border_color(t.border).children(children)
        };
        let Some(d) = panel.detail.clone() else {
            let body = match &panel.error {
                Some(e) => div().p_4().text_color(t.danger).child(e.clone()),
                None => div().p_4().text_color(t.muted).child("Loading…"),
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(head(vec![title(String::new()).into_any_element(), close.into_any_element()]))
                .child(body)
                .into_any_element();
        };
        let total = panel.blocks().count();
        let left = panel.undecided();
        let manual = panel.manual.clone();
        let info = match &manual {
            Some(input) => {
                if has_markers(input.read(cx).text(), true) {
                    "The text still has conflict markers".to_string()
                } else {
                    "Edited by hand".to_string()
                }
            }
            None if total == 0 => String::new(),
            None if left > 0 => format!("{} left to decide", plural(left, "conflict")),
            None => "All conflicts are decided".to_string(),
        };
        let can_save = manual.is_some() || (total > 0 && left == 0);
        let (p1, p2) = (path.clone(), path.clone());
        let mut head_items = vec![
            title(d.kind.replace('-', " ")).into_any_element(),
            div().flex_none().text_color(t.muted).text_size(px(12.)).child(info).into_any_element(),
            btn("cf-ours", if d.kind == "deleted-by-us" { "Keep deleted" } else { "Use ours" })
                .tip(format!("Use our version of the whole file: {}", d.ours_label))
                .on_click(cx.listener(move |this, _, _, cx| this.resolve_side(p1.clone(), "ours", cx)))
                .into_any_element(),
            btn("cf-theirs", if d.kind == "deleted-by-them" { "Delete file" } else { "Use theirs" })
                .tip(format!("Use their version of the whole file: {}", d.theirs_label))
                .on_click(cx.listener(move |this, _, _, cx| this.resolve_side(p2.clone(), "theirs", cx)))
                .into_any_element(),
        ];
        if !d.binary && d.merged.is_some() && manual.is_none() {
            head_items.push(
                btn("cf-edit", "Edit by hand")
                    .icon("edit")
                    .tip("Edit the resulting file as text")
                    .on_click(cx.listener(|this, _, _, cx| this.edit_conflict_by_hand(cx)))
                    .into_any_element(),
            );
        }
        head_items.push(
            btn("cf-save", "Mark resolved")
                .icon("check")
                .primary()
                .disabled(!can_save)
                .on_click(cx.listener(|this, _, _, cx| this.save_conflict(false, cx)))
                .into_any_element(),
        );
        head_items.push(close.into_any_element());

        let pre = |lines: Vec<String>, t: &Theme| {
            div()
                .px_3()
                .py_1()
                .font_family(super::MONO_FONT)
                .text_size(px(12.5))
                .text_color(t.muted)
                .children(lines.into_iter().map(|l| div().min_h(px(18.)).whitespace_nowrap().child(SharedString::from(l))))
        };
        let mut body = div().flex().flex_col();
        if let Some(input) = &manual {
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child("You edit the result by hand.")
                        .children((total > 0).then(|| {
                            div()
                                .id("cf-back")
                                .text_color(t.accent)
                                .cursor_pointer()
                                .child("Back to the conflict blocks")
                                .on_click(cx.listener(|this, _, _, cx| this.back_to_blocks(cx)))
                        })),
                )
                .child(div().px_3().pb_3().child(field(input, window, cx)));
        } else if total == 0 {
            let note = if d.binary {
                "This file is binary or not UTF-8 text. Choose one side for the whole file."
            } else if d.kind == "deleted-by-us" {
                "We deleted this file; they changed it. Keep their file or keep it deleted."
            } else if d.kind == "deleted-by-them" {
                "They deleted this file; we changed it. Keep our file or delete it."
            } else {
                "The file has no conflict markers. Choose one side, or edit the result by hand."
            };
            body = body.child(div().p_4().text_color(t.muted).child(note));
        } else {
            let mut n = 0;
            for (si, seg) in panel.segments.iter().enumerate() {
                match seg {
                    Segment::Text(lines) => {
                        let shown = if lines.len() > 8 {
                            let mut v = lines[..3].to_vec();
                            v.push(format!("… {} unchanged lines …", lines.len() - 6));
                            v.extend_from_slice(&lines[lines.len() - 3..]);
                            v
                        } else {
                            lines.clone()
                        };
                        body = body.child(pre(shown, t));
                    }
                    Segment::Conflict(b) => {
                        n += 1;
                        let set = |ours: bool, theirs: bool| {
                            cx.listener(move |this: &mut RepoView, _: &gpui::ClickEvent, _, cx| {
                                if let Some(Segment::Conflict(b)) = this.conflict_mut().and_then(|p| p.segments.get_mut(si)) {
                                    b.set_all(ours, theirs);
                                }
                                cx.notify();
                            })
                        };
                        let side = |ours: bool, label: &str, cx: &mut Context<Self>| {
                            let (lines, picked, accent) =
                                if ours { (&b.ours, &b.pick_ours, t.graph[4]) } else { (&b.theirs, &b.pick_theirs, t.graph[1]) };
                            let mut col =
                                div().flex_1().min_w_0().border_1().border_color(t.border).rounded(px(6.)).overflow_hidden().child(
                                    div()
                                        .flex()
                                        .gap_1()
                                        .px_2()
                                        .py_1()
                                        .bg(t.panel2)
                                        .text_size(px(12.))
                                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(accent).child(if ours {
                                            "Ours"
                                        } else {
                                            "Theirs"
                                        }))
                                        .child(div().min_w_0().text_color(t.muted).child(clip(format!("· {label}")))),
                                );
                            if lines.is_empty() {
                                col = col.child(div().px_2().py_1().text_color(t.dim).text_size(px(12.)).child("(no lines on this side)"));
                            }
                            for (li, line) in lines.iter().enumerate() {
                                let on = picked.contains(&li);
                                col = col.child(
                                    div()
                                        .id((if ours { "cf-o" } else { "cf-t" }, si * 100_000 + li))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_2()
                                        .min_h(px(20.))
                                        .cursor_pointer()
                                        .when(on, |el| el.bg(t.accent_soft))
                                        .hover(|s| s.bg(t.hover))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_none()
                                                .items_center()
                                                .justify_center()
                                                .size(px(13.))
                                                .rounded(px(3.))
                                                .border_1()
                                                .border_color(if on { t.accent } else { t.border_strong })
                                                .when(on, |el| el.bg(t.accent))
                                                .children(on.then(|| icon("check", 9., t.accent_text))),
                                        )
                                        .child(div().min_w_0().font_family(super::MONO_FONT).text_size(px(12.5)).child(clip(line.clone())))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(Segment::Conflict(b)) = this.conflict_mut().and_then(|p| p.segments.get_mut(si)) {
                                                let picked = if ours { &mut b.pick_ours } else { &mut b.pick_theirs };
                                                if !picked.remove(&li) {
                                                    picked.insert(li);
                                                }
                                                b.decided = true;
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                            col
                        };
                        let out: Vec<String> = b.lines().into_iter().map(str::to_string).collect();
                        let block = div()
                            .m_2()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if b.decided { t.border } else { t.mod_border })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Conflict {n} of {total}")))
                                    .child(div().flex_1())
                                    .child(btn(("cf-take-o", si), "Take ours").on_click(set(true, false)))
                                    .child(btn(("cf-take-t", si), "Take theirs").on_click(set(false, true)))
                                    .child(btn(("cf-take-b", si), "Take both").on_click(set(true, true)))
                                    .child(
                                        btn(("cf-order", si), if b.theirs_first { "Theirs first" } else { "Ours first" })
                                            .ghost()
                                            .tip("Put their lines before ours in the result")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(Segment::Conflict(b)) = this.conflict_mut().and_then(|p| p.segments.get_mut(si))
                                                {
                                                    b.theirs_first = !b.theirs_first;
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(div().flex().gap_2().child(side(true, &d.ours_label, cx)).child(side(false, &d.theirs_label, cx)))
                            .child(
                                div()
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(t.border)
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .px_2()
                                            .py_1()
                                            .text_size(px(12.))
                                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Result"))
                                            .children((!b.decided).then(|| div().text_color(t.muted).child("· choose lines above"))),
                                    )
                                    .children(b.decided.then(|| {
                                        pre(if out.is_empty() { vec!["(both sides removed)".into()] } else { out }, t).text_color(t.text)
                                    })),
                            );
                        body = body.child(block);
                    }
                }
            }
        }
        let (scroll, bar) = (panel.scroll.clone(), panel.bar.clone());
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head(head_items))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(div().id("cf-scroll").size_full().overflow_y_scroll().track_scroll(&scroll).child(body))
                    .child(scrollbar(scroll, bar, t)),
            )
            .into_any_element()
    }
}

// ---------------------------------------------------------------- interactive rebase dialog

struct RebaseStep {
    commit: PlanCommit,
    action: &'static str,
    /// The editor of the new message, for `reword`.
    message: Option<Entity<TextInput>>,
}

struct RebaseDialog {
    repo: WeakEntity<RepoView>,
    base: String,
    steps: Vec<RebaseStep>,
    focus: FocusHandle,
}

impl Focusable for RebaseDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const ACTIONS: [&str; 5] = ["pick", "reword", "squash", "fixup", "drop"];

/// Why the list cannot be applied, if it cannot. `actions`: the action of each step and
/// whether a reworded step has a message.
pub fn rebase_problem(actions: &[(&str, bool)]) -> Option<&'static str> {
    match actions.iter().find(|(a, _)| *a != "drop") {
        None => return Some("Every commit is dropped. To remove them all, reset the branch instead."),
        Some((first, _)) if *first == "squash" || *first == "fixup" => {
            return Some("The first commit cannot be melded: there is no commit before it.")
        }
        _ => {}
    }
    actions.iter().any(|(a, has_message)| *a == "reword" && !has_message).then_some("Enter a message for each reworded commit.")
}

impl RebaseDialog {
    fn problem(&self, cx: &App) -> Option<&'static str> {
        let actions: Vec<(&str, bool)> =
            self.steps.iter().map(|s| (s.action, s.message.as_ref().is_some_and(|m| !m.read(cx).text().trim().is_empty()))).collect();
        rebase_problem(&actions)
    }

    fn set_action(&mut self, i: usize, action: &'static str, cx: &mut Context<Self>) {
        let Some(step) = self.steps.get_mut(i) else { return };
        step.action = action;
        if action == "reword" && step.message.is_none() {
            let text = step.commit.message.clone();
            let input = cx.new(|cx| TextInput::new(cx).multiline(3).with_text(text));
            cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()).detach();
            self.steps[i].message = Some(input);
        }
        cx.notify();
    }

    fn start(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() {
            return;
        }
        let published = self.steps.iter().filter(|s| s.commit.published).count();
        if published > 0 && !confirmed {
            let this = cx.entity().downgrade();
            overlay::confirm(
                cx,
                ConfirmOpts {
                    title: format!("Rewrite {}?", plural(published, "published commit")),
                    body: vec![
                        Para::Text(format!(
                            "{} in this list {} already on the upstream branch. The rebase replaces them with new commits; a push then needs force.",
                            plural(published, "commit"),
                            if published == 1 { "is" } else { "are" }
                        )),
                        Para::Muted("You can undo the rebase from the notification.".into()),
                    ],
                    confirm: "Rewrite anyway".into(),
                    danger: true,
                },
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.start(true, cx));
                },
            );
            return;
        }
        let steps: Vec<Step> = self
            .steps
            .iter()
            .map(|s| Step {
                oid: s.commit.oid.clone(),
                action: s.action.to_string(),
                message: s.message.as_ref().filter(|_| s.action == "reword").map(|m| m.read(cx).text().to_string()),
            })
            .collect();
        let op = HistoryOp::RebaseInteractive { base: Some(self.base.clone()), steps };
        close_modal(cx);
        let _ = self.repo.update(cx, |repo, cx| repo.history_op(op, cx, |this, res, cx| this.finish(res, "Rebase finished".into(), cx)));
    }
}

impl Render for RebaseDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let published = self.steps.iter().filter(|s| s.commit.published).count();
        let problem = self.problem(cx);
        let n = self.steps.len();
        let mut list = div().flex().flex_col().gap_1();
        for (i, s) in self.steps.iter().enumerate() {
            let dropped = s.action == "drop";
            let summary = match (&s.message, s.action) {
                (Some(m), "reword") => m
                    .read(cx)
                    .text()
                    .lines()
                    .next()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| s.commit.summary.clone()),
                _ => s.commit.summary.clone(),
            };
            let this = cx.entity().downgrade();
            let current = s.action;
            let row = div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(32.))
                .when(dropped, |el| el.opacity(0.5))
                .child(icon_btn(("rb-up", i), "up").icon_size(13.).tip("Move up (earlier)").disabled(i == 0).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.steps.swap(i, i - 1);
                        cx.notify();
                    },
                )))
                .child(icon_btn(("rb-down", i), "down").icon_size(13.).tip("Move down (later)").disabled(i + 1 == n).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.steps.swap(i, i + 1);
                        cx.notify();
                    },
                )))
                .child(
                    div()
                        .id(("rb-action", i))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_between()
                        .w(px(84.))
                        .h(px(24.))
                        .px_2()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(t.border_strong)
                        .bg(t.panel)
                        .cursor_pointer()
                        .text_color(match s.action {
                            "drop" => t.danger,
                            "pick" => t.text,
                            _ => t.accent,
                        })
                        .child(s.action)
                        .child(icon("chevron", 12., t.muted))
                        .on_click(move |e, _, cx| {
                            let items = ACTIONS
                                .iter()
                                .map(|a| {
                                    let (this, a) = (this.clone(), *a);
                                    MenuItem::new(if a == current { "check" } else { "minus" }, a, move |_, cx| {
                                        let _ = this.update(cx, |this, cx| this.set_action(i, a, cx));
                                    })
                                    .active(a == current)
                                })
                                .collect();
                            overlay::open_menu(cx, e.position(), items);
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(super::MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .child(s.commit.oid[..7.min(s.commit.oid.len())].to_string()),
                )
                .child(
                    div()
                        .id(("rb-sum", i))
                        .flex_1()
                        .min_w_0()
                        .when(dropped, |el| el.line_through())
                        .tooltip(tip(s.commit.message.clone()))
                        .child(clip(summary)),
                )
                .children(s.commit.published.then(|| badge("published", t.text, t.mod_bg)))
                .child(div().flex_none().max_w(px(140.)).text_size(px(12.)).text_color(t.muted).child(clip(s.commit.author.clone())));
            let mut item = div().rounded(px(6.)).border_1().border_color(t.border).child(row);
            if let Some(input) = s.message.as_ref().filter(|_| s.action == "reword") {
                item = item.child(div().px_2().pb_2().child(field(input, window, cx)));
            }
            list = list.child(item);
        }
        let mut body = div().flex().flex_col().gap_3().child(div().text_size(px(12.)).text_color(t.muted).child(
            "The list is applied from top to bottom, oldest commit first. squash melds a commit into the one above and keeps both messages; fixup melds it and drops its message. To write one message for a melded group, set its first commit to reword.",
        ));
        if published > 0 {
            body = body.child(
                div()
                    .flex()
                    .gap_2()
                    .p_2()
                    .rounded(px(6.))
                    .bg(t.mod_bg)
                    .border_1()
                    .border_color(t.mod_border)
                    .child(icon("alert", 13., t.text))
                    .child(format!(
                        "{} here {} already published.",
                        plural(published, "commit"),
                        if published == 1 { "is" } else { "are" }
                    )),
            );
        }
        body = body.child(list).children(problem.map(|p| div().text_color(t.danger).child(p)));
        let cancel = btn("rb-cancel", "Cancel").on_click(|_, _, cx| close_modal(cx));
        let start = btn("rb-start", "Start rebase")
            .primary()
            .disabled(problem.is_some())
            .on_click(cx.listener(|this, _, _, cx| this.start(false, cx)));
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame(
            format!("Rebase {} interactively", plural(n, "commit")),
            760.,
            body,
            vec![cancel.into_any_element(), start.into_any_element()],
            cx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_markers_are_split_into_blocks() {
        let text = "a\n<<<<<<< HEAD\nours 1\nours 2\n||||||| base\nold\n=======\ntheirs\n>>>>>>> topic\nb\n";
        let segs = parse_conflicts(text);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], Segment::Text(vec!["a".into()]));
        let Segment::Conflict(b) = &segs[1] else { panic!("a conflict block") };
        assert_eq!(b.ours, vec!["ours 1", "ours 2"]);
        assert_eq!(b.base, vec!["old"]);
        assert_eq!(b.theirs, vec!["theirs"]);
        assert_eq!(segs[2], Segment::Text(vec!["b".into(), "".into()]));
    }

    #[test]
    fn unfinished_block_is_text() {
        let segs = parse_conflicts("x\n<<<<<<< HEAD\nours\n");
        assert_eq!(segs, vec![Segment::Text(vec!["x".into()]), Segment::Text(vec!["<<<<<<<".into(), "ours".into(), "".into()])]);
    }

    #[test]
    fn result_follows_the_choices() {
        let mut panel = ConflictPanel {
            path: "f".into(),
            detail: None,
            error: None,
            segments: parse_conflicts("top\n<<<<<<< a\no1\no2\n=======\nt1\n>>>>>>> b\nend"),
            manual: None,
            scroll: gpui::ScrollHandle::new(),
            bar: ScrollbarState::default(),
        };
        assert_eq!(panel.undecided(), 1);
        if let Segment::Conflict(b) = &mut panel.segments[1] {
            b.set_all(true, true);
            b.pick_ours.remove(&0);
        }
        assert_eq!(panel.undecided(), 0);
        assert_eq!(panel.result(), "top\no2\nt1\nend");
        if let Segment::Conflict(b) = &mut panel.segments[1] {
            b.theirs_first = true;
        }
        assert_eq!(panel.result(), "top\nt1\no2\nend");
    }

    #[test]
    fn rebase_list_is_validated() {
        assert_eq!(rebase_problem(&[("pick", false), ("squash", false)]), None);
        assert!(rebase_problem(&[("drop", false), ("drop", false)]).unwrap().contains("dropped"));
        assert!(rebase_problem(&[("drop", false), ("fixup", false)]).unwrap().contains("melded"));
        assert!(rebase_problem(&[("pick", false), ("reword", false)]).unwrap().contains("message"));
        assert_eq!(rebase_problem(&[("reword", true)]), None);
    }
}
