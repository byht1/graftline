//! Repository actions: toolbar buttons, context menus, dialogs, network jobs, clone, and
//! dropping one branch onto another.

use super::changes::plural;
use super::graph_view::DraggedRef;
use super::input::{field, InputEvent, TextInput};
use super::overlay::{self, close_modal, modal_frame, ConfirmOpts, Field, FormOpts, MenuItem, Para, ToastKind, Values};
use super::repo_view::RepoView;
use super::theme::{theme, Theme};
use super::widgets::{btn, clip, icon, icon_btn, tip};
use super::{call, core, pref, set_pref};
use gpui::prelude::*;
use gpui::{div, px, App, Entity, FocusHandle, Focusable, Global, PathPromptOptions, Pixels, Point, SharedString, WeakEntity, Window};
use graftline::history::Kind;
use graftline::ops::{self, NetJob, Op};
use graftline::repo::{Branch, Remote};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

#[derive(Clone)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub command: String,
    pub line: String,
    pub percent: Option<u8>,
}

type Waiter = Box<dyn FnOnce(&mut RepoView, bool, &mut Context<RepoView>)>;

#[derive(Default)]
pub struct JobState {
    pub running: Vec<Job>,
    /// Jobs that report success silently (automatic fetch).
    quiet: HashSet<String>,
    waiters: HashMap<String, Waiter>,
}

fn job_of(v: &Value) -> Job {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    Job { id: s("id"), title: s("title"), command: s("command"), line: s("line"), percent: v["percent"].as_u64().map(|p| p as u8) }
}

/// A local operation in the core. The session is refreshed after it, so the UI follows.
fn op_call(cx: &App, id: String, op: Op) -> impl Future<Output = anyhow::Result<Value>> + 'static {
    call(cx, move |st| async move { st.mutate(&id, move |r, s| ops::run(r, &s.root, op)).await })
}

/// A menu item that runs a method of the repository view.
pub fn item(
    this: &WeakEntity<RepoView>,
    icon: &'static str,
    label: impl Into<SharedString>,
    f: impl Fn(&mut RepoView, &mut Context<RepoView>) + 'static,
) -> MenuItem {
    let this = this.clone();
    MenuItem::new(icon, label, move |_, cx| {
        let _ = this.update(cx, |this, cx| f(this, cx));
    })
}

fn stash_index(name: &str) -> usize {
    name.split_once('{').and_then(|(_, rest)| rest.split('}').next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

impl RepoView {
    pub fn current_branch(&self) -> Option<&Branch> {
        self.refs.as_ref()?.branches.iter().find(|b| b.current)
    }

    fn remotes(&self) -> &[Remote] {
        self.refs.as_ref().map(|r| r.remotes.as_slice()).unwrap_or(&[])
    }

    /// Splits `<remote>/<branch>`. The longest remote name that is a prefix wins:
    /// "origin/team" before "origin".
    fn remote_of(&self, upstream: &str) -> Option<(String, String)> {
        let mut names: Vec<&str> = self.remotes().iter().map(|r| r.name.as_str()).collect();
        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
        let remote = names.into_iter().find(|n| upstream.starts_with(&format!("{n}/")))?;
        Some((remote.to_string(), upstream[remote.len() + 1..].to_string()))
    }

    /// Runs a local operation; `done` gets its answer. An error is shown.
    pub fn op(&mut self, op: Op, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static) {
        self.mutate(cx, move |r, s| ops::run(r, &s.root, op), done);
    }

    /// A form whose values become one operation.
    fn op_form(&self, cx: &mut Context<Self>, opts: FormOpts, build: impl Fn(&Values) -> Op + 'static) {
        let id = self.id.clone();
        overlay::form(cx, opts, move |v, _, cx| {
            let fut = op_call(cx, id.clone(), build(&v));
            cx.spawn(async move |_| fut.await.map(|_| ()))
        });
    }

    // ------------------------------------------------------------ network jobs

    /// Starts a network job. `done(ok)` runs when it ends.
    pub fn net(&mut self, job: NetJob, quiet: bool, cx: &mut Context<Self>, done: Option<Waiter>) {
        if core(cx).git.is_none() {
            overlay::error(cx, "Git was not found on PATH. Install Git to use the network.");
            return;
        }
        let id = self.id.clone();
        let task = call(cx, move |st| async move { st.start_job(&id, job).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(info) => {
                    if info.state == "running" && !this.jobs.running.iter().any(|j| j.id == info.id) {
                        this.jobs.running.push(Job {
                            id: info.id.clone(),
                            title: info.title,
                            command: info.command,
                            line: info.line,
                            percent: info.percent,
                        });
                    }
                    if quiet {
                        this.jobs.quiet.insert(info.id.clone());
                    }
                    if let Some(done) = done {
                        this.jobs.waiters.insert(info.id, done);
                    }
                    cx.notify();
                }
                Err(e) => overlay::show_error(cx, &e),
            });
        })
        .detach();
    }

    /// Called for every `job` event of this repository.
    pub fn on_job(&mut self, v: &Value, cx: &mut Context<Self>) {
        let job = job_of(v);
        let state = v["state"].as_str().unwrap_or("");
        if state == "running" {
            match self.jobs.running.iter_mut().find(|j| j.id == job.id) {
                Some(j) => *j = job,
                None => self.jobs.running.push(job),
            }
        } else {
            self.jobs.running.retain(|j| j.id != job.id);
            let quiet = self.jobs.quiet.remove(&job.id);
            match state {
                "done" => {
                    if !quiet {
                        overlay::success(cx, format!("{}: done", job.title));
                    }
                }
                "cancelled" => overlay::info(cx, format!("{}: cancelled", job.title)),
                _ => {
                    let output = v["output"].as_str().filter(|o| !o.is_empty()).unwrap_or(&job.line).to_string();
                    let e = anyhow::Error::new(graftline::gitcli::GitError { command: job.command.clone(), code: None, output });
                    overlay::show_error_titled(cx, &e, &format!("{} failed", job.title));
                }
            }
            if let Some(done) = self.jobs.waiters.remove(&job.id) {
                done(self, state == "done", cx);
            }
        }
        cx.notify();
    }

    pub fn fetch_all(&mut self, prune: bool, quiet: bool, cx: &mut Context<Self>) {
        self.last_fetch = Some(std::time::Instant::now());
        self.net(NetJob::Fetch { remote: None, prune }, quiet, cx, None);
    }

    pub fn pull(&mut self, mode: Option<&str>, cx: &mut Context<Self>) {
        self.net(NetJob::Pull { mode: mode.map(str::to_string) }, false, cx, None);
    }

    /// Fetches automatically, as set in the settings, while the repository is open.
    pub fn start_auto_fetch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(30)).await;
            let alive = this.update(cx, |this, cx| {
                let minutes = super::tools::settings(cx).auto_fetch;
                if minutes == 0 || core(cx).git.is_none() || this.remotes().is_empty() || !this.jobs.running.is_empty() {
                    return;
                }
                let since = this.last_fetch.unwrap_or(this.opened_at);
                if since.elapsed() >= Duration::from_secs(minutes * 60) {
                    this.fetch_all(false, true, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
    }

    pub fn add_remote(&mut self, cx: &mut Context<Self>) {
        let name = if self.remotes().is_empty() { "origin" } else { "" };
        self.op_form(
            cx,
            FormOpts {
                title: "Add a remote".into(),
                intro: Vec::new(),
                fields: vec![
                    Field::text("name", "Name", name),
                    Field::text("url", "URL", "").placeholder("https://example.com/team/project.git or git@example.com:team/project.git"),
                ],
                confirm: "Add".into(),
                danger: false,
            },
            |v| Op::RemoteAdd { name: v.trimmed("name"), url: v.trimmed("url") },
        );
    }

    pub fn push(&mut self, force: bool, cx: &mut Context<Self>) {
        self.push_branch(None, force, cx);
    }

    pub fn push_branch(&mut self, branch: Option<String>, force: bool, cx: &mut Context<Self>) {
        let found = match &branch {
            Some(name) => self.refs.as_ref().and_then(|r| r.branches.iter().find(|b| &b.name == name)),
            None => self.current_branch(),
        };
        let Some(b) = found.cloned() else {
            overlay::error(cx, "Check out a branch to push it");
            return;
        };
        if self.remotes().is_empty() {
            overlay::info(cx, "This repository has no remote yet. Add one, then push again.");
            self.add_remote(cx);
            return;
        }
        let upstream = b.upstream.as_deref().filter(|_| !b.gone).and_then(|u| self.remote_of(u));
        match upstream {
            Some((remote, remote_name)) => self.push_to(b, remote, remote_name, false, force, cx),
            None => {
                // The branch has no upstream yet: ask where to push.
                let remotes: Vec<(String, String)> = self.remotes().iter().map(|r| (r.name.clone(), r.name.clone())).collect();
                let default = remotes.iter().find(|(n, _)| n == "origin").unwrap_or(&remotes[0]).0.clone();
                let this = cx.entity().downgrade();
                let name = b.name.clone();
                overlay::form(
                    cx,
                    FormOpts {
                        title: format!("Push {}", b.name),
                        intro: vec![Para::Muted("This branch has no upstream yet.".into())],
                        fields: vec![
                            Field::select("remote", "Remote", remotes, default),
                            Field::text("name", "Branch name on the remote", name.clone()),
                            Field::check("track", "Set as upstream of this branch", true),
                        ],
                        confirm: "Push".into(),
                        danger: false,
                    },
                    move |v, _, cx| {
                        let remote_name = Some(v.trimmed("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| name.clone());
                        let (b, remote, track) = (b.clone(), v.text("remote"), v.flag("track"));
                        let _ = this.update(cx, |this, cx| this.push_to(b, remote, remote_name, track, force, cx));
                        gpui::Task::ready(Ok(()))
                    },
                );
            }
        }
    }

    fn push_to(&mut self, b: Branch, remote: String, remote_name: String, set_upstream: bool, force: bool, cx: &mut Context<Self>) {
        let job = NetJob::Push {
            remote: remote.clone(),
            refname: b.name.clone(),
            remote_name: Some(remote_name.clone()),
            set_upstream,
            force,
            tags: false,
        };
        if !force {
            self.net(job, false, cx, None);
            return;
        }
        let mut body = vec![Para::Text(format!("The branch {remote}/{remote_name} is replaced with your local {}.", b.name))];
        if b.behind > 0 {
            body.push(Para::Warn(format!(
                "{} on the remote that your branch does not have will be removed from it.",
                plural(b.behind, "commit")
            )));
        }
        body.push(Para::Muted(
            "The push uses --force-with-lease: it stops if the remote has commits that you have not fetched yet. Other people who pulled the old commits must repair their branches.".into(),
        ));
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts { title: format!("Force push {}?", b.name), body, confirm: "Force push".into(), danger: true },
            move |_, cx| {
                let _ = this.update(cx, |this, cx| this.net(job.clone(), false, cx, None));
            },
        );
    }

    // ------------------------------------------------------------ branches

    pub fn create_branch(&mut self, at: Option<String>, cx: &mut Context<Self>) {
        self.create_branch_at(at, None, cx);
    }

    pub fn create_branch_at(&mut self, at: Option<String>, at_label: Option<String>, cx: &mut Context<Self>) {
        let intro = at_label.map(|l| format!("At {l}")).unwrap_or_else(|| "At the current commit (HEAD)".into());
        self.op_form(
            cx,
            FormOpts {
                title: "Create a branch".into(),
                intro: vec![Para::Muted(intro)],
                fields: vec![
                    Field::text("name", "Branch name", "").placeholder("feature/my-change"),
                    Field::check("checkout", "Check out the new branch", true),
                ],
                confirm: "Create".into(),
                danger: false,
            },
            move |v| Op::BranchCreate { name: v.trimmed("name"), at: at.clone(), checkout: v.flag("checkout") },
        );
    }

    fn rename_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.op_form(
            cx,
            FormOpts {
                title: format!("Rename {name}"),
                intro: Vec::new(),
                fields: vec![Field::text("to", "New name", name.clone())],
                confirm: "Rename".into(),
                danger: false,
            },
            move |v| Op::BranchRename { name: name.clone(), to: v.trimmed("to") },
        );
    }

    fn branch_deleted(&mut self, name: String, res: Value, cx: &mut Context<Self>) {
        let oid = res["oid"].as_str().unwrap_or("").to_string();
        let this = cx.entity().downgrade();
        overlay::toast_action(cx, format!("Deleted branch {name}"), ToastKind::Success, "Undo", move |_, cx| {
            let op = Op::BranchRestore { name: name.clone(), oid: oid.clone() };
            let _ = this.update(cx, |this, cx| this.op(op, cx, |_, _, _| {}));
        });
    }

    fn delete_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.op(Op::BranchDelete { name: name.clone(), force: false }, cx, move |this, res, cx| {
            if res["needsForce"] != true {
                this.branch_deleted(name, res, cx);
                return;
            }
            // Commits that exist only on this branch: ask before they become unreachable.
            let commits = res["commits"].as_array().cloned().unwrap_or_default();
            let mut list: Vec<String> = commits
                .iter()
                .take(10)
                .map(|c| {
                    format!(
                        "{}  {}",
                        c["oid"].as_str().unwrap_or("").chars().take(7).collect::<String>(),
                        c["summary"].as_str().unwrap_or("")
                    )
                })
                .collect();
            if commits.len() > 10 {
                list.push(format!("… and {} more", commits.len() - 10));
            }
            let weak = cx.entity().downgrade();
            overlay::confirm(
                cx,
                ConfirmOpts {
                    title: format!("Delete {name}?"),
                    body: vec![
                        Para::Warn(format!(
                            "{} exist only on this branch. After the delete no branch or tag leads to them:",
                            plural(commits.len(), "commit")
                        )),
                        Para::Code(list.join("\n")),
                        Para::Muted(
                            "Git keeps unreachable commits for a limited time (the reflog). You can undo the delete from the notification."
                                .into(),
                        ),
                    ],
                    confirm: "Delete branch".into(),
                    danger: true,
                },
                move |_, cx| {
                    let name = name.clone();
                    let _ = weak.update(cx, |this, cx| {
                        this.op(Op::BranchDelete { name: name.clone(), force: true }, cx, move |this, res, cx| {
                            this.branch_deleted(name, res, cx)
                        });
                    });
                },
            );
        });
    }

    /// `kind`: `branch`, `remote`, `tag` or `commit`.
    pub fn checkout(&mut self, target: String, kind: &'static str, cx: &mut Context<Self>) {
        self.op(Op::Checkout { target, kind: kind.to_string() }, cx, move |_, _, cx| {
            if kind == "commit" {
                overlay::info(cx, "HEAD is detached. Create a branch to keep new commits.");
            }
        });
    }

    fn delete_remote_branch(&mut self, full: String, cx: &mut Context<Self>) {
        let Some((remote, branch)) = self.remote_of(&full) else { return };
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: format!("Delete {full} on the remote?"),
                body: vec![
                    Para::Text(format!("The branch {branch} is deleted on {remote} for everyone who uses that remote.")),
                    Para::Muted(
                        "Your local branches are not changed. To bring the branch back, push it again from a repository that still has it."
                            .into(),
                    ),
                ],
                confirm: "Delete on remote".into(),
                danger: true,
            },
            move |_, cx| {
                let job = NetJob::PushDelete { remote: remote.clone(), refname: branch.clone() };
                let _ = this.update(cx, |this, cx| this.net(job, false, cx, None));
            },
        );
    }

    // ------------------------------------------------------------ tags

    pub fn create_tag(&mut self, at: Option<String>, at_label: Option<String>, cx: &mut Context<Self>) {
        let intro = at_label.map(|l| format!("At {l}")).unwrap_or_else(|| "At the current commit (HEAD)".into());
        self.op_form(
            cx,
            FormOpts {
                title: "Create a tag".into(),
                intro: vec![Para::Muted(intro)],
                fields: vec![
                    Field::text("name", "Tag name", "").placeholder("v1.0.0"),
                    Field::area("message", "Message (optional)", "")
                        .hint("With a message the tag is annotated: it records who created it and when."),
                ],
                confirm: "Create".into(),
                danger: false,
            },
            move |v| Op::TagCreate {
                name: v.trimmed("name"),
                at: at.clone(),
                message: Some(v.text("message")).filter(|m| !m.trim().is_empty()),
            },
        );
    }

    fn delete_tag(&mut self, name: String, cx: &mut Context<Self>) {
        self.op(Op::TagDelete { name: name.clone() }, cx, move |_, res, cx| {
            let text = format!("Deleted tag {name}");
            match res["oid"].as_str().map(str::to_string) {
                Some(oid) => {
                    let this = cx.entity().downgrade();
                    overlay::toast_action(cx, text, ToastKind::Success, "Undo", move |_, cx| {
                        let op = Op::TagRestore { name: name.clone(), oid: oid.clone() };
                        let _ = this.update(cx, |this, cx| this.op(op, cx, |_, _, _| {}));
                    });
                }
                None => overlay::success(cx, text),
            }
        });
    }

    fn push_tag(&mut self, name: String, remote: String, remove: bool, cx: &mut Context<Self>) {
        let refname = format!("refs/tags/{name}");
        if !remove {
            self.net(NetJob::Push { remote, refname, remote_name: None, set_upstream: false, force: false, tags: false }, false, cx, None);
            return;
        }
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: format!("Delete tag {name} on {remote}?"),
                body: vec![Para::Text(format!("The tag is deleted on {remote} for everyone. Your local tag is not changed."))],
                confirm: "Delete on remote".into(),
                danger: true,
            },
            move |_, cx| {
                let job = NetJob::PushDelete { remote: remote.clone(), refname: refname.clone() };
                let _ = this.update(cx, |this, cx| this.net(job, false, cx, None));
            },
        );
    }

    // ------------------------------------------------------------ stashes

    pub fn stash_save(&mut self, cx: &mut Context<Self>) {
        self.op_form(
            cx,
            FormOpts {
                title: "Stash changes".into(),
                intro: vec![Para::Muted(
                    "Saves your uncommitted changes and cleans the working tree. Apply or pop the stash to get them back.".into(),
                )],
                fields: vec![
                    Field::text("message", "Message (optional)", ""),
                    Field::check("untracked", "Include untracked files", true),
                    Field::check("keepIndex", "Keep staged changes in the working tree", false),
                ],
                confirm: "Stash".into(),
                danger: false,
            },
            |v| Op::StashSave {
                message: Some(v.text("message")).filter(|m| !m.trim().is_empty()),
                untracked: v.flag("untracked"),
                keep_index: v.flag("keepIndex"),
            },
        );
    }

    fn stash_apply(&mut self, index: usize, pop: bool, cx: &mut Context<Self>) {
        let op = if pop { Op::StashPop { index } } else { Op::StashApply { index } };
        self.op(op, cx, move |_, _, cx| overlay::success(cx, if pop { "Stash applied and removed" } else { "Stash applied" }));
    }

    fn stash_drop(&mut self, index: usize, message: String, cx: &mut Context<Self>) {
        let shown = if message.is_empty() { format!("stash@{{{index}}}") } else { message };
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts {
                title: "Drop this stash?".into(),
                body: vec![
                    Para::Text("The stashed changes are removed from the stash list:".into()),
                    Para::Code(shown),
                    Para::Muted("You can undo the drop from the notification. After that the changes are hard to find.".into()),
                ],
                confirm: "Drop stash".into(),
                danger: true,
            },
            move |_, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.op(Op::StashDrop { index }, cx, |_, res, cx| {
                        let (oid, message) =
                            (res["oid"].as_str().unwrap_or("").to_string(), res["message"].as_str().unwrap_or("").to_string());
                        let weak = cx.entity().downgrade();
                        overlay::toast_action(cx, "Stash dropped", ToastKind::Success, "Undo", move |_, cx| {
                            let op = Op::StashStore { oid: oid.clone(), message: message.clone() };
                            let _ = weak.update(cx, |this, cx| this.op(op, cx, |_, _, _| {}));
                        });
                    });
                });
            },
        );
    }

    // ------------------------------------------------------------ remotes

    fn edit_remote(&mut self, r: &Remote, cx: &mut Context<Self>) {
        let name = r.name.clone();
        self.op_form(
            cx,
            FormOpts {
                title: format!("Remote {}", r.name),
                intro: Vec::new(),
                fields: vec![Field::text("url", "URL", r.url.clone().unwrap_or_default())],
                confirm: "Save".into(),
                danger: false,
            },
            move |v| Op::RemoteSetUrl { name: name.clone(), url: v.trimmed("url") },
        );
    }

    fn rename_remote(&mut self, r: &Remote, cx: &mut Context<Self>) {
        let name = r.name.clone();
        self.op_form(
            cx,
            FormOpts {
                title: format!("Rename remote {}", r.name),
                intro: Vec::new(),
                fields: vec![Field::text("to", "New name", r.name.clone())],
                confirm: "Rename".into(),
                danger: false,
            },
            move |v| Op::RemoteRename { name: name.clone(), to: v.trimmed("to") },
        );
    }

    fn remove_remote(&mut self, r: &Remote, cx: &mut Context<Self>) {
        let name = r.name.clone();
        let mut body = vec![Para::Text(format!(
            "The remote and its {} are removed from this repository. Nothing changes on the server.",
            plural(r.branches.len(), "remote-tracking branch").replace("branchs", "branches")
        ))];
        if let Some(url) = &r.url {
            body.push(Para::Code(url.clone()));
        }
        let this = cx.entity().downgrade();
        overlay::confirm(
            cx,
            ConfirmOpts { title: format!("Remove the remote {}?", r.name), body, confirm: "Remove".into(), danger: true },
            move |_, cx| {
                let op = Op::RemoteRemove { name: name.clone() };
                let _ = this.update(cx, |this, cx| this.op(op, cx, |_, _, _| {}));
            },
        );
    }

    // ------------------------------------------------------------ menus

    pub fn branch_items(&self, name: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let is_current = self.current_branch().is_some_and(|b| b.name == name);
        let here = self.current_branch().map(|b| b.name.clone());
        let n = name.to_string();
        let mut items = vec![MenuItem::title(name.to_string())];
        items.push(
            item(&w, "check", "Checkout", {
                let n = n.clone();
                move |this, cx| this.checkout(n.clone(), "branch", cx)
            })
            .disabled(is_current),
        );
        if let Some(here) = here.filter(|_| !is_current) {
            items.push(item(&w, "merge", format!("Merge into {here}"), {
                let (n, h) = (n.clone(), here.clone());
                move |this, cx| this.merge(n.clone(), h.clone(), cx)
            }));
            items.push(item(&w, "branch", format!("Rebase {here} onto this branch"), {
                let (n, h) = (n.clone(), here.clone());
                move |this, cx| this.rebase(h.clone(), n.clone(), cx)
            }));
        }
        items.push(MenuItem::sep());
        items.push(item(&w, "push", "Push", {
            let n = n.clone();
            move |this, cx| this.push_branch(Some(n.clone()), false, cx)
        }));
        items.push(
            item(&w, "push", "Force push…", {
                let n = n.clone();
                move |this, cx| this.push_branch(Some(n.clone()), true, cx)
            })
            .danger(),
        );
        items.push(MenuItem::sep());
        items.push(item(&w, "edit", "Rename…", {
            let n = n.clone();
            move |this, cx| this.rename_branch(n.clone(), cx)
        }));
        let mut delete = item(&w, "trash", "Delete…", {
            let n = n.clone();
            move |this, cx| this.delete_branch(n.clone(), cx)
        })
        .danger()
        .disabled(is_current);
        if is_current {
            delete = delete.sub("Check out another branch first");
        }
        items.push(delete);
        items.push(MenuItem::new("copy", "Copy name", move |_, cx| overlay::copy_text(cx, n.clone(), "")));
        items
    }

    pub fn remote_branch_items(&self, full: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let here = self.current_branch().map(|b| b.name.clone());
        let n = full.to_string();
        let mut items = vec![MenuItem::title(full.to_string())];
        items.push(item(&w, "check", "Checkout as a local branch", {
            let n = n.clone();
            move |this, cx| this.checkout(n.clone(), "remote", cx)
        }));
        if let Some(here) = here {
            items.push(item(&w, "merge", format!("Merge into {here}"), {
                let (n, h) = (n.clone(), here.clone());
                move |this, cx| this.merge(n.clone(), h.clone(), cx)
            }));
            items.push(item(&w, "branch", format!("Rebase {here} onto this branch"), {
                let (n, h) = (n.clone(), here.clone());
                move |this, cx| this.rebase(h.clone(), n.clone(), cx)
            }));
        }
        items.push(MenuItem::sep());
        items.push(
            item(&w, "trash", "Delete on the remote…", {
                let n = n.clone();
                move |this, cx| this.delete_remote_branch(n.clone(), cx)
            })
            .danger(),
        );
        items.push(MenuItem::new("copy", "Copy name", move |_, cx| overlay::copy_text(cx, n.clone(), "")));
        items
    }

    pub fn tag_items(&self, name: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let n = name.to_string();
        let remotes: Vec<String> = self.remotes().iter().take(3).map(|r| r.name.clone()).collect();
        let mut items = vec![MenuItem::title(name.to_string())];
        items.push(item(&w, "check", "Checkout (detached HEAD)", {
            let n = n.clone();
            move |this, cx| this.checkout(n.clone(), "tag", cx)
        }));
        for r in &remotes {
            items.push(item(&w, "push", format!("Push to {r}"), {
                let (n, r) = (n.clone(), r.clone());
                move |this, cx| this.push_tag(n.clone(), r.clone(), false, cx)
            }));
        }
        items.push(MenuItem::sep());
        items.push(
            item(&w, "trash", "Delete tag", {
                let n = n.clone();
                move |this, cx| this.delete_tag(n.clone(), cx)
            })
            .danger(),
        );
        for r in &remotes {
            items.push(
                item(&w, "trash", format!("Delete on {r}…"), {
                    let (n, r) = (n.clone(), r.clone());
                    move |this, cx| this.push_tag(n.clone(), r.clone(), true, cx)
                })
                .danger(),
            );
        }
        items.push(MenuItem::new("copy", "Copy name", move |_, cx| overlay::copy_text(cx, n.clone(), "")));
        items
    }

    pub fn stash_items(&self, index: usize, message: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let m = message.to_string();
        vec![
            MenuItem::title(format!("stash@{{{index}}}")),
            item(&w, "download", "Apply", move |this, cx| this.stash_apply(index, false, cx)).sub("Keep the stash"),
            item(&w, "download", "Pop", move |this, cx| this.stash_apply(index, true, cx)).sub("Apply and remove the stash"),
            MenuItem::sep(),
            item(&w, "trash", "Drop…", move |this, cx| this.stash_drop(index, m.clone(), cx)).danger(),
        ]
    }

    pub fn remote_items(&self, r: &Remote, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let w = cx.entity().downgrade();
        let name = r.name.clone();
        let (r1, r2, r3) = (r.clone(), r.clone(), r.clone());
        let mut items = vec![
            MenuItem::title(r.name.clone()),
            item(&w, "fetch", format!("Fetch {}", r.name), {
                let n = name.clone();
                move |this, cx| this.net(NetJob::Fetch { remote: Some(n.clone()), prune: false }, false, cx, None)
            }),
            item(&w, "fetch", "Fetch and prune", {
                let n = name.clone();
                move |this, cx| this.net(NetJob::Fetch { remote: Some(n.clone()), prune: true }, false, cx, None)
            })
            .sub("Remove branches deleted on the remote"),
            MenuItem::sep(),
            item(&w, "edit", "Edit URL…", move |this, cx| this.edit_remote(&r1, cx)),
            item(&w, "edit", "Rename…", move |this, cx| this.rename_remote(&r2, cx)),
            item(&w, "trash", "Remove…", move |this, cx| this.remove_remote(&r3, cx)).danger(),
        ];
        if let Some(url) = r.url.clone() {
            items.push(MenuItem::new("copy", "Copy URL", move |_, cx| overlay::copy_text(cx, url.clone(), "")));
        }
        items
    }

    /// Menu of a graph row; `label` is the ref label under the pointer, if any.
    pub fn commit_menu(&mut self, row: usize, label: Option<(String, &'static str)>, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(h) = self.history.clone() else { return };
        let Some(c) = h.commits.get(row) else { return };
        let w = cx.entity().downgrade();
        let labels = h.labels.get(&c.oid).cloned().unwrap_or_default();
        let summary = c.summary.to_string();
        let items = match c.kind {
            Kind::Wip => vec![item(&w, "stash", "Stash changes…", |this, cx| this.stash_save(cx))],
            Kind::Stash => {
                let name = labels.iter().find(|l| l.kind == "stash").map(|l| l.name.clone()).unwrap_or_default();
                self.stash_items(stash_index(&name), &summary, cx)
            }
            Kind::Commit => {
                let oid = c.oid.to_string();
                let short = oid[..7].to_string();
                let mut items = match &label {
                    Some((name, "branch")) => self.branch_items(name, cx),
                    Some((name, "remote")) => self.remote_branch_items(name, cx),
                    Some((name, "tag")) => self.tag_items(name, cx),
                    Some((name, "stash")) => self.stash_items(stash_index(name), &summary, cx),
                    _ => Vec::new(),
                };
                if !items.is_empty() {
                    items.push(MenuItem::sep());
                }
                items.push(MenuItem::title(format!("Commit {short}")));
                let at_label = format!("{short} {summary}");
                items.push(
                    item(&w, "check", "Checkout this commit", {
                        let o = oid.clone();
                        move |this, cx| this.checkout(o.clone(), "commit", cx)
                    })
                    .sub("Detached HEAD"),
                );
                items.push(item(&w, "branch", "Create a branch here…", {
                    let (o, l) = (oid.clone(), at_label.clone());
                    move |this, cx| this.create_branch_at(Some(o.clone()), Some(l.clone()), cx)
                }));
                items.push(item(&w, "tag", "Create a tag here…", {
                    let (o, l) = (oid.clone(), at_label.clone());
                    move |this, cx| this.create_tag(Some(o.clone()), Some(l.clone()), cx)
                }));
                items.extend(self.history_items(&oid, labels.iter().any(|l| l.current), cx));
                items.push(MenuItem::sep());
                let (o, s) = (oid.clone(), summary.clone());
                items.push(
                    MenuItem::new("copy", "Copy commit id", move |_, cx| overlay::copy_text(cx, o.clone(), "Commit id copied"))
                        .sub(oid[..10].to_string()),
                );
                items.push(MenuItem::new("copy", "Copy message", move |_, cx| overlay::copy_text(cx, s.clone(), "")));
                items
            }
        };
        overlay::open_menu(cx, pos, items);
    }

    /// A branch label was dropped on another one: offer merge and rebase.
    pub fn ref_dropped(&mut self, from: DraggedRef, to: String, to_kind: &'static str, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if from.name == to {
            return;
        }
        let w = cx.entity().downgrade();
        let here = self.current_branch().map(|b| b.name.clone());
        let mut items = vec![MenuItem::title(format!("{} → {to}", from.name))];
        if to_kind == "branch" {
            let mut it = item(&w, "merge", format!("Merge {} into {to}", from.name), {
                let (f, t) = (from.name.clone(), to.clone());
                move |this, cx| this.merge(f.clone(), t.clone(), cx)
            });
            if here.as_deref() != Some(to.as_str()) {
                it = it.sub(format!("Checks out {to} first"));
            }
            items.push(it);
        }
        if from.kind == "branch" {
            let mut it = item(&w, "branch", format!("Rebase {} onto {to}", from.name), {
                let (f, t) = (from.name.clone(), to.clone());
                move |this, cx| this.rebase(f.clone(), t.clone(), cx)
            });
            if here.as_deref() != Some(from.name.as_str()) {
                it = it.sub(format!("Checks out {} first", from.name));
            }
            items.push(it);
        }
        if items.len() > 1 {
            overlay::open_menu(cx, pos, items);
        }
    }

    // ------------------------------------------------------------ toolbar

    pub fn render_actions(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let b = self.current_branch();
        let busy = !self.jobs.running.is_empty();
        let act = |id: &'static str, name: &'static str, label: &'static str, title: String| {
            btn(id, label).icon(name).icon_size(15.).ghost().tip(title).disabled(busy)
        };
        let caret = |id: &'static str, title: &'static str| icon_btn(id, "chevron").icon_size(12.).tip(title).disabled(busy);
        let pull_tip = match b.and_then(|b| b.upstream.as_ref().map(|u| (u, &b.name))) {
            Some((u, name)) => format!("Pull {u} into {name}"),
            None => "Pull the upstream branch".into(),
        };
        let push_tip = match b {
            Some(b) => match &b.upstream {
                Some(u) => format!("Push {} to {u}", b.name),
                None => format!("Push {} and choose its upstream", b.name),
            },
            None => "Check out a branch to push".into(),
        };
        let mut pull = act("act-pull", "pull", "Pull", pull_tip).on_click(cx.listener(|this, _, _, cx| this.pull(None, cx)));
        if let Some(n) = b.map(|b| b.behind).filter(|n| *n > 0) {
            pull = pull.suffix(format!("↓{n}"));
        }
        let mut push = act("act-push", "push", "Push", push_tip).on_click(cx.listener(|this, _, _, cx| this.push(false, cx)));
        if let Some(n) = b.map(|b| b.ahead).filter(|n| *n > 0) {
            push = push.suffix(format!("↑{n}"));
        }
        let has_stash = self.refs.as_ref().is_some_and(|r| !r.stashes.is_empty());
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                act("act-fetch", "fetch", "Fetch", "Fetch all remotes (Ctrl+Shift+F)".into())
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_all(false, false, cx))),
            )
            .child(pull)
            .child(caret("act-pull-more", "More ways to pull and fetch").on_click(cx.listener(|_, e: &gpui::ClickEvent, _, cx| {
                let w = cx.entity().downgrade();
                overlay::open_menu(
                    cx,
                    e.position(),
                    vec![
                        item(&w, "pull", "Pull", |this, cx| this.pull(None, cx)).sub("As set in your Git configuration"),
                        item(&w, "pull", "Pull, fast-forward only", |this, cx| this.pull(Some("ff-only"), cx))
                            .sub("Stops when the branches diverged"),
                        item(&w, "merge", "Pull with merge", |this, cx| this.pull(Some("merge"), cx)),
                        item(&w, "branch", "Pull with rebase", |this, cx| this.pull(Some("rebase"), cx)),
                        MenuItem::sep(),
                        item(&w, "fetch", "Fetch all and prune", |this, cx| this.fetch_all(true, false, cx))
                            .sub("Remove branches deleted on the remotes"),
                    ],
                );
            })))
            .child(push)
            .child(caret("act-push-more", "More ways to push").on_click(cx.listener(|_, e: &gpui::ClickEvent, _, cx| {
                let w = cx.entity().downgrade();
                overlay::open_menu(
                    cx,
                    e.position(),
                    vec![
                        item(&w, "push", "Push", |this, cx| this.push(false, cx)),
                        item(&w, "push", "Force push…", |this, cx| this.push(true, cx)).sub("With --force-with-lease").danger(),
                    ],
                );
            })))
            .child(div().w(px(1.)).h(px(18.)).mx_1().bg(t.border))
            .child(
                act("act-branch", "branch", "Branch", "Create a branch at HEAD (Ctrl+B)".into())
                    .on_click(cx.listener(|this, _, _, cx| this.create_branch(None, cx))),
            )
            .child(
                act("act-stash", "stash", "Stash", "Stash the uncommitted changes (Ctrl+Shift+S)".into())
                    .on_click(cx.listener(|this, _, _, cx| this.stash_save(cx))),
            )
            .child(
                act("act-pop", "download", "Pop", "Apply the newest stash and remove it".into())
                    .disabled(busy || !has_stash)
                    .on_click(cx.listener(|this, _, _, cx| this.stash_apply(0, true, cx))),
            );
        if let Some(job) = self.jobs.running.last() {
            let id = job.id.clone();
            let text = match job.percent {
                Some(p) => format!("{} {p}%", job.title),
                None => job.title.clone(),
            };
            bar = bar.child(
                div()
                    .id("job-status")
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .ml_2()
                    .pl_2()
                    .pr_1()
                    .h(px(28.))
                    .max_w(px(320.))
                    .rounded(px(6.))
                    .bg(t.panel2)
                    .border_1()
                    .border_color(t.border)
                    .overflow_hidden()
                    .text_size(px(12.))
                    .tooltip(tip(format!("{}\n{}", job.command, job.line)))
                    .child(icon("refresh", 12., t.accent))
                    .child(div().flex_none().child(text))
                    .children((!job.line.is_empty()).then(|| div().min_w_0().text_color(t.dim).child(clip(job.line.clone()))))
                    .child(icon_btn("job-cancel", "x").icon_size(12.).tip("Cancel").on_click(move |_, _, cx| {
                        core(cx).jobs.cancel(&id);
                    }))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .bottom_0()
                            .h(px(2.))
                            .w(gpui::relative(job.percent.unwrap_or(0) as f32 / 100.))
                            .bg(t.accent),
                    ),
            );
        }
        bar
    }
}

// ---------------------------------------------------------------- clone

/// The clone dialog that is open, so job events reach it.
struct CloneWatch(WeakEntity<CloneDialog>);

impl Global for CloneWatch {}

/// Called by the shell for every `job` event.
pub fn on_job_event(job: &Value, cx: &mut App) {
    let Some(dialog) = cx.try_global::<CloneWatch>().map(|w| w.0.clone()) else { return };
    let _ = dialog.update(cx, |d, cx| d.on_job(job, cx));
}

struct CloneDialog {
    url: Entity<TextInput>,
    parent: Entity<TextInput>,
    name: Entity<TextInput>,
    status: Option<(String, bool)>,
    job: Option<String>,
    target: Option<String>,
    percent: Option<u8>,
    focus: FocusHandle,
}

impl Focusable for CloneDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.url.focus_handle(cx)
    }
}

impl CloneDialog {
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let (url, parent, name) =
            (self.url.read(cx).text().to_string(), self.parent.read(cx).text().to_string(), self.name.read(cx).text().to_string());
        match core(cx).clone_repo(&url, &parent, Some(&name)) {
            Ok((info, target)) => {
                set_pref(cx, "gl.cloneDir", &parent.trim().to_string());
                self.job = Some(info.id);
                self.target = Some(target.display().to_string());
                self.status = Some(("Starting…".into(), false));
                self.percent = None;
            }
            Err(e) => self.status = Some((format!("{e:#}"), true)),
        }
        cx.notify();
    }

    fn on_job(&mut self, v: &Value, cx: &mut Context<Self>) {
        if self.job.as_deref() != v["id"].as_str() {
            return;
        }
        let line = v["line"].as_str().filter(|l| !l.is_empty()).or(v["title"].as_str()).unwrap_or("").to_string();
        self.percent = v["percent"].as_u64().map(|p| p as u8);
        match v["state"].as_str() {
            Some("running") => self.status = Some((line, false)),
            Some("done") => {
                self.job = None;
                if let Some(path) = self.target.take() {
                    // The shell is in the middle of handling this event: open the tab after it.
                    overlay::with_shell(cx, move |shell, _, cx| shell.open_repo(path, true, false, cx));
                }
                overlay::success(cx, "Clone finished");
                close_modal(cx);
            }
            Some("cancelled") => {
                self.job = None;
                self.status = Some(("Cancelled.".into(), true));
            }
            _ => {
                self.job = None;
                let output = v["output"].as_str().filter(|o| !o.is_empty()).unwrap_or(&line);
                self.status = Some((format!("{}\n{output}", v["command"].as_str().unwrap_or("")), true));
            }
        }
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                if let Some(p) = paths.first() {
                    let text = p.display().to_string();
                    let _ = this.update(cx, |this, cx| this.parent.update(cx, |i, cx| i.set_text(text, cx)));
                }
            }
        })
        .detach();
    }
}

impl Render for CloneDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let label = |text: &'static str| div().text_color(t.muted).text_size(px(12.)).child(text);
        let busy = self.job.is_some();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().gap_1().child(label("Repository URL")).child(field(&self.url, window, cx)))
            .child(
                div().flex().flex_col().gap_1().child(label("Clone into")).child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(field(&self.parent, window, cx)))
                        .child(btn("clone-browse", "Browse").icon("folder").on_click(cx.listener(|this, _, _, cx| this.browse(cx)))),
                ),
            )
            .child(div().flex().flex_col().gap_1().child(label("Folder name")).child(field(&self.name, window, cx)))
            .child(
                div()
                    .text_color(t.dim)
                    .text_size(px(12.))
                    .child("Sign-in uses your Git setup: the credential helper and the SSH agent. Graftline does not store passwords."),
            );
        if busy {
            body = body.child(
                div()
                    .h(px(3.))
                    .rounded(px(2.))
                    .bg(t.panel2)
                    .child(div().h_full().rounded(px(2.)).bg(t.accent).w(gpui::relative(self.percent.unwrap_or(4) as f32 / 100.))),
            );
        }
        if let Some((text, is_error)) = &self.status {
            body = body.child(
                div()
                    .text_color(if *is_error { t.danger } else { t.muted })
                    .text_size(px(12.))
                    .children(text.lines().map(|l| div().child(SharedString::from(l.to_string())))),
            );
        }
        let cancel = btn("clone-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| match &this.job {
            Some(id) => {
                core(cx).jobs.cancel(id);
            }
            None => close_modal(cx),
        }));
        let go = btn("clone-go", "Clone").primary().disabled(busy).on_click(cx.listener(|this, _, _, cx| this.start(cx)));
        div()
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &super::shell::Confirm, _, cx| this.start(cx)))
            .child(modal_frame("Clone a repository", 520., body, vec![cancel.into_any_element(), go.into_any_element()], cx))
    }
}

/// `https://example.com/team/project.git` -> `project`.
fn folder_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or("");
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

pub fn open_clone_dialog(cx: &mut App) {
    if core(cx).git.is_none() {
        overlay::error(cx, "Git was not found on PATH. Install Git to clone a repository.");
        return;
    }
    let default_parent: String = pref(cx, "gl.cloneDir").unwrap_or_default();
    overlay::open_modal(cx, move |window, cx: &mut Context<CloneDialog>| {
        let url = cx.new(|cx| TextInput::new(cx).placeholder("https://example.com/team/project.git"));
        let parent = cx.new(|cx| TextInput::new(cx).placeholder("Folder that receives the new repository").with_text(default_parent));
        let name = cx.new(|cx| TextInput::new(cx).placeholder("Folder name (from the URL)"));
        cx.subscribe_in(&url, window, |this: &mut CloneDialog, url, event: &InputEvent, _, cx| match event {
            InputEvent::Change => {
                // The folder name follows the URL until the user types one.
                let guess = folder_name(url.read(cx).text());
                let hint = if guess.is_empty() { "Folder name (from the URL)".to_string() } else { guess };
                this.name.update(cx, |i, cx| i.set_placeholder(hint, cx));
            }
            InputEvent::Enter | InputEvent::Submit => this.start(cx),
            InputEvent::ShiftEnter => {}
        })
        .detach();
        for input in [&parent, &name] {
            cx.subscribe(input, |this: &mut CloneDialog, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Enter | InputEvent::Submit) {
                    this.start(cx);
                }
            })
            .detach();
        }
        cx.set_global(CloneWatch(cx.entity().downgrade()));
        // Closing the dialog stops a clone that is still running.
        cx.on_release(|this, cx| {
            if let Some(id) = &this.job {
                core(cx).jobs.cancel(id);
            }
        })
        .detach();
        CloneDialog { url, parent, name, status: None, job: None, target: None, percent: None, focus: cx.focus_handle() }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_name_comes_from_the_url() {
        assert_eq!(folder_name("https://example.com/team/project.git"), "project");
        assert_eq!(folder_name("git@example.com:team/project.git"), "project");
        assert_eq!(folder_name("C:\\repos\\thing\\"), "thing");
        assert_eq!(folder_name(""), "");
    }

    #[test]
    fn stash_index_is_read_from_the_label() {
        assert_eq!(stash_index("stash@{3}"), 3);
        assert_eq!(stash_index("stash"), 0);
    }
}
