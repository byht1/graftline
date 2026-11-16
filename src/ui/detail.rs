//! The panel on the right: details of the selected commit and its changed files.

use super::diff_view::{DiffPanel, DiffSource};
use super::overlay::{self, MenuItem};
use super::repo_view::{Detail, Panel, RepoView, Sel};
use super::theme::Theme;
use super::widgets::{avatar, badge, clip, fmt_date, fmt_num, icon, rel_time, scrollbar, tip, RIGHT};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Div, FontWeight, Hsla, MouseDownEvent, SharedString, Stateful, Window};
use graftline::repo::{CommitDetail, FileChange, Person};

/// Color of a status letter.
pub fn status_color(status: char, t: &Theme) -> Hsla {
    match status {
        'A' | '?' => t.ok,
        'D' => t.danger,
        'U' => t.danger,
        'R' | 'C' => t.t_id,
        _ => t.t_num,
    }
}

/// One file of a list: status letter, path with a dim directory, line counts.
pub fn file_row(id: impl Into<gpui::ElementId>, f: &FileChange, active: bool, t: &Theme) -> Stateful<Div> {
    let slash = f.path.rfind('/');
    let (dir, name) = match slash {
        Some(i) => (&f.path[..=i], &f.path[i + 1..]),
        None => ("", f.path.as_str()),
    };
    let title = match &f.old_path {
        Some(old) => format!("{old} → {}", f.path),
        None => f.path.clone(),
    };
    let stat: AnyElement = if f.binary {
        div().text_color(t.muted).child("bin").into_any_element()
    } else {
        div()
            .flex()
            .gap_1()
            .children(f.additions.filter(|n| *n > 0).map(|n| div().text_color(t.ok).child(format!("+{n}"))))
            .children(f.deletions.filter(|n| *n > 0).map(|n| div().text_color(t.danger).child(format!("−{n}"))))
            .into_any_element()
    };
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap_2()
        .h(px(26.))
        .px_3()
        .cursor_pointer()
        .when(active, |el| el.bg(t.row_sel))
        .when(!active, |el| el.hover(|s| s.bg(t.hover)))
        .tooltip(tip(title))
        .child(
            div()
                .flex_none()
                .w(px(12.))
                .font_family(super::MONO_FONT)
                .font_weight(FontWeight::BOLD)
                .text_size(px(12.))
                .text_color(status_color(f.status, t))
                .child(f.status.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .children((!dir.is_empty()).then(|| {
                    div()
                        .flex_shrink()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(t.dim)
                        .child(SharedString::from(dir.to_string()))
                }))
                .child(div().flex_none().child(SharedString::from(name.to_string()))),
        )
        .child(div().flex_none().text_size(px(12.)).font_family(super::MONO_FONT).child(stat))
}

fn person(label: &str, p: &Person, t: &Theme) -> Div {
    let rel = rel_time(p.time);
    let recent = rel.contains("ago") || rel.contains("now");
    let sub = if recent { format!("{label} · {} · {rel}", fmt_date(p.time)) } else { format!("{label} · {}", fmt_date(p.time)) };
    div().flex().items_center().gap_2().child(avatar(&p.name, &p.email, 30.)).child(
        div()
            .min_w_0()
            .child(
                div()
                    .id(SharedString::from(format!("who-{label}")))
                    .font_weight(FontWeight::SEMIBOLD)
                    .tooltip(tip(format!("{} <{}>", p.name, p.email)))
                    .child(clip(p.name.clone())),
            )
            .child(div().text_size(px(12.)).text_color(t.muted).child(clip(sub))),
    )
}

fn chip(id: impl Into<gpui::ElementId>, text: String, t: &Theme) -> Stateful<Div> {
    div()
        .id(id.into())
        .px(px(6.))
        .h(px(20.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(t.panel2)
        .border_1()
        .border_color(t.border)
        .font_family(super::MONO_FONT)
        .text_size(px(12.))
        .cursor_pointer()
        .hover(|s| s.border_color(t.accent))
        .child(text)
}

impl RepoView {
    /// Menu items for a file: its history and blame.
    pub fn file_tools(&self, path: &str, commit: Option<String>, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let (p1, p2, c2) = (path.to_string(), path.to_string(), commit.clone());
        let this2 = this.clone();
        vec![
            MenuItem::new("history", "File history", move |_, cx| {
                let _ = this.update(cx, |this, cx| this.open_file_history(p1.clone(), cx));
            }),
            MenuItem::new(
                "user",
                commit.as_ref().map(|c| format!("Blame at {}", &c[..7.min(c.len())])).unwrap_or_else(|| "Blame".into()),
                move |_, cx| {
                    let _ = this2.update(cx, |this, cx| this.open_blame(p2.clone(), c2.clone(), cx));
                },
            ),
        ]
    }

    fn open_commit_diff(&mut self, f: &FileChange, cx: &mut Context<Self>) {
        let Some(Sel::Commit(oid)) = self.selected else { return };
        let subtitle = oid.to_string()[..7].to_string();
        let panel = DiffPanel::new(f.clone(), subtitle, DiffSource::Commit(oid));
        self.panel = Some(Panel::Diff(panel));
        self.load_diff(cx);
        cx.notify();
    }

    fn render_commit(&self, c: &CommitDetail, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let same_committer =
            c.author.name == c.committer.name && c.author.email == c.committer.email && (c.author.time - c.committer.time).abs() < 60;
        let oid = c.oid.clone();
        let mut parents = div().flex().flex_wrap().gap_1();
        if c.parents.is_empty() {
            parents = parents.child(div().text_color(t.muted).child("none (root commit)"));
        }
        for (i, p) in c.parents.iter().enumerate() {
            let target = p.clone();
            parents = parents.child(
                chip(("parent", i), p[..7.min(p.len())].to_string(), t)
                    .tooltip(tip(format!("{p}\nClick to go to this commit")))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_oid(&target, cx))),
            );
        }
        let label = |text: &'static str| div().w(px(60.)).flex_none().text_color(t.muted).text_size(px(12.)).child(text);
        let head = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(if c.summary.is_empty() {
                "(no message)".to_string()
            } else {
                c.summary.clone()
            }))
            .children((!c.body.is_empty()).then(|| {
                div()
                    .text_color(t.muted)
                    .text_size(px(12.5))
                    .children(c.body.lines().map(|l| div().min_h(px(16.)).child(SharedString::from(l.to_string()))))
            }))
            .child(person("authored", &c.author, t))
            .children((!same_committer).then(|| person("committed", &c.committer, t)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div().flex().items_center().child(label("Commit")).child(
                            chip("sha", c.oid[..10.min(c.oid.len())].to_string(), t)
                                .tooltip(tip(format!("{}\nClick to copy", c.oid)))
                                .on_click(move |_, _, cx| overlay::copy_text(cx, oid.clone(), "Commit id copied")),
                        ),
                    )
                    .child(
                        div().flex().items_center().child(label(if c.parents.len() == 1 { "Parent" } else { "Parents" })).child(parents),
                    ),
            );
        let n = c.files.len();
        let files_head = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .px_3()
            .border_t_1()
            .border_b_1()
            .border_color(t.border)
            .text_size(px(12.))
            .text_color(t.muted)
            .child(format!("{} changed file{}", fmt_num(n), if n == 1 { "" } else { "s" }))
            .children((c.parents.len() > 1).then(|| badge("vs first parent", t.muted, t.panel2)))
            .child(div().flex_1())
            .children((c.additions > 0).then(|| div().text_color(t.ok).child(format!("+{}", fmt_num(c.additions)))))
            .children((c.deletions > 0).then(|| div().text_color(t.danger).child(format!("−{}", fmt_num(c.deletions)))));
        let open_path = match &self.panel {
            Some(Panel::Diff(d)) => Some(d.file.path.clone()),
            _ => None,
        };
        let mut files = div().flex().flex_col().py_1();
        if c.files.is_empty() {
            files = files.child(div().p_3().text_color(t.muted).text_size(px(12.)).child("This commit changes no files."));
        }
        for (i, f) in c.files.iter().enumerate() {
            let (file, menu_file, commit) = (f.clone(), f.clone(), c.oid.clone());
            files = files.child(
                file_row(("file", i), f, open_path.as_deref() == Some(f.path.as_str()), t)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_commit_diff(&file, cx)))
                    .on_mouse_down(
                        RIGHT,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            let path = menu_file.path.clone();
                            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                            let mut items = this.file_tools(&path, Some(commit.clone()), cx);
                            items.push(MenuItem::sep());
                            items.push(MenuItem::new("copy", "Copy path", move |_, cx| overlay::copy_text(cx, path.clone(), "")));
                            items.push(MenuItem::new("copy", "Copy file name", move |_, cx| overlay::copy_text(cx, name.clone(), "")));
                            overlay::open_menu(cx, e.position, items);
                        }),
                    ),
            );
        }
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("detail-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(head)
                    .child(files_head)
                    .child(files),
            )
            .child(scrollbar(self.detail_scroll.clone(), self.detail_bar.clone(), t))
            .into_any_element()
    }

    pub fn render_detail(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let empty = |name: &'static str, text: &'static str| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(t.muted)
                .child(icon(name, 28., t.dim))
                .child(text)
                .into_any_element()
        };
        match &self.detail {
            Detail::None => empty("commit", "Select a commit to see its changes"),
            Detail::Loading => empty("commit", "Loading…"),
            Detail::Error(e) => {
                div().p_3().flex().gap_2().text_color(t.danger).child(icon("alert", 15., t.danger)).child(e.clone()).into_any_element()
            }
            Detail::Commit(c) => {
                let c = c.clone();
                self.render_commit(&c, t, cx)
            }
            Detail::Wip => self.render_changes(t, window, cx),
        }
    }
}
