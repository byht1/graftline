//! The list of branches, remotes, tags, stashes, submodules and worktrees in the sidebar.
//! The shell draws the sidebar; the data and the actions belong to the repository view.

use super::graph_view::{DragChip, DraggedRef};
use super::overlay::{self, MenuItem};
use super::repo_view::RepoView;
use super::theme::theme;
use super::widgets::{clip, fmt_num, icon, icon_btn, tip, RIGHT};
use super::{pref, set_pref};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Div, Entity, FontWeight, MouseDownEvent, Stateful};
use std::collections::BTreeMap;

type ViewFn = Box<dyn Fn(&mut RepoView, &mut Context<RepoView>)>;
type MenuFn = Box<dyn Fn(&RepoView, &mut Context<RepoView>) -> Vec<MenuItem>>;

/// Items a section shows before "Show all".
const SECTION_LIMIT: usize = 200;

fn closed_sections(cx: &App) -> BTreeMap<String, bool> {
    pref(cx, "gl.sbClosed").unwrap_or_else(|| BTreeMap::from([("tags".to_string(), true)]))
}

pub fn refs_list(tab: &Entity<RepoView>, filter: &str, cx: &mut App) -> AnyElement {
    let t = theme(cx);
    let q = filter.trim().to_lowercase();
    let hit = |name: &str| q.is_empty() || name.to_lowercase().contains(&q);
    let closed = closed_sections(cx);
    let view = tab.read(cx);
    let mut out = div().id("sb-scroll").flex_1().min_h_0().overflow_y_scroll().pb_2();
    if let Some(e) = &view.refs_error {
        return out.child(div().p_3().text_color(t.danger).text_size(px(12.)).child(e.clone())).into_any_element();
    }
    let Some(refs) = view.refs.clone() else {
        return out.child(div().p_3().text_color(t.muted).text_size(px(12.)).child("Loading…")).into_any_element();
    };
    let extras = view.extras.clone();
    let expanded = view.expanded.clone();
    let weak = tab.downgrade();

    // One row of the list.
    let row = |id: (&'static str, usize), name: &'static str, label: String, nested: bool, current: bool| -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .h(px(26.))
            .mx_1()
            .pl(px(if nested { 26. } else { 10. }))
            .pr_2()
            .rounded(px(5.))
            .cursor_pointer()
            .when(current, |el| el.text_color(t.accent).font_weight(FontWeight::SEMIBOLD))
            .hover(|s| s.bg(t.hover))
            .child(icon(name, 14., if current { t.accent } else { t.muted }))
            .child(div().flex_1().min_w_0().child(clip(label)))
    };
    let track = |text: String| div().flex_none().text_size(px(11.)).text_color(t.muted).child(text);
    // Runs a method of the repository view from an element handler.
    let on = |f: ViewFn| {
        let weak = weak.clone();
        move |cx: &mut App| {
            let _ = weak.update(cx, |this, cx| f(this, cx));
        }
    };
    let menu = |build: MenuFn| {
        let weak = weak.clone();
        move |e: &MouseDownEvent, _: &mut gpui::Window, cx: &mut App| {
            let items = weak.update(cx, |this, cx| build(this, cx)).unwrap_or_default();
            overlay::open_menu(cx, e.position, items);
        }
    };

    let section = |out: Stateful<Div>,
                   key: &'static str,
                   title: &'static str,
                   count: usize,
                   add: Option<(&'static str, ViewFn)>,
                   items: Vec<AnyElement>,
                   empty: &'static str|
     -> Stateful<Div> {
        let is_closed = q.is_empty() && closed.get(key).copied().unwrap_or(false);
        let mut closed_next = closed.clone();
        closed_next.insert(key.to_string(), !is_closed);
        let mut head = div().flex().items_center().h(px(28.)).mt_1().px_2().child(
            div()
                .id(("sb-section", key.len() * 31 + key.as_bytes()[0] as usize))
                .flex()
                .flex_1()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.muted)
                .child(icon(if is_closed { "right" } else { "chevron" }, 12., t.dim))
                .child(title)
                .when(key == "local", |el| {
                    el.tooltip(tip("Double click a branch to check it out.\nDrag a branch onto another one to merge or rebase."))
                })
                .child(div().font_weight(FontWeight::NORMAL).text_color(t.dim).child(fmt_num(count)))
                .on_click(move |_, _, cx| {
                    set_pref(cx, "gl.sbClosed", &closed_next);
                    cx.refresh_windows();
                }),
        );
        if let Some((title, run)) = add {
            let run = on(run);
            head = head.child(
                icon_btn(("sb-add", key.len() * 31 + key.as_bytes()[0] as usize), "plus")
                    .icon_size(13.)
                    .tip(title)
                    .on_click(move |_, _, cx| run(cx)),
            );
        }
        let mut out = out.child(head);
        if is_closed {
            return out;
        }
        if items.is_empty() {
            return out.child(div().px_3().py_1().text_size(px(12.)).text_color(t.dim).child(if q.is_empty() {
                empty
            } else {
                "No matches"
            }));
        }
        let total = items.len();
        let limit = if expanded.contains(key) { total } else { SECTION_LIMIT };
        out = out.children(items.into_iter().take(limit));
        if total > limit {
            let show = on(Box::new(move |this, cx| {
                this.expanded.insert(key);
                cx.notify();
            }));
            out = out.child(
                div()
                    .id(("sb-more", key.len()))
                    .mx_1()
                    .px_3()
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_color(t.accent)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .child(format!("Show all {}", fmt_num(total)))
                    .on_click(move |_, _, cx| show(cx)),
            );
        }
        out
    };

    // ---- local branches
    let mut items = Vec::new();
    for (i, b) in refs.branches.iter().filter(|b| hit(&b.name)).enumerate() {
        // No tooltip on the row: it would cover the context menu that opens under the pointer.
        let (oid, name, current) = (b.oid.clone(), b.name.clone(), b.current);
        let (n1, n2, n3) = (b.name.clone(), b.name.clone(), b.name.clone());
        let click = on(Box::new(move |this, cx| this.select_oid(&oid, cx)));
        let dbl = on(Box::new(move |this, cx| {
            if !current {
                this.checkout(name.clone(), "branch", cx);
            }
        }));
        let drop_weak = weak.clone();
        let mut el = row(("sb-branch", i), if b.current { "check" } else { "branch" }, b.name.clone(), false, b.current)
            .on_click(move |e, _, cx| if e.click_count() >= 2 { dbl(cx) } else { click(cx) })
            .on_mouse_down(RIGHT, menu(Box::new(move |this, cx| this.branch_items(&n1, cx))))
            .on_drag(DraggedRef { name: n2, kind: "branch" }, |d: &DraggedRef, _, _, cx| cx.new(|_| DragChip(d.name.clone())))
            .drag_over::<DraggedRef>(|s, _, _, cx| s.bg(theme(cx).accent_soft))
            .on_drop(move |d: &DraggedRef, window, cx| {
                let (d, to, pos) = (d.clone(), n3.clone(), window.mouse_position());
                let _ = drop_weak.update(cx, |this, cx| this.ref_dropped(d, to, "branch", pos, cx));
            });
        if b.gone {
            let up = b.upstream.clone().unwrap_or_default();
            el = el.child(
                div()
                    .id(("sb-gone", i))
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(t.danger)
                    .tooltip(tip(format!("The upstream {up} no longer exists")))
                    .child("gone"),
            );
        } else if b.ahead > 0 || b.behind > 0 {
            let mut text = String::new();
            if b.ahead > 0 {
                text.push_str(&format!("↑{}", b.ahead));
            }
            if b.ahead > 0 && b.behind > 0 {
                text.push(' ');
            }
            if b.behind > 0 {
                text.push_str(&format!("↓{}", b.behind));
            }
            let up = b.upstream.clone().unwrap_or_default();
            el = el.child(
                div()
                    .id(("sb-track", i))
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(t.muted)
                    .tooltip(tip(format!("{} ahead of, {} behind {up}", b.ahead, b.behind)))
                    .child(text),
            );
        }
        items.push(el.into_any_element());
    }
    out = section(
        out,
        "local",
        "Local branches",
        items.len(),
        Some(("Create a branch", Box::new(|this, cx| this.create_branch(None, cx)))),
        items,
        "No local branches",
    );

    // ---- remotes
    let mut items = Vec::new();
    let mut n = 0;
    for r in &refs.remotes {
        let branches: Vec<_> = r.branches.iter().filter(|b| hit(&b.full)).collect();
        if !q.is_empty() && branches.is_empty() {
            continue;
        }
        let remote = r.clone();
        items.push(
            div()
                .id(("sb-remote", n))
                .flex()
                .items_center()
                .gap_2()
                .h(px(26.))
                .mx_1()
                .pl(px(10.))
                .pr_2()
                .rounded(px(5.))
                .text_color(t.muted)
                .tooltip(tip(format!("{}\nRight click for fetch, edit, remove", r.url.clone().unwrap_or_default())))
                .child(icon("cloud", 13., t.muted))
                .child(div().flex_1().min_w_0().child(clip(r.name.clone())))
                .on_mouse_down(RIGHT, menu(Box::new(move |this, cx| this.remote_items(&remote, cx))))
                .into_any_element(),
        );
        n += 1;
        for b in branches {
            let (oid, full) = (b.oid.clone(), b.full.clone());
            let (f1, f2, f3) = (b.full.clone(), b.full.clone(), b.full.clone());
            let click = on(Box::new(move |this, cx| this.select_oid(&oid, cx)));
            let dbl = on(Box::new(move |this, cx| this.checkout(full.clone(), "remote", cx)));
            let drop_weak = weak.clone();
            items.push(
                row(("sb-rbranch", n), "branch", b.name.clone(), true, false)
                    .on_click(move |e, _, cx| if e.click_count() >= 2 { dbl(cx) } else { click(cx) })
                    .on_mouse_down(RIGHT, menu(Box::new(move |this, cx| this.remote_branch_items(&f1, cx))))
                    .on_drag(DraggedRef { name: f2, kind: "remote" }, |d: &DraggedRef, _, _, cx| cx.new(|_| DragChip(d.name.clone())))
                    .drag_over::<DraggedRef>(|s, _, _, cx| s.bg(theme(cx).accent_soft))
                    .on_drop(move |d: &DraggedRef, window, cx| {
                        let (d, to, pos) = (d.clone(), f3.clone(), window.mouse_position());
                        let _ = drop_weak.update(cx, |this, cx| this.ref_dropped(d, to, "remote", pos, cx));
                    })
                    .into_any_element(),
            );
            n += 1;
        }
    }
    out = section(
        out,
        "remotes",
        "Remotes",
        items.len(),
        Some(("Add a remote", Box::new(|this, cx| this.add_remote(cx)))),
        items,
        "No remotes",
    );

    // ---- tags
    let mut items = Vec::new();
    for (i, tag) in refs.tags.iter().filter(|t| hit(&t.name)).enumerate() {
        let (oid, name) = (tag.oid.clone(), tag.name.clone());
        let click = on(Box::new(move |this, cx| this.select_oid(&oid, cx)));
        items.push(
            row(("sb-tag", i), "tag", tag.name.clone(), false, false)
                .tooltip(tip(if tag.annotated { format!("{} (annotated)", tag.name) } else { tag.name.clone() }))
                .on_click(move |_, _, cx| click(cx))
                .on_mouse_down(RIGHT, menu(Box::new(move |this, cx| this.tag_items(&name, cx))))
                .into_any_element(),
        );
    }
    out = section(
        out,
        "tags",
        "Tags",
        items.len(),
        Some(("Create a tag at HEAD", Box::new(|this, cx| this.create_tag(None, None, cx)))),
        items,
        "No tags",
    );

    // ---- stashes
    let mut items = Vec::new();
    for (i, s) in refs.stashes.iter().filter(|s| hit(&s.message)).enumerate() {
        let (oid, index, message) = (s.oid.clone(), s.index, s.message.clone());
        let click = on(Box::new(move |this, cx| this.select_oid(&oid, cx)));
        let label = if s.message.is_empty() { format!("stash@{{{}}}", s.index) } else { s.message.clone() };
        items.push(
            row(("sb-stash", i), "stash", label, false, false)
                .tooltip(tip(s.message.clone()))
                .on_click(move |_, _, cx| click(cx))
                .on_mouse_down(RIGHT, menu(Box::new(move |this, cx| this.stash_items(index, &message, cx))))
                .into_any_element(),
        );
    }
    out = section(
        out,
        "stashes",
        "Stashes",
        items.len(),
        Some(("Stash the uncommitted changes", Box::new(|this, cx| this.stash_save(cx)))),
        items,
        "No stashes",
    );

    // ---- submodules and worktrees
    let open_repo = |path: String| {
        move |cx: &mut App| {
            let path = path.clone();
            overlay::with_shell(cx, move |shell, _, cx| shell.open_repo(path, true, false, cx));
        }
    };
    if let Some(extras) = extras {
        if !extras.submodules.is_empty() {
            let mut items = Vec::new();
            for (i, m) in extras.submodules.iter().filter(|m| hit(&m.path)).enumerate() {
                let open = open_repo(m.full_path.clone());
                let state =
                    if m.initialized { "Click to open as a repository" } else { "Not initialized: run git submodule update --init" };
                let (path, url, initialized) = (m.path.clone(), m.url.clone(), m.initialized);
                let mut el = row(("sb-sub", i), "repo", m.path.clone(), false, false)
                    .tooltip(tip(format!("{}\n{}\n{state}", m.path, m.url.clone().unwrap_or_default())))
                    .when(!initialized, |el| el.opacity(0.55))
                    .on_click(move |_, _, cx| {
                        if initialized {
                            open(cx);
                        }
                    })
                    .on_mouse_down(RIGHT, move |e: &MouseDownEvent, _, cx| {
                        let p = path.clone();
                        let mut items = vec![MenuItem::new("copy", "Copy path", move |_, cx| overlay::copy_text(cx, p.clone(), ""))];
                        if let Some(url) = url.clone() {
                            items.push(MenuItem::new("copy", "Copy URL", move |_, cx| overlay::copy_text(cx, url.clone(), "")));
                        }
                        overlay::open_menu(cx, e.position, items);
                    });
                if !m.initialized {
                    el = el.child(track("not initialized".into()));
                } else if let (Some(rec), Some(out_oid)) = (&m.recorded, &m.checked_out) {
                    if rec != out_oid {
                        el = el.child(track("modified".into()));
                    }
                }
                items.push(el.into_any_element());
            }
            out = section(out, "submodules", "Submodules", items.len(), None, items, "No submodules");
        }
        if !extras.worktrees.is_empty() {
            let mut items = Vec::new();
            for (i, w) in extras.worktrees.iter().filter(|w| hit(&w.name)).enumerate() {
                let open = open_repo(w.path.clone());
                let (path, missing) = (w.path.clone(), w.missing);
                let state = if w.missing {
                    "missing".to_string()
                } else if w.locked {
                    "locked".to_string()
                } else {
                    w.branch.clone().unwrap_or_else(|| "detached".into())
                };
                items.push(
                    row(("sb-wt", i), "folder", w.name.clone(), false, false)
                        .tooltip(tip(format!(
                            "{}\n{}",
                            w.path,
                            if w.missing { "The folder no longer exists" } else { "Click to open as a repository" }
                        )))
                        .when(missing, |el| el.opacity(0.55))
                        .on_click(move |_, _, cx| {
                            if !missing {
                                open(cx);
                            }
                        })
                        .on_mouse_down(RIGHT, move |e: &MouseDownEvent, _, cx| {
                            let p = path.clone();
                            overlay::open_menu(
                                cx,
                                e.position,
                                vec![MenuItem::new("copy", "Copy path", move |_, cx| overlay::copy_text(cx, p.clone(), ""))],
                            );
                        })
                        .child(div().flex_none().max_w(px(90.)).text_size(px(11.)).text_color(t.muted).child(clip(state)))
                        .into_any_element(),
                );
            }
            out = section(out, "worktrees", "Worktrees", items.len(), None, items, "No worktrees");
        }
    }
    out.into_any_element()
}
