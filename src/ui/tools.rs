//! Settings, keyboard shortcuts, command palette, file history, blame, updates.

use super::detail::status_color;
use super::diff_view::{DiffPanel, DiffSource};
use super::highlight::{highlight, lang_of};
use super::input::{InputEvent, TextInput};
use super::overlay::{self, close_modal, modal_frame, MenuItem, Run, ToastKind};
use super::repo_view::{Panel, RepoView};
use super::shell::Shell;
use super::theme::{self, theme, Theme};
use super::widgets::{avatar, btn, checkbox, clip, fmt_date, icon, icon_btn, rel_time, scrollbar, tip, ScrollbarState};
use super::{call, pref, set_pref};
use gpui::prelude::*;
use gpui::{
    actions, div, px, uniform_list, AnyElement, App, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle, KeyBinding,
    ScrollStrategy, SharedString, StyledText, Task, UniformListScrollHandle, Window,
};
use graftline::extras::{Blame, BlameHunk, FileCommit};
use graftline::repo::FileChange;
use graftline::update::Progress;
use graftline::APP_NAME;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------- settings

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Minutes between automatic fetches; 0 turns them off.
    pub auto_fetch: u64,
    pub tab_size: usize,
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { auto_fetch: 0, tab_size: 4, auto_update: true }
    }
}

pub fn settings(cx: &App) -> Settings {
    pref(cx, "gl.settings").unwrap_or_default()
}

struct SettingsDialog {
    focus: FocusHandle,
}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A button that shows the current value and opens a menu of the choices.
fn select<T: Clone + PartialEq + 'static>(
    id: &'static str,
    current: T,
    options: Vec<(T, String)>,
    t: &Theme,
    set: impl Fn(T, &mut App) + 'static,
) -> impl IntoElement {
    let shown = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone()).unwrap_or_default();
    let set = Rc::new(set);
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .px(px(8.))
        .h(px(30.))
        .rounded(px(6.))
        .border_1()
        .border_color(t.border_strong)
        .bg(t.panel)
        .cursor_pointer()
        .child(shown)
        .child(icon("chevron", 13., t.muted))
        .on_click(move |e, _, cx| {
            let items = options
                .iter()
                .map(|(value, label)| {
                    let (set, value, active) = (set.clone(), value.clone(), *value == current);
                    MenuItem::new(if active { "check" } else { "minus" }, label.clone(), move |_, cx| {
                        set(value.clone(), cx);
                        cx.refresh_windows();
                    })
                    .active(active)
                })
                .collect();
            overlay::open_menu(cx, e.position(), items);
        })
}

impl Render for SettingsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let s = settings(cx);
        let label = |text: &'static str| div().text_color(t.muted).text_size(px(12.)).child(text);
        let hint = |text: String| div().text_color(t.dim).text_size(px(12.)).child(text);
        let themes: Vec<(&'static str, String)> = theme::THEMES.iter().map(|(id, name)| (*id, name.to_string())).collect();
        let save = |patch: fn(&mut Settings, u64)| {
            move |v: u64, cx: &mut App| {
                let mut s = settings(cx);
                patch(&mut s, v);
                set_pref(cx, "gl.settings", &s);
            }
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().gap_1().child(label("Theme")).child(select(
                "set-theme",
                theme::chosen(cx).id,
                themes,
                &t,
                |id, cx| {
                    theme::set_theme(cx, id);
                    set_pref(cx, "gl.theme", &id);
                },
            )))
            .child(div().flex().flex_col().gap_1().child(label("Fetch from the remotes automatically")).child(select(
                "set-fetch",
                s.auto_fetch,
                vec![(0, "Never".into()), (5, "Every 5 minutes".into()), (15, "Every 15 minutes".into()), (30, "Every 30 minutes".into())],
                &t,
                save(|s, v| s.auto_fetch = v),
            )))
            .child(hint("A fetch only downloads new commits. It never changes your branches or files.".into()))
            .child(div().flex().flex_col().gap_1().child(label("Tab width in diffs")).child(select(
                "set-tab",
                s.tab_size as u64,
                vec![(2, "2 spaces".into()), (4, "4 spaces".into()), (8, "8 spaces".into())],
                &t,
                save(|s, v| s.tab_size = v as usize),
            )))
            .child(checkbox("set-update", s.auto_update, "Check for updates on start", &t, |on, _, cx| {
                let mut s = settings(cx);
                s.auto_update = on;
                set_pref(cx, "gl.settings", &s);
                cx.refresh_windows();
            }))
            .child(hint(format!(
                "The check asks GitHub for the latest {APP_NAME} release. It sends the app version and nothing about your repositories."
            )));
        let close = btn("settings-close", "Close").on_click(|_, _, cx| close_modal(cx));
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame("Settings", 480., body, vec![close.into_any_element()], cx))
    }
}

pub fn open_settings(cx: &mut App) {
    overlay::open_modal(cx, |_, cx| SettingsDialog { focus: cx.focus_handle() });
}

// ---------------------------------------------------------------- shortcuts

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+P", "Command palette"),
    ("Ctrl+O", "Open a repository"),
    ("Ctrl+W", "Close the tab"),
    ("Ctrl+Tab", "Next tab"),
    ("Ctrl+F", "Search commits"),
    ("Enter / Shift+Enter", "Next / previous search match"),
    ("↑ ↓ PageUp PageDown Home End", "Move in the commit graph"),
    ("F5", "Refresh"),
    ("Ctrl+Shift+F", "Fetch"),
    ("Ctrl+Shift+L", "Pull"),
    ("Ctrl+Shift+P", "Push"),
    ("Ctrl+B", "Create a branch"),
    ("Ctrl+Shift+S", "Stash the changes"),
    ("Ctrl+Shift+C", "Show the uncommitted changes"),
    ("Ctrl+`", "Show or hide the terminal"),
    ("↑ ↓ in the terminal", "Earlier and later commands you typed"),
    ("Ctrl+Enter", "Commit (in the message box)"),
    ("Double click", "Stage or unstage a file"),
    ("Esc", "Close the diff, menu or dialog"),
    ("Ctrl+,", "Settings"),
    ("F1", "Keyboard shortcuts"),
];

struct ShortcutsDialog {
    focus: FocusHandle,
}

impl Focusable for ShortcutsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn kbd(text: String, t: &Theme) -> gpui::Div {
    div()
        .px(px(6.))
        .h(px(20.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(t.panel2)
        .border_1()
        .border_color(t.border_strong)
        .font_family(super::MONO_FONT)
        .text_size(px(11.))
        .child(text)
}

impl Render for ShortcutsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let mac = cfg!(target_os = "macos");
        let mut body = div().flex().flex_col();
        for (keys, what) in SHORTCUTS {
            let keys = if mac { keys.replace("Ctrl", "⌘") } else { keys.to_string() };
            let mut combo = div().flex().items_center().gap_1();
            for (i, part) in keys.split(" / ").enumerate() {
                if i > 0 {
                    combo = combo.child(div().text_color(t.dim).child("/"));
                }
                combo = combo.child(kbd(part.to_string(), &t));
            }
            body = body.child(
                div().flex().items_center().justify_between().h(px(30.)).border_b_1().border_color(t.border).child(*what).child(combo),
            );
        }
        let close = btn("keys-close", "Close").on_click(|_, _, cx| close_modal(cx));
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame(
            "Keyboard shortcuts",
            480.,
            body,
            vec![close.into_any_element()],
            cx,
        ))
    }
}

pub fn open_shortcuts(cx: &mut App) {
    overlay::open_modal(cx, |_, cx| ShortcutsDialog { focus: cx.focus_handle() });
}

// ---------------------------------------------------------------- command palette

actions!(palette, [PaletteDown, PaletteUp]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("down", PaletteDown, Some("Palette")), KeyBinding::new("up", PaletteUp, Some("Palette"))]);
}

struct Command {
    icon: &'static str,
    label: String,
    sub: Option<String>,
    hint: Option<&'static str>,
    run: Run,
}

struct PaletteDialog {
    input: Entity<TextInput>,
    commands: Vec<Command>,
    shown: Vec<usize>,
    active: usize,
    scroll: UniformListScrollHandle,
}

impl Focusable for PaletteDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// Indexes of the commands whose text has every typed word.
fn filter_commands(texts: &[String], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    texts.iter().enumerate().filter(|(_, text)| words.iter().all(|w| text.contains(w.as_str()))).map(|(i, _)| i).take(60).collect()
}

impl PaletteDialog {
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let texts: Vec<String> =
            self.commands.iter().map(|c| format!("{} {}", c.label, c.sub.as_deref().unwrap_or("")).to_lowercase()).collect();
        self.shown = filter_commands(&texts, self.input.read(cx).text());
        self.active = 0;
        cx.notify();
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        self.active = if down { (self.active + 1).min(self.shown.len() - 1) } else { self.active.saturating_sub(1) };
        self.scroll.scroll_to_item(self.active, ScrollStrategy::Center);
        cx.notify();
    }

    fn pick(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self.shown.get(at).map(|i| self.commands[*i].run.clone()) else { return };
        close_modal(cx);
        // The command runs after the palette is gone, so a dialog it opens gets the focus.
        window.defer(cx, move |window, cx| run(window, cx));
    }
}

impl Render for PaletteDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let active = self.active;
        let count = self.shown.len();
        let list = uniform_list(
            "palette",
            count,
            cx.processor(move |this: &mut PaletteDialog, range: Range<usize>, _, cx| {
                let t = theme(cx);
                range
                    .filter_map(|i| {
                        let c = &this.commands[*this.shown.get(i)?];
                        Some(
                            div()
                                .id(("cmd", i))
                                .flex()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .h(px(36.))
                                .px_3()
                                .cursor_pointer()
                                .when(i == active, |el| el.bg(t.row_sel))
                                .hover(|s| s.bg(t.hover))
                                .child(icon(c.icon, 15., if i == active { t.accent } else { t.muted }))
                                .child(div().flex_none().max_w(px(330.)).child(clip(c.label.clone())))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(12.))
                                        .text_color(t.dim)
                                        .child(clip(c.sub.clone().unwrap_or_default())),
                                )
                                .children(c.hint.map(|h| kbd(h.to_string(), &t)))
                                .on_click(cx.listener(move |this, _, window, cx| this.pick(i, window, cx)))
                                .into_any_element(),
                        )
                    })
                    .collect::<Vec<AnyElement>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .h(px((count.min(11) as f32 * 36.).max(36.)));
        div()
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &PaletteDown, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &PaletteUp, _, cx| this.step(false, cx)))
            .w(px(620.))
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(t.elev)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .text_color(t.text)
            .text_size(px(13.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(46.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(icon("search", 15., t.muted))
                    .child(div().flex_1().text_size(px(14.)).child(self.input.clone())),
            )
            .child(if count == 0 {
                div().p_3().text_color(t.muted).text_size(px(12.)).child("No command matches").into_any_element()
            } else {
                list.into_any_element()
            })
    }
}

/// Opens the command palette with the commands that apply now.
pub fn open_palette(shell: &mut Shell, cx: &mut Context<Shell>) {
    let weak = cx.entity().downgrade();
    let mut cmds: Vec<Command> = Vec::new();
    let mut add = |icon: &'static str, label: String, sub: Option<String>, hint: Option<&'static str>, run: Run| {
        cmds.push(Command { icon, label, sub, hint, run })
    };
    let tab = shell.active_tab().cloned();
    if let Some(tab) = &tab {
        let w = tab.downgrade();
        let on_tab = |f: fn(&mut RepoView, &mut Context<RepoView>)| -> Run {
            let w = w.clone();
            Rc::new(move |_, cx| {
                let _ = w.update(cx, f);
            })
        };
        add("fetch", "Fetch all remotes".into(), None, Some("Ctrl+Shift+F"), on_tab(|t, cx| t.fetch_all(false, false, cx)));
        add("fetch", "Fetch all remotes and prune".into(), None, None, on_tab(|t, cx| t.fetch_all(true, false, cx)));
        add("pull", "Pull".into(), None, Some("Ctrl+Shift+L"), on_tab(|t, cx| t.pull(None, cx)));
        add("pull", "Pull with rebase".into(), None, None, on_tab(|t, cx| t.pull(Some("rebase"), cx)));
        add("push", "Push".into(), None, Some("Ctrl+Shift+P"), on_tab(|t, cx| t.push(false, cx)));
        add("push", "Force push (with lease)".into(), None, None, on_tab(|t, cx| t.push(true, cx)));
        add("branch", "Create a branch".into(), None, Some("Ctrl+B"), on_tab(|t, cx| t.create_branch(None, cx)));
        add("tag", "Create a tag".into(), None, None, on_tab(|t, cx| t.create_tag(None, None, cx)));
        add("stash", "Stash the changes".into(), None, Some("Ctrl+Shift+S"), on_tab(|t, cx| t.stash_save(cx)));
        add("cloud", "Add a remote".into(), None, None, on_tab(|t, cx| t.add_remote(cx)));
        add("edit", "Show the uncommitted changes".into(), None, Some("Ctrl+Shift+C"), on_tab(|t, cx| t.show_changes(cx)));
        add("search", "Search commits".into(), None, Some("Ctrl+F"), {
            let w = w.clone();
            Rc::new(move |window, cx| {
                let _ = w.update(cx, |tab, cx| tab.focus_search(window, cx));
            })
        });
        add("refresh", "Refresh".into(), None, Some("F5"), on_tab(|t, cx| t.refresh(true, cx)));
        add("terminal", "Show or hide the terminal".into(), None, Some("Ctrl+`"), {
            let w = w.clone();
            Rc::new(move |window, cx| {
                let _ = w.update(cx, |tab, cx| tab.toggle_terminal(window, cx));
            })
        });
        add("x", "Close this repository".into(), None, Some("Ctrl+W"), {
            let weak = weak.clone();
            Rc::new(move |_, cx| {
                let _ = weak.update(cx, |shell, cx| {
                    if let Some(i) = shell.active {
                        shell.close_tab(i, cx);
                    }
                });
            })
        });
        let on_ref = |name: String, f: fn(&mut RepoView, String, &mut Context<RepoView>)| -> Run {
            let w = w.clone();
            Rc::new(move |_, cx| {
                let _ = w.update(cx, |tab, cx| f(tab, name.clone(), cx));
            })
        };
        if let Some(refs) = &tab.read(cx).refs {
            for b in &refs.branches {
                if !b.current {
                    add(
                        "check",
                        format!("Checkout {}", b.name),
                        Some("local branch".into()),
                        None,
                        on_ref(b.name.clone(), |t, n, cx| t.checkout(n, "branch", cx)),
                    );
                }
                add(
                    "branch",
                    format!("Go to {}", b.name),
                    Some("show the branch in the graph".into()),
                    None,
                    on_ref(b.oid.clone(), |t, o, cx| t.select_oid(&o, cx)),
                );
            }
            for r in &refs.remotes {
                for b in &r.branches {
                    add(
                        "cloud",
                        format!("Checkout {}", b.full),
                        Some("remote branch, as a local tracking branch".into()),
                        None,
                        on_ref(b.full.clone(), |t, n, cx| t.checkout(n, "remote", cx)),
                    );
                }
            }
            for tag in refs.tags.iter().take(200) {
                add("tag", format!("Go to tag {}", tag.name), None, None, on_ref(tag.oid.clone(), |t, o, cx| t.select_oid(&o, cx)));
            }
        }
    }
    let on_shell = |f: fn(&mut Shell, &mut Context<Shell>)| -> Run {
        let weak = weak.clone();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, f);
        })
    };
    add("folder", "Open a repository".into(), None, Some("Ctrl+O"), on_shell(|s, cx| s.pick_and_open(cx)));
    add("download", "Clone a repository".into(), None, None, Rc::new(|_, cx| super::actions::open_clone_dialog(cx)));
    add("home", "Start page".into(), None, None, on_shell(|s, cx| s.activate(None, cx)));
    add("settings", "Settings".into(), None, Some("Ctrl+,"), Rc::new(|_, cx| open_settings(cx)));
    add("keyboard", "Keyboard shortcuts".into(), None, Some("F1"), Rc::new(|_, cx| open_shortcuts(cx)));
    add("download", "Check for updates".into(), None, None, Rc::new(|_, cx| open_updates(cx)));
    for (i, t) in shell.tabs.iter().enumerate() {
        if shell.active != Some(i) {
            let weak = weak.clone();
            let (name, path) = (t.read(cx).name.clone(), t.read(cx).path.clone());
            add(
                "repo",
                format!("Switch to {name}"),
                Some(path),
                None,
                Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |shell, cx| shell.activate(Some(i), cx));
                }),
            );
        }
    }
    let open_paths: Vec<String> = shell.tabs.iter().map(|t| t.read(cx).path.clone()).collect();
    for (r, _) in shell.recent.iter().filter(|(r, exists)| *exists && !open_paths.contains(&r.path)).take(15) {
        let (weak, path) = (weak.clone(), r.path.clone());
        add(
            "repo",
            format!("Open {}", r.name),
            Some(r.path.clone()),
            None,
            Rc::new(move |_, cx| {
                let _ = weak.update(cx, |shell, cx| shell.open_repo(path.clone(), true, false, cx));
            }),
        );
    }
    for (id, name) in theme::THEMES {
        add(
            "palette",
            format!("Theme: {name}"),
            None,
            None,
            Rc::new(move |_, cx| {
                theme::set_theme(cx, id);
                set_pref(cx, "gl.theme", id);
            }),
        );
    }
    overlay::open_modal(cx, move |window, cx: &mut Context<PaletteDialog>| {
        let input = cx.new(|cx| TextInput::new(cx).placeholder("Type a command, branch or repository…"));
        cx.subscribe_in(&input, window, |this: &mut PaletteDialog, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.refilter(cx),
            InputEvent::Enter | InputEvent::Submit => this.pick(this.active, window, cx),
            InputEvent::ShiftEnter => {}
        })
        .detach();
        let shown = (0..cmds.len().min(60)).collect();
        PaletteDialog { input, commands: cmds, shown, active: 0, scroll: UniformListScrollHandle::new() }
    });
}

// ---------------------------------------------------------------- file history

pub struct FileHistoryPanel {
    pub path: String,
    commits: Option<Vec<FileCommit>>,
    more: bool,
    error: Option<String>,
    selected: Option<String>,
    /// The diff of the selected commit.
    pub diff: Option<DiffPanel>,
    scroll: gpui::ScrollHandle,
    bar: ScrollbarState,
}

pub struct BlamePanel {
    pub path: String,
    commit: Option<String>,
    blame: Option<Arc<Blame>>,
    /// For each line: the index of its hunk and whether the line starts the hunk.
    rows: Vec<(usize, bool)>,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    bar: ScrollbarState,
}

/// For each line of a blame: the index of its hunk and whether it is the hunk's first line.
pub fn blame_rows(lines: usize, hunks: &[BlameHunk]) -> Vec<(usize, bool)> {
    let mut rows = vec![(usize::MAX, false); lines];
    for (hi, h) in hunks.iter().enumerate() {
        for k in 0..h.lines {
            if let Some(row) = rows.get_mut(h.start.saturating_sub(1) + k) {
                *row = (hi, k == 0);
            }
        }
    }
    rows
}

fn panel_head(name: &'static str, path: &str, sub: String, t: &Theme, cx: &mut Context<RepoView>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(40.))
        .px_3()
        .border_b_1()
        .border_color(t.border)
        .child(icon(name, 15., t.accent))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(clip(path.to_string()))
                .child(div().flex_none().text_color(t.muted).text_size(px(12.)).child(sub)),
        )
        .child(icon_btn("panel-close", "x").tip("Close (Esc)").on_click(cx.listener(|this, _, window, cx| {
            this.panel = None;
            window.focus(&this.focus);
            cx.notify();
        })))
}

impl RepoView {
    pub fn open_file_history(&mut self, path: String, cx: &mut Context<Self>) {
        self.panel = Some(Panel::FileHistory(FileHistoryPanel {
            path: path.clone(),
            commits: None,
            more: false,
            error: None,
            selected: None,
            diff: None,
            scroll: gpui::ScrollHandle::new(),
            bar: ScrollbarState::default(),
        }));
        cx.notify();
        let id = self.id.clone();
        let p = path.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::extras::file_history(r, &p, 500)).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(Panel::FileHistory(panel)) = &mut this.panel else { return };
                if panel.path != path {
                    return;
                }
                let first = match result {
                    Ok((commits, more)) => {
                        let first = commits.first().map(|c| c.oid.clone());
                        panel.commits = Some(commits);
                        panel.more = more;
                        first
                    }
                    Err(e) => {
                        panel.error = Some(format!("{e:#}"));
                        None
                    }
                };
                if let Some(oid) = first {
                    this.show_file_commit(oid, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_file_commit(&mut self, oid: String, cx: &mut Context<Self>) {
        let Some(Panel::FileHistory(panel)) = &mut self.panel else { return };
        let Ok(id) = git2::Oid::from_str(&oid) else { return };
        let file = FileChange { path: panel.path.clone(), old_path: None, status: 'M', additions: None, deletions: None, binary: false };
        panel.selected = Some(oid.clone());
        panel.diff = Some(DiffPanel::new(file, oid[..7].to_string(), DiffSource::Commit(id)));
        self.load_diff(cx);
        cx.notify();
    }

    pub fn render_file_history(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(Panel::FileHistory(panel)) = &self.panel else { return div().into_any_element() };
        let head = panel_head("history", &panel.path, "file history".into(), t, cx);
        let mut list = div().flex().flex_col();
        match (&panel.error, &panel.commits) {
            (Some(e), _) => list = list.child(div().p_3().text_color(t.danger).child(e.clone())),
            (None, None) => list = list.child(div().p_3().text_color(t.muted).child("Loading…")),
            (None, Some(commits)) if commits.is_empty() => {
                list = list.child(div().p_3().text_color(t.muted).child("No commit on the current branch changed this file."))
            }
            (None, Some(commits)) => {
                for (i, c) in commits.iter().enumerate() {
                    let active = panel.selected.as_deref() == Some(c.oid.as_str());
                    let oid = c.oid.clone();
                    list = list.child(
                        div()
                            .id(("fh", i))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .h(px(44.))
                            .cursor_pointer()
                            .when(active, |el| el.bg(t.row_sel))
                            .when(!active, |el| el.hover(|s| s.bg(t.hover)))
                            .tooltip(tip("Double click to go to this commit in the graph"))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(12.))
                                    .font_family(super::MONO_FONT)
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(status_color(c.status, t))
                                    .child(c.status.to_string()),
                            )
                            .child(avatar(&c.author, &c.email, 22.))
                            .child(div().flex_1().min_w_0().child(clip(c.summary.clone())).child(
                                div().text_size(px(11.)).text_color(t.muted).child(clip(format!(
                                    "{} · {} · {}",
                                    c.author,
                                    rel_time(c.time),
                                    &c.oid[..7.min(c.oid.len())]
                                ))),
                            ))
                            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                                if e.click_count() >= 2 {
                                    this.panel = None;
                                    this.select_oid(&oid, cx);
                                } else {
                                    this.show_file_commit(oid.clone(), cx);
                                }
                            })),
                    );
                }
                if panel.more {
                    list = list.child(div().p_3().text_color(t.muted).text_size(px(12.)).child("Only the newest 500 commits are listed."));
                }
            }
        }
        let (scroll, bar) = (panel.scroll.clone(), panel.bar.clone());
        let diff: AnyElement = if panel.diff.is_some() {
            self.render_diff_body(t, cx)
        } else {
            div().p_4().text_color(t.muted).child("Select a commit to see what it changed in this file.").into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .w(px(340.))
                            .h_full()
                            .border_r_1()
                            .border_color(t.border)
                            .child(div().id("fh-list").size_full().overflow_y_scroll().track_scroll(&scroll).child(list))
                            .child(scrollbar(scroll, bar, t)),
                    )
                    .child(div().flex_1().min_w_0().h_full().child(diff)),
            )
            .into_any_element()
    }

    pub fn open_blame(&mut self, path: String, commit: Option<String>, cx: &mut Context<Self>) {
        self.panel = Some(Panel::Blame(BlamePanel {
            path: path.clone(),
            commit: commit.clone(),
            blame: None,
            rows: Vec::new(),
            error: None,
            scroll: UniformListScrollHandle::new(),
            bar: ScrollbarState::default(),
        }));
        cx.notify();
        let id = self.id.clone();
        let p = path.clone();
        let task = call(cx, move |st| async move { st.with_repo(&id, move |r| graftline::extras::blame(r, &p, commit.as_deref())).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(Panel::Blame(panel)) = &mut this.panel else { return };
                if panel.path != path {
                    return;
                }
                match result {
                    Ok(b) => {
                        panel.rows = blame_rows(b.lines.len(), &b.hunks);
                        panel.blame = Some(Arc::new(b));
                    }
                    Err(e) => panel.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_blame_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = theme(cx);
        let Some(Panel::Blame(panel)) = &self.panel else { return Vec::new() };
        let Some(b) = panel.blame.clone() else { return Vec::new() };
        let lang = lang_of(&panel.path);
        let tab = " ".repeat(settings(cx).tab_size);
        let mut out = Vec::with_capacity(range.len());
        for i in range {
            let Some((hi, first)) = panel.rows.get(i).copied() else { break };
            let hunk = b.hunks.get(hi);
            let text = b.lines[i].replace('\t', &tab);
            let spans: Vec<(Range<usize>, HighlightStyle)> = match lang {
                Some(lang) => highlight(&text, lang)
                    .into_iter()
                    .map(|(r, tok)| (r, HighlightStyle { color: Some(tok.color(&t)), ..Default::default() }))
                    .collect(),
                None => Vec::new(),
            };
            let mut info = div()
                .id(("bl", i))
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .w(px(250.))
                .h_full()
                .px_2()
                .text_size(px(11.))
                .overflow_hidden();
            if let Some(h) = hunk {
                let oid = h.oid.clone();
                info = info
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .tooltip(tip(format!(
                        "{}  {}  {}\n{}\n\nClick to go to this commit",
                        &h.oid[..10.min(h.oid.len())],
                        h.author,
                        fmt_date(h.time),
                        h.summary
                    )))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panel = None;
                        this.select_oid(&oid, cx);
                    }));
                if first {
                    info = info
                        .child(
                            div()
                                .flex_none()
                                .font_family(super::MONO_FONT)
                                .text_color(t.accent)
                                .child(h.oid[..7.min(h.oid.len())].to_string()),
                        )
                        .child(div().flex_1().min_w_0().text_color(t.muted).child(clip(h.author.clone())))
                        .child(div().flex_none().text_color(t.dim).child(rel_time(h.time)));
                }
            }
            out.push(
                div()
                    .flex()
                    .items_center()
                    .h(px(20.))
                    .font_family(super::MONO_FONT)
                    .text_size(px(12.5))
                    .whitespace_nowrap()
                    .when(hi % 2 == 1, |el| el.bg(t.panel))
                    .when(first && i > 0, |el| el.border_t_1().border_color(t.border))
                    .child(info.font_family(super::UI_FONT))
                    .child(
                        div()
                            .flex_none()
                            .w(px(46.))
                            .pr_2()
                            .flex()
                            .justify_end()
                            .text_color(t.dim)
                            .text_size(px(11.))
                            .child((i + 1).to_string()),
                    )
                    .child(if text.is_empty() {
                        div().into_any_element()
                    } else {
                        StyledText::new(SharedString::from(text)).with_highlights(spans).into_any_element()
                    })
                    .into_any_element(),
            );
        }
        out
    }

    pub fn render_blame(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(Panel::Blame(panel)) = &self.panel else { return div().into_any_element() };
        let sub = match &panel.commit {
            Some(c) => format!("blame at {}", &c[..7.min(c.len())]),
            None => "blame".into(),
        };
        let head = panel_head("user", &panel.path, sub, t, cx);
        let body: AnyElement = match (&panel.error, &panel.blame) {
            (Some(e), _) => {
                div().p_4().flex().gap_2().text_color(t.danger).child(icon("alert", 15., t.danger)).child(e.clone()).into_any_element()
            }
            (None, None) => div().p_4().text_color(t.muted).child("Loading…").into_any_element(),
            (None, Some(b)) => {
                let handle = panel.scroll.0.borrow().base_handle.clone();
                let list = uniform_list(
                    "blame",
                    b.lines.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| this.render_blame_rows(range, cx)),
                )
                .track_scroll(panel.scroll.clone())
                .size_full();
                div().relative().size_full().child(list).child(scrollbar(handle, panel.bar.clone(), t)).into_any_element()
            }
        };
        div().size_full().flex().flex_col().child(head).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }
}

// ---------------------------------------------------------------- updates

fn set_update_info(info: Value, cx: &mut App) {
    let skipped: Option<String> = pref(cx, "gl.skipVersion");
    let latest = info["latest"].as_str().map(str::to_string);
    let available = info["available"] == true && latest != skipped;
    overlay::with_shell(cx, move |shell, _, cx| {
        shell.update = latest.filter(|_| available);
        cx.notify();
    });
}

/// Checks for a new release a few seconds after the window opens, when the settings allow it.
pub fn check_updates_on_start(cx: &mut Context<Shell>) {
    cx.spawn(async move |_, cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        let Ok(Some(task)) = cx.update(|cx| settings(cx).auto_update.then(|| call(cx, |st| async move { st.updater.check(false).await })))
        else {
            return;
        };
        let Ok(info) = task.await else { return };
        let _ = cx.update(|cx| {
            let latest = info["latest"].as_str().unwrap_or("").to_string();
            let skipped: Option<String> = pref(cx, "gl.skipVersion");
            let fresh = info["available"] == true
                && skipped.as_deref() != Some(latest.as_str())
                && pref::<String>(cx, "gl.updateToasted").as_deref() != Some(latest.as_str());
            set_update_info(info, cx);
            if fresh {
                set_pref(cx, "gl.updateToasted", &latest);
                overlay::toast_action(cx, format!("{APP_NAME} {latest} is available"), ToastKind::Info, "Details", |_, cx| {
                    open_updates(cx)
                });
            }
        });
    })
    .detach();
}

struct UpdateDialog {
    info: Option<Value>,
    progress: Option<Progress>,
    error: Option<String>,
    poll: Option<Task<()>>,
    focus: FocusHandle,
}

impl Focusable for UpdateDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.)
}

impl UpdateDialog {
    fn load(&mut self, force: bool, cx: &mut Context<Self>) {
        self.info = None;
        self.error = None;
        cx.notify();
        let task = call(cx, move |st| async move {
            let info = st.updater.check(force).await?;
            Ok((info, st.updater.progress().await))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((info, progress)) => {
                        let same = progress.version.as_deref() == info["latest"].as_str();
                        let downloading = same && progress.phase == "downloading";
                        this.progress = same.then_some(progress);
                        set_update_info(info.clone(), cx);
                        this.info = Some(info);
                        if downloading {
                            this.start_poll(cx);
                        }
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_poll(&mut self, cx: &mut Context<Self>) {
        self.poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let Ok(task) = cx.update(|cx| call(cx, |st| async move { Ok(st.updater.progress().await) })) else { break };
            let Ok(p) = task.await else { break };
            let going = p.phase == "downloading";
            if this
                .update(cx, |this, cx| {
                    this.progress = Some(p);
                    cx.notify();
                })
                .is_err()
                || !going
            {
                break;
            }
        }));
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let task = call(cx, |st| async move {
            st.updater.download().await?;
            Ok(st.updater.progress().await)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(p) => {
                        this.progress = Some(p);
                        this.error = None;
                        this.start_poll(cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Hands over to the updater and exits; the new version starts by itself.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.progress = Some(Progress { phase: "restarting".into(), ..Default::default() });
        cx.notify();
        let task = call(cx, |st| async move { st.updater.restart().await });
        cx.spawn(async move |this, cx| match task.await {
            Ok(()) => {
                tracing::info!("exiting for the update");
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let _ = cx.update(|cx| cx.quit());
            }
            Err(e) => {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("{e:#}"));
                    this.progress = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for UpdateDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let version = env!("CARGO_PKG_VERSION");
        let mut body = div().flex().flex_col().gap_3();
        let mut foot: Vec<AnyElement> = Vec::new();
        let head = |name: &'static str, title: String, sub: String| {
            div().flex().items_center().gap_3().child(icon(name, 18., t.accent)).child(
                div()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_size(px(12.)).text_color(t.muted).child(sub)),
            )
        };
        let phase = self.progress.as_ref().map(|p| p.phase.as_str()).unwrap_or("idle");
        match &self.info {
            None => {
                body = body.child(match &self.error {
                    Some(e) => div().text_color(t.danger).child(e.clone()),
                    None => div().text_color(t.muted).child("Checking for updates…"),
                });
            }
            Some(u) if u["available"] != true => {
                body = body.child(head(
                    "check",
                    format!("{APP_NAME} is up to date"),
                    format!("Version {version}. Latest release: {}.", u["latest"].as_str().unwrap_or("?")),
                ));
            }
            Some(u) => {
                let latest = u["latest"].as_str().unwrap_or("?").to_string();
                let size = u["size"].as_u64().map(|n| format!(" Download: {}.", mb(n))).unwrap_or_default();
                body = body.child(head("download", format!("{APP_NAME} {latest} is available"), format!("You have {version}.{size}")));
                if let Some(notes) = u["release"]["notes"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
                    body = body.child(div().id("update-notes").max_h(px(260.)).overflow_y_scroll().child(overlay::code_block(notes, &t)));
                }
                if let Some(p) = self.progress.as_ref().filter(|p| p.phase == "downloading") {
                    let ratio = if p.total > 0 { (p.received as f32 / p.total as f32).min(1.) } else { 0. };
                    let of = if p.total > 0 { format!(" of {}", mb(p.total)) } else { String::new() };
                    body = body
                        .child(
                            div()
                                .h(px(4.))
                                .rounded(px(2.))
                                .bg(t.panel2)
                                .child(div().h_full().rounded(px(2.)).bg(t.accent).w(gpui::relative(ratio))),
                        )
                        .child(div().text_size(px(12.)).text_color(t.muted).child(format!("Downloading… {}{of}", mb(p.received))));
                }
                if phase == "ready" {
                    let text = if u["method"] == "msi" {
                        format!("Downloaded and verified. {APP_NAME} closes, Windows asks for permission once, installs the update and starts {APP_NAME} again.")
                    } else {
                        format!("Downloaded and verified. {APP_NAME} restarts with the new version.")
                    };
                    body = body.child(div().text_color(t.ok).child(text));
                }
                if phase == "restarting" {
                    body = body.child(div().text_color(t.muted).child("Restarting…"));
                }
                let failure =
                    self.error.clone().or_else(|| self.progress.as_ref().filter(|p| p.phase == "error").and_then(|p| p.error.clone()));
                if let Some(e) = failure {
                    body = body.child(div().text_color(t.danger).child(e));
                }
                let can_install = u["canInstall"] == true;
                if !can_install {
                    body = body.child(div().text_size(px(12.)).text_color(t.dim).child(
                        "This install cannot update itself (for example, a system package). Download the new version from the release page.",
                    ));
                }
                let url = u["release"]["url"].as_str().unwrap_or("https://github.com/byht1/graftline/releases").to_string();
                foot.push(
                    btn("upd-page", "Release page").icon("external").ghost().on_click(move |_, _, cx| cx.open_url(&url)).into_any_element(),
                );
                foot.push(
                    btn("upd-skip", "Skip this version")
                        .ghost()
                        .on_click(move |_, _, cx| {
                            set_pref(cx, "gl.skipVersion", &latest);
                            overlay::with_shell(cx, |shell, _, cx| {
                                shell.update = None;
                                cx.notify();
                            });
                            close_modal(cx);
                        })
                        .into_any_element(),
                );
                if can_install {
                    foot.push(match phase {
                        "ready" => btn("upd-restart", "Restart and update")
                            .icon("refresh")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.restart(cx)))
                            .into_any_element(),
                        "downloading" => btn("upd-busy", "Downloading…").primary().disabled(true).into_any_element(),
                        "restarting" => btn("upd-busy", "Restarting…").primary().disabled(true).into_any_element(),
                        other => btn("upd-get", if other == "error" { "Try again" } else { "Download and install" })
                            .icon("download")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.download(cx)))
                            .into_any_element(),
                    });
                }
            }
        }
        if self.info.as_ref().is_none_or(|u| u["available"] != true) {
            foot.push(
                btn("upd-check", "Check now")
                    .icon("refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.load(true, cx)))
                    .into_any_element(),
            );
        }
        foot.push(btn("upd-close", "Close").on_click(|_, _, cx| close_modal(cx)).into_any_element());
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame("Updates", 620., body, foot, cx))
    }
}

pub fn open_updates(cx: &mut App) {
    overlay::open_modal(cx, |_, cx: &mut Context<UpdateDialog>| {
        let mut dialog = UpdateDialog { info: None, progress: None, error: None, poll: None, focus: cx.focus_handle() };
        dialog.load(false, cx);
        dialog
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_needs_every_word() {
        let texts: Vec<String> = ["fetch all remotes", "checkout main local branch", "theme: nord"].iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_commands(&texts, ""), vec![0, 1, 2]);
        assert_eq!(filter_commands(&texts, "MAIN check"), vec![1]);
        assert!(filter_commands(&texts, "main nord").is_empty());
    }

    #[test]
    fn blame_lines_know_their_hunk() {
        let hunk = |start, lines| BlameHunk { start, lines, oid: String::new(), summary: String::new(), author: String::new(), time: 0 };
        let rows = blame_rows(5, &[hunk(1, 2), hunk(3, 3)]);
        assert_eq!(rows, vec![(0, true), (0, false), (1, true), (1, false), (1, false)]);
    }
}
