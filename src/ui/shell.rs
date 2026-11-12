//! The window: sidebar, tab bar, start page, and the layer of menus, dialogs and notifications.

use super::input::{field, InputEvent, TextInput};
use super::overlay::{self, Menu, MenuItem, MenuKind, ShellRef, Toast, ToastKind};
use super::repo_view::RepoView;
use super::theme::{self, theme, Theme};
use super::widgets::{badge, btn, clip, icon, icon_btn, rel_time, splitter, tip, LEFT, RIGHT};
use super::{call, core, pref, set_pref};
use gpui::prelude::*;
use gpui::{
    actions, anchored, deferred, div, img, point, px, size, AnyElement, AnyView, App, Bounds, Corner, Entity, ExternalPaths, FocusHandle,
    FontWeight, KeyBinding, MouseButton, MouseMoveEvent, PathPromptOptions, Pixels, Point, TitlebarOptions, Window, WindowAppearance,
    WindowBounds, WindowOptions,
};
use graftline::{Recent, APP_ID, APP_NAME};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

actions!(
    graftline,
    [
        Cancel,
        Confirm,
        OpenRepo,
        CloseTab,
        NextTab,
        PrevTab,
        Palette,
        OpenSettings,
        Shortcuts,
        Fetch,
        Pull,
        Push,
        StashSave,
        ShowChanges,
        NewBranch,
        Search,
        Refresh,
    ]
);

fn bind_keys(cx: &mut App) {
    super::input::bind_keys(cx);
    super::graph_view::bind_keys(cx);
    super::tools::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, None),
        KeyBinding::new("enter", Confirm, Some("Dialog")),
        KeyBinding::new("secondary-o", OpenRepo, None),
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("secondary-p", Palette, None),
        KeyBinding::new("secondary-k", Palette, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("f1", Shortcuts, None),
        KeyBinding::new("secondary-shift-f", Fetch, None),
        KeyBinding::new("secondary-shift-l", Pull, None),
        KeyBinding::new("secondary-shift-p", Push, None),
        KeyBinding::new("secondary-shift-s", StashSave, None),
        KeyBinding::new("secondary-shift-c", ShowChanges, None),
        KeyBinding::new("secondary-b", NewBranch, None),
        KeyBinding::new("secondary-f", Search, None),
        KeyBinding::new("f5", Refresh, None),
    ]);
}

/// Size, position and maximized state of the window, restored on the next start.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct WindowState {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    maximized: bool,
}

fn window_state_file(cx: &App) -> PathBuf {
    core(cx).data_dir.join("window.json")
}

fn load_window_state(path: &Path) -> Option<WindowState> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok().filter(|s: &WindowState| s.width >= 400. && s.height >= 300.)
}

pub fn open_window(cx: &mut App, open: Vec<PathBuf>) {
    bind_keys(cx);
    let saved = load_window_state(&window_state_file(cx));
    let bounds = match saved {
        Some(s) => {
            let b = Bounds::new(point(px(s.x), px(s.y)), size(px(s.width), px(s.height)));
            if s.maximized {
                WindowBounds::Maximized(b)
            } else {
                WindowBounds::Windowed(b)
            }
        }
        None => WindowBounds::Windowed(Bounds::centered(None, size(px(1400.), px(880.)), cx)),
    };
    let options = WindowOptions {
        window_bounds: Some(bounds),
        titlebar: Some(TitlebarOptions { title: Some(APP_NAME.into()), appears_transparent: false, traffic_light_position: None }),
        window_min_size: Some(size(px(760.), px(480.))),
        app_id: Some(APP_ID.into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        let saved: Option<String> = pref(cx, "gl.theme");
        theme::init(cx, saved.as_deref(), dark);
        cx.new(|cx| Shell::new(window, cx, open))
    });
    if let Err(e) = opened {
        tracing::error!("cannot create the app window: {e:#}");
        cx.quit();
        return;
    }
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    cx.activate(true);
}

enum Resize {
    Sidebar { x: Pixels, width: f32 },
}

pub struct Shell {
    pub focus: FocusHandle,
    pub tabs: Vec<Entity<RepoView>>,
    /// Index into `tabs`; `None` is the start page.
    pub active: Option<usize>,
    pub recent: Vec<(Recent, bool)>,
    home_filter: Entity<TextInput>,
    pub sb_filter: Entity<TextInput>,
    sidebar_w: f32,
    resize: Option<Resize>,
    pub menu: Option<Menu>,
    pub modals: Vec<(AnyView, FocusHandle)>,
    pub toasts: Vec<Toast>,
    pub next_toast: u64,
    /// The latest release, when it is newer than this build.
    pub update: Option<String>,
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>, open: Vec<PathBuf>) -> Self {
        cx.set_global(ShellRef { shell: cx.entity().downgrade(), window: window.window_handle() });
        let home_filter = cx.new(|cx| TextInput::new(cx).placeholder("Filter recent repositories…"));
        let sb_filter = cx.new(|cx| TextInput::new(cx).placeholder("Filter branches and tags…"));
        cx.subscribe(&home_filter, |this: &mut Shell, input, event: &InputEvent, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::ShiftEnter => {}
            InputEvent::Submit | InputEvent::Enter => {
                let q = input.read(cx).text().trim().to_lowercase();
                if let Some((r, _)) =
                    this.recent.iter().find(|(r, _)| q.is_empty() || format!("{} {}", r.name, r.path).to_lowercase().contains(&q))
                {
                    this.open_repo(r.path.clone(), true, false, cx);
                }
            }
        })
        .detach();
        cx.subscribe(&sb_filter, |_, _, _: &InputEvent, cx| cx.notify()).detach();

        // Core events: a repository changed, a job made progress.
        let mut events = core(cx).events.subscribe();
        cx.spawn(async move |this, cx| loop {
            match events.recv().await {
                Ok(text) => {
                    if this.update(cx, |this, cx| this.on_event(&text, cx)).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        })
        .detach();

        cx.observe_window_bounds(window, |_, window, cx| {
            let (b, maximized) = match window.window_bounds() {
                WindowBounds::Windowed(b) => (b, false),
                WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
            };
            let state = WindowState {
                x: b.origin.x.into(),
                y: b.origin.y.into(),
                width: b.size.width.into(),
                height: b.size.height.into(),
                maximized,
            };
            if let Ok(json) = serde_json::to_string(&state) {
                let _ = std::fs::write(window_state_file(cx), json);
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus);
        let this = Shell {
            focus,
            tabs: Vec::new(),
            active: None,
            recent: Vec::new(),
            home_filter,
            sb_filter,
            sidebar_w: pref(cx, "gl.sidebarW").unwrap_or(248.),
            resize: None,
            menu: None,
            modals: Vec::new(),
            toasts: Vec::new(),
            next_toast: 0,
            update: None,
        };
        this.load_recent(cx);

        // Tabs of the last session, then repositories from the command line.
        let saved: Vec<String> = pref(cx, "gl.tabs").unwrap_or_default();
        let wanted: Option<String> = pref(cx, "gl.active").flatten();
        cx.spawn(async move |this, cx| {
            for path in saved {
                let task = this.update(cx, |this, cx| this.open_repo_task(path, false, true, cx));
                if let Ok(task) = task {
                    task.await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if open.is_empty() {
                    if let Some(i) = wanted.and_then(|w| this.tabs.iter().position(|t| t.read(cx).path == w)) {
                        this.activate(Some(i), cx);
                    }
                }
            });
            for path in open {
                // Relative paths refer to the directory the app was started from.
                let path = std::path::absolute(&path).unwrap_or(path);
                let task = this.update(cx, |this, cx| this.open_repo_task(path.display().to_string(), true, false, cx));
                if let Ok(task) = task {
                    task.await;
                }
            }
        })
        .detach();
        super::tools::check_updates_on_start(cx);
        #[cfg(debug_assertions)]
        super::driver::start(window, cx);
        this
    }

    fn on_event(&mut self, text: &str, cx: &mut Context<Self>) {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(text) else { return };
        match ev["type"].as_str() {
            Some("repo") => {
                let id = ev["id"].as_str().unwrap_or("");
                if let Some(tab) = self.tabs.iter().find(|t| t.read(cx).id == id) {
                    let (git, tree, graph) = (ev["git"] == true, ev["tree"] == true, ev["graph"] == true);
                    tab.update(cx, |tab, cx| tab.on_repo_event(git, tree, graph, cx));
                }
            }
            Some("job") => {
                let job = &ev["job"];
                let repo = job["repo"].as_str().unwrap_or("");
                if let Some(tab) = self.tabs.iter().find(|t| t.read(cx).id == repo) {
                    tab.update(cx, |tab, cx| tab.on_job(job, cx));
                }
                super::actions::on_job_event(job, cx);
            }
            _ => {}
        }
    }

    pub fn active_tab(&self) -> Option<&Entity<RepoView>> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    fn save_tabs(&self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.tabs.iter().map(|t| t.read(cx).path.clone()).collect();
        let active: Option<String> = self.active_tab().map(|t| t.read(cx).path.clone());
        set_pref(cx, "gl.tabs", &paths);
        set_pref(cx, "gl.active", &active);
    }

    pub fn load_recent(&self, cx: &mut Context<Self>) {
        let task = call(cx, |st| async move { Ok(st.recent().await) });
        cx.spawn(async move |this, cx| {
            if let Ok(recent) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.recent = recent;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub fn activate(&mut self, tab: Option<usize>, cx: &mut Context<Self>) {
        self.active = tab.filter(|i| *i < self.tabs.len());
        self.save_tabs(cx);
        cx.notify();
    }

    pub fn open_repo(&mut self, path: String, activate: bool, quiet: bool, cx: &mut Context<Self>) {
        self.open_repo_task(path, activate, quiet, cx).detach();
    }

    fn open_repo_task(&mut self, path: String, activate: bool, quiet: bool, cx: &mut Context<Self>) -> gpui::Task<()> {
        let task = call(cx, move |st| async move { st.open_repo(&path).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(session) => {
                    let at = match this.tabs.iter().position(|t| t.read(cx).id == session.id) {
                        Some(i) => i,
                        None => {
                            let (id, path, name) = (session.id.clone(), session.root.display().to_string(), session.name.clone());
                            this.tabs.push(cx.new(|cx| RepoView::new(id, path, name, cx)));
                            this.tabs.len() - 1
                        }
                    };
                    if activate {
                        this.active = Some(at);
                    }
                    this.save_tabs(cx);
                    this.load_recent(cx);
                    cx.notify();
                }
                Err(e) => {
                    if quiet {
                        this.save_tabs(cx);
                    } else {
                        overlay::error(cx, format!("{e:#}"));
                    }
                }
            });
        })
    }

    /// The native "choose a folder" dialog, then the repository opens.
    pub fn pick_and_open(&mut self, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |this, cx| {
                    for p in paths {
                        this.open_repo(p.display().to_string(), true, false, cx);
                    }
                });
            }
        })
        .detach();
    }

    pub fn close_tab(&mut self, at: usize, cx: &mut Context<Self>) {
        if at >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(at);
        let id = tab.read(cx).id.clone();
        let task = call(cx, move |st| async move {
            st.close_repo(&id).await;
            Ok(())
        });
        cx.background_executor().spawn(task).detach();
        self.active = match self.active {
            Some(i) if i == at => {
                if at < self.tabs.len() {
                    Some(at)
                } else {
                    at.checked_sub(1)
                }
            }
            Some(i) if i > at => Some(i - 1),
            other => other,
        };
        self.save_tabs(cx);
        cx.notify();
    }

    pub fn pop_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modals.pop();
        match self.modals.last() {
            Some((_, focus)) => window.focus(focus),
            None => window.focus(&self.focus),
        }
        cx.notify();
    }

    fn close_menu(&mut self, chosen: bool, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu.take() {
            if !chosen {
                if let Some(f) = menu.on_dismiss {
                    f(cx);
                }
            }
            cx.notify();
        }
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(false, cx);
        } else if !self.modals.is_empty() {
            self.pop_modal(window, cx);
        } else if let Some(tab) = self.active_tab().cloned() {
            let handled = tab.update(cx, |tab, cx| tab.escape(window, cx));
            if !handled {
                window.focus(&self.focus);
            }
        }
    }

    fn step_tab(&mut self, back: bool, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        // The start page is position 0, the tabs follow.
        let n = self.tabs.len() + 1;
        let cur = self.active.map(|i| i + 1).unwrap_or(0);
        let next = (cur + if back { n - 1 } else { 1 }) % n;
        self.activate(next.checked_sub(1), cx);
    }

    /// Runs `f` with the repository in view; does nothing on the start page.
    fn with_tab(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut RepoView, &mut Context<RepoView>)) {
        if let Some(tab) = self.active_tab().cloned() {
            tab.update(cx, f);
        }
    }

    // ------------------------------------------------------------ menus

    fn open_repo_menu(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let mut items = vec![
            MenuItem::new("folder", "Open repository…", {
                let this = this.clone();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.pick_and_open(cx));
                }
            })
            .hint("Ctrl+O"),
            MenuItem::new("download", "Clone repository…", |_, cx| super::actions::open_clone_dialog(cx)),
            MenuItem::new("home", "Start page", {
                let this = this.clone();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.activate(None, cx));
                }
            }),
        ];
        let current = self.active_tab().map(|t| t.read(cx).path.clone());
        let recent: Vec<&Recent> = self.recent.iter().filter(|(_, exists)| *exists).map(|(r, _)| r).take(12).collect();
        if !recent.is_empty() {
            items.push(MenuItem::sep());
            items.push(MenuItem::title("Recent"));
            for r in recent {
                let (this, path) = (this.clone(), r.path.clone());
                items.push(
                    MenuItem::new("repo", r.name.clone(), move |_, cx| {
                        let _ = this.update(cx, |this, cx| this.open_repo(path.clone(), true, false, cx));
                    })
                    .sub(r.path.clone())
                    .active(current.as_deref() == Some(r.path.as_str())),
                );
            }
        }
        self.menu = Some(Menu { pos, items, on_dismiss: None, upward: false });
        cx.notify();
    }

    fn open_theme_menu(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let chosen = theme::chosen(cx).id;
        let all = cx.global::<theme::ThemeState>().all.clone();
        let items = all
            .iter()
            .map(|t| {
                let id = t.id;
                let mut item = MenuItem::new(if id == chosen { "check" } else { "palette" }, t.name, move |_, cx| {
                    theme::set_theme(cx, id);
                    set_pref(cx, "gl.theme", &id);
                })
                .active(id == chosen);
                item.swatch = Some([t.bg, t.accent, t.graph[1], t.graph[2]]);
                // Live preview while the pointer is on the item; the click keeps it.
                item.hover = Some(std::rc::Rc::new(move |cx| theme::set_preview(cx, Some(id))));
                item
            })
            .collect();
        self.menu = Some(Menu { pos, items, on_dismiss: Some(std::rc::Rc::new(|cx| theme::set_preview(cx, None))), upward: true });
        cx.notify();
    }

    // ------------------------------------------------------------ parts

    fn render_tab_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let home_active = self.active.is_none();
        let mut bar = div()
            .id("tabbar")
            .flex()
            .flex_none()
            .items_end()
            .h(px(38.))
            .px_1()
            .gap(px(2.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .overflow_x_scroll();
        bar = bar.child(
            div()
                .id("tab-home")
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(px(36.))
                .h(px(32.))
                .rounded_t(px(6.))
                .cursor_pointer()
                .when(home_active, |el| el.bg(t.bg))
                .hover(|s| s.bg(t.hover))
                .tooltip(tip("Start page"))
                .child(icon("home", 15., if home_active { t.accent } else { t.muted }))
                .on_click(cx.listener(|this, _, _, cx| this.activate(None, cx))),
        );
        for (i, tab) in self.tabs.iter().enumerate() {
            let (name, path) = {
                let tab = tab.read(cx);
                (tab.name.clone(), tab.path.clone())
            };
            let active = self.active == Some(i);
            let menu_path = path.clone();
            bar = bar.child(
                div()
                    .id(("tab", i))
                    .group("tab")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .max_w(px(220.))
                    .h(px(32.))
                    .pl(px(10.))
                    .pr(px(4.))
                    .rounded_t(px(6.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active { t.text } else { t.muted })
                    .when(active, |el| el.bg(t.bg).border_t_2().border_color(t.accent))
                    .hover(|s| s.bg(t.hover))
                    .tooltip(tip(path))
                    .child(icon("repo", 13., if active { t.accent } else { t.dim }))
                    .child(clip(name))
                    .child(
                        icon_btn(("tab-close", i), "x")
                            .icon_size(12.)
                            .tip("Close (Ctrl+W)")
                            .on_click(cx.listener(move |this, _, _, cx| this.close_tab(i, cx))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(Some(i), cx)))
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, _, cx| this.close_tab(i, cx)))
                    .on_mouse_down(
                        RIGHT,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                            let weak = cx.entity().downgrade();
                            let (w1, w2, path) = (weak.clone(), weak, menu_path.clone());
                            let others = this.tabs.len() > 1;
                            let items = vec![
                                MenuItem::new("x", "Close", move |_, cx| {
                                    let _ = w1.update(cx, |this, cx| this.close_tab(i, cx));
                                })
                                .hint("Ctrl+W"),
                                MenuItem::new("x", "Close other tabs", move |_, cx| {
                                    let _ = w2.update(cx, |this, cx| {
                                        let keep = this.tabs.get(i).cloned();
                                        for at in (0..this.tabs.len()).rev() {
                                            if Some(&this.tabs[at]) != keep.as_ref() {
                                                this.close_tab(at, cx);
                                            }
                                        }
                                    });
                                })
                                .disabled(!others),
                                MenuItem::sep(),
                                MenuItem::new("copy", "Copy path", move |_, cx| overlay::copy_text(cx, path.clone(), "")),
                            ];
                            this.menu = Some(Menu { pos: e.position, items, on_dismiss: None, upward: false });
                            cx.notify();
                        }),
                    ),
            );
        }
        bar.child(div().flex().items_center().h(px(32.)).child(
            icon_btn("tab-add", "plus").tip("Open a repository (Ctrl+O)").on_click(cx.listener(|this, _, _, cx| this.pick_and_open(cx))),
        ))
    }

    fn render_home(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.home_filter.read(cx).text().trim().to_lowercase();
        let list: Vec<&(Recent, bool)> =
            self.recent.iter().filter(|(r, _)| q.is_empty() || format!("{} {}", r.name, r.path).to_lowercase().contains(&q)).collect();
        let has_git = core(cx).git.is_some();
        let mut grid = div().flex().flex_wrap().gap_3();
        if list.is_empty() && !self.recent.is_empty() {
            grid = grid.child(div().w_full().text_color(t.muted).child("No repositories match."));
        }
        for (i, (r, exists)) in list.iter().enumerate() {
            let (path, forget) = (r.path.clone(), r.path.clone());
            let open = self.tabs.iter().any(|tab| tab.read(cx).path == r.path);
            let title = if *exists { r.path.clone() } else { format!("{}\nThis folder no longer exists", r.path) };
            grid = grid.child(
                div()
                    .id(("recent", i))
                    .w(px(280.))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.panel)
                    .cursor_pointer()
                    .when(!*exists, |el| el.opacity(0.6))
                    .hover(|s| s.border_color(t.accent).bg(t.panel2))
                    .tooltip(tip(title))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon("repo", 15., t.accent))
                            .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).child(clip(r.name.clone())))
                            .child(icon_btn(("recent-x", i), "x").icon_size(13.).tip("Remove from this list").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    let path = forget.clone();
                                    let task = call(cx, move |st| async move { st.forget_recent(&path).await });
                                    cx.spawn(async move |this, cx| {
                                        let _ = task.await;
                                        let _ = this.update(cx, |this, cx| this.load_recent(cx));
                                    })
                                    .detach();
                                },
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(12.))
                            .text_color(t.muted)
                            .child(if r.opened_at > 0 { format!("opened {}", rel_time(r.opened_at as i64)) } else { String::new() })
                            .children(open.then(|| badge("open", t.accent, t.accent_soft)))
                            .children((!*exists).then(|| badge("missing", t.text, t.mod_bg))),
                    )
                    .child(div().font_family(super::MONO_FONT).text_size(px(11.)).text_color(t.dim).child(clip(r.path.clone())))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_repo(path.clone(), true, false, cx))),
            );
        }
        grid = grid.child(
            div()
                .id("recent-add")
                .w(px(280.))
                .min_h(px(84.))
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(t.border_strong)
                .text_color(t.muted)
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent).text_color(t.text))
                .child(icon("plus", 18., t.muted))
                .child("Open a repository")
                .on_click(cx.listener(|this, _, _, cx| this.pick_and_open(cx))),
        );
        let notice = (!has_git).then(|| {
            div()
                .flex()
                .gap_2()
                .p_2()
                .rounded(px(6.))
                .border_1()
                .border_color(t.mod_border)
                .bg(t.mod_bg)
                .child(icon("alert", 15., t.text))
                .child(div().flex_1().child(
                    "Git was not found on PATH. You can browse repositories; fetch, pull, push and history changes need Git. Install Git from https://git-scm.com/downloads, then restart the app.",
                ))
        });
        div().id("home").size_full().overflow_y_scroll().bg(t.bg).child(
            div()
                .max_w(px(960.))
                .mx_auto()
                .p_8()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div().flex().items_center().gap_3().child(img("brand/symbol.svg").size(px(48.))).child(
                        div()
                            .child(div().text_size(px(26.)).font_weight(FontWeight::BOLD).child(APP_NAME))
                            .child(div().text_color(t.muted).child("Open a repository to see its history")),
                    ),
                )
                .children(notice)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            btn("home-open", "Open repository")
                                .icon("folder")
                                .primary()
                                .suffix("Ctrl+O")
                                .on_click(cx.listener(|this, _, _, cx| this.pick_and_open(cx))),
                        )
                        .child(
                            btn("home-clone", "Clone repository")
                                .icon("download")
                                .on_click(|_, _, cx| super::actions::open_clone_dialog(cx)),
                        )
                        .children((self.recent.len() > 4).then(|| div().w(px(260.)).child(field(&self.home_filter, window, cx))))
                        .child(div().text_color(t.muted).text_size(px(12.)).child("or drop a folder on the window")),
                )
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(if self.recent.is_empty() {
                    "No recent repositories yet"
                } else {
                    "Recent repositories"
                }))
                .child(grid),
        )
    }

    fn render_sidebar(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.active_tab().cloned();
        let (name, path, branch) = match &tab {
            Some(tab) => {
                let tab = tab.read(cx);
                (tab.name.clone(), tab.path.clone(), tab.refs.as_ref().and_then(|r| r.head.branch.clone()))
            }
            None => ("No repository".to_string(), "Open a repository".to_string(), None),
        };
        let head = div().flex_none().p_2().child(
            div()
                .id("repo-switch")
                .flex()
                .items_center()
                .gap_2()
                .h(px(34.))
                .px_2()
                .rounded(px(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.panel2)
                .cursor_pointer()
                .hover(|s| s.bg(t.hover))
                .tooltip(tip(path))
                .child(icon("repo", 15., t.accent))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).child(clip(name)))
                .children(branch.map(|b| div().max_w(px(90.)).text_size(px(11.)).text_color(t.muted).child(clip(b))))
                .child(icon("chevron", 13., t.muted))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| this.open_repo_menu(e.position(), cx))),
        );
        let body: AnyElement =
            match &tab {
                Some(tab) => div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(div().flex_none().px_2().pb_2().child(field(&self.sb_filter, window, cx)))
                    .child({
                        let filter = self.sb_filter.read(cx).text().to_string();
                        super::sidebar::refs_list(tab, &filter, cx)
                    })
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .text_color(t.muted)
                    .child("Open a repository to see its branches, remotes, tags and stashes.")
                    .child(div().flex().child(
                        btn("sb-open", "Open repository").icon("folder").on_click(cx.listener(|this, _, _, cx| this.pick_and_open(cx))),
                    ))
                    .into_any_element(),
            };
        let chosen = theme::chosen(cx);
        let foot = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(40.))
            .px_2()
            .border_t_1()
            .border_color(t.border)
            .child(img("brand/symbol.svg").size(px(18.)))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(12.)).child(APP_NAME))
            .child(
                div()
                    .id("sb-version")
                    .px_1()
                    .rounded(px(4.))
                    .text_size(px(11.))
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .tooltip(tip("Check for updates"))
                    .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .on_click(|_, _, cx| super::tools::open_updates(cx)),
            )
            .children(self.update.as_ref().map(|latest| {
                div()
                    .id("sb-update")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px(px(6.))
                    .h(px(18.))
                    .rounded(px(9.))
                    .bg(t.accent)
                    .text_color(t.accent_text)
                    .text_size(px(11.))
                    .cursor_pointer()
                    .tooltip(tip(format!("{APP_NAME} {latest} is available")))
                    .child(icon("download", 11., t.accent_text))
                    .child("Update")
                    .on_click(|_, _, cx| super::tools::open_updates(cx))
            }))
            .child(div().flex_1())
            .child(
                icon_btn("sb-palette", "search")
                    .icon_size(14.)
                    .tip("Command palette (Ctrl+P)")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(Palette), cx)),
            )
            .child(
                icon_btn("sb-settings", "settings")
                    .icon_size(14.)
                    .tip("Settings (Ctrl+,)")
                    .on_click(|_, _, cx| super::tools::open_settings(cx)),
            )
            .child(
                icon_btn("sb-theme", "palette")
                    .icon_size(14.)
                    .tip(format!("Theme: {}", chosen.name))
                    .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| this.open_theme_menu(e.position(), cx))),
            );
        div().flex().flex_col().flex_none().w(px(self.sidebar_w)).h_full().bg(t.panel).child(head).child(body).child(foot)
    }

    fn render_menu(&self, menu: &Menu, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("menu")
            .occlude()
            .min_w(px(200.))
            .max_w(px(460.))
            .max_h(px(520.))
            .overflow_y_scroll()
            .py_1()
            .rounded(px(8.))
            .bg(t.elev)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .text_color(t.text)
            .text_size(px(13.));
        for (i, item) in menu.items.iter().enumerate() {
            list = list.child(match item.kind {
                MenuKind::Separator => div().my_1().h(px(1.)).bg(t.border).into_any_element(),
                MenuKind::Title => div().px_3().py_1().text_size(px(11.)).text_color(t.dim).child(item.label.clone()).into_any_element(),
                MenuKind::Item => {
                    let fg = if item.danger {
                        t.danger
                    } else if item.active {
                        t.accent
                    } else {
                        t.text
                    };
                    let (run, hover, disabled) = (item.run.clone(), item.hover.clone(), item.disabled);
                    let mut row = div()
                        .id(("menu-item", i))
                        .flex()
                        .items_center()
                        .gap_2()
                        .mx_1()
                        .px_2()
                        .min_h(px(28.))
                        .rounded(px(5.))
                        .text_color(fg)
                        .children(item.swatch.map(|colors| {
                            div()
                                .flex()
                                .flex_none()
                                .rounded(px(3.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(t.border_strong)
                                .children(colors.map(|c| div().w(px(9.)).h(px(14.)).bg(c)))
                        }))
                        .children(item.icon.filter(|_| item.swatch.is_none()).map(|name| {
                            icon(
                                name,
                                14.,
                                if item.danger {
                                    t.danger
                                } else if item.active {
                                    t.accent
                                } else {
                                    t.muted
                                },
                            )
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(clip(item.label.clone()))
                                .children(item.sub.clone().map(|s| div().text_size(px(11.)).text_color(t.dim).child(clip(s)))),
                        )
                        .children(item.hint.clone().map(|h| div().flex_none().text_size(px(11.)).text_color(t.dim).child(h)));
                    if disabled {
                        row = row.opacity(0.45);
                    } else {
                        row = row.cursor_pointer().hover(|s| s.bg(t.hover)).on_click(cx.listener(move |this, _, window, cx| {
                            this.close_menu(true, cx);
                            if let Some(run) = &run {
                                run(window, cx);
                            }
                        }));
                        if let Some(hover) = hover {
                            row = row.on_hover(move |inside, _, cx| {
                                if *inside {
                                    hover(cx);
                                }
                            });
                        }
                    }
                    row.into_any_element()
                }
            });
        }
        let corner = if menu.upward { Corner::BottomLeft } else { Corner::TopLeft };
        div()
            .id("menu-layer")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(LEFT, cx.listener(|this, _, _, cx| this.close_menu(false, cx)))
            .on_mouse_down(RIGHT, cx.listener(|this, _, _, cx| this.close_menu(false, cx)))
            .child(deferred(anchored().position(menu.pos).anchor(corner).snap_to_window_with_margin(px(8.)).child(list)).with_priority(2))
    }

    fn render_toasts(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().absolute().bottom(px(16.)).left(px(self.sidebar_w + 16.)).flex().flex_col().items_start().gap_2();
        for toast in &self.toasts {
            let id = toast.id;
            let (color, name) = match toast.kind {
                ToastKind::Info => (t.accent, "info"),
                ToastKind::Success => (t.ok, "check"),
                ToastKind::Error => (t.danger, "alert"),
            };
            let action = toast.action.clone();
            col = col.child(
                div()
                    .id(("toast", id as usize))
                    .occlude()
                    .flex()
                    .items_center()
                    .gap_2()
                    .max_w(px(520.))
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(t.elev)
                    .border_1()
                    .border_color(color)
                    .shadow_lg()
                    .text_color(t.text)
                    .text_size(px(13.))
                    .child(icon(name, 15., color))
                    .child(div().flex_1().min_w_0().child(toast.message.clone()))
                    .children(action.map(|(label, run)| {
                        btn(("toast-act", id as usize), label).on_click(cx.listener(move |this, _, window, cx| {
                            this.toasts.retain(|t| t.id != id);
                            cx.notify();
                            run(window, cx);
                        }))
                    }))
                    .child(icon_btn(("toast-x", id as usize), "x").icon_size(12.).on_click(cx.listener(move |this, _, _, cx| {
                        this.toasts.retain(|t| t.id != id);
                        cx.notify();
                    }))),
            );
        }
        col
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let title = match self.active_tab() {
            Some(tab) => format!("{} — {APP_NAME}", tab.read(cx).name),
            None => APP_NAME.to_string(),
        };
        window.set_window_title(&title);

        let content: AnyElement = match self.active_tab() {
            Some(tab) => tab.clone().into_any_element(),
            None => self.render_home(&t, window, cx).into_any_element(),
        };
        let mut root = div()
            .id("shell")
            .key_context("Shell")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.text)
            .text_size(px(13.))
            .font_family(super::UI_FONT)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &OpenRepo, _, cx| this.pick_and_open(cx)))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if let Some(i) = this.active {
                    this.close_tab(i, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.step_tab(false, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| this.step_tab(true, cx)))
            .on_action(cx.listener(|this, _: &Palette, _, cx| super::tools::open_palette(this, cx)))
            .on_action(cx.listener(|_, _: &OpenSettings, _, cx| super::tools::open_settings(cx)))
            .on_action(cx.listener(|_, _: &Shortcuts, _, cx| super::tools::open_shortcuts(cx)))
            .on_action(cx.listener(|this, _: &Fetch, _, cx| this.with_tab(cx, |tab, cx| tab.fetch_all(false, false, cx))))
            .on_action(cx.listener(|this, _: &Pull, _, cx| this.with_tab(cx, |tab, cx| tab.pull(None, cx))))
            .on_action(cx.listener(|this, _: &Push, _, cx| this.with_tab(cx, |tab, cx| tab.push(false, cx))))
            .on_action(cx.listener(|this, _: &StashSave, _, cx| this.with_tab(cx, |tab, cx| tab.stash_save(cx))))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.with_tab(cx, |tab, cx| tab.show_changes(cx))))
            .on_action(cx.listener(|this, _: &NewBranch, _, cx| this.with_tab(cx, |tab, cx| tab.create_branch(None, cx))))
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                if let Some(tab) = this.active_tab().cloned() {
                    tab.update(cx, |tab, cx| tab.focus_search(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.with_tab(cx, |tab, cx| tab.refresh(true, cx))))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if let Some(Resize::Sidebar { x, width }) = &this.resize {
                    let dx: f32 = (e.position.x - *x).into();
                    this.sidebar_w = (width + dx).clamp(180., 520.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                LEFT,
                cx.listener(|this, _, _, cx| {
                    if this.resize.take().is_some() {
                        let w = this.sidebar_w;
                        set_pref(cx, "gl.sidebarW", &w);
                    }
                }),
            )
            // A dropped folder opens as a repository.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                for p in paths.paths() {
                    this.open_repo(p.display().to_string(), true, false, cx);
                }
            }))
            .child(self.render_sidebar(&t, window, cx))
            .child(splitter("sidebar-resizer", false, &t).on_mouse_down(
                LEFT,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, _| {
                    this.resize = Some(Resize::Sidebar { x: e.position.x, width: this.sidebar_w });
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_tab_bar(&t, cx))
                    .child(div().flex_1().min_h_0().child(content)),
            );

        if !self.modals.is_empty() {
            let mut layer = div()
                .id("modal-layer")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::black().opacity(0.45));
            if let Some((view, _)) = self.modals.last() {
                layer = layer.child(view.clone());
            }
            root = root.child(deferred(layer).with_priority(1));
        }
        if let Some(menu) = &self.menu {
            root = root.child(self.render_menu(menu, &t, cx));
        }
        if !self.toasts.is_empty() {
            root = root.child(deferred(self.render_toasts(&t, cx)).with_priority(3));
        }
        root
    }
}
