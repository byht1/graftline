//! Things drawn above the window content: menus, dialogs, notifications. The shell owns
//! them; the functions here reach the shell from any view.

use super::input::{field, InputEvent, TextInput};
use super::shell::Shell;
use super::theme::{theme, Theme};
use super::widgets::{btn, checkbox, icon, icon_btn};
use gpui::prelude::*;
use gpui::{
    div, px, AnyWindowHandle, App, ClipboardItem, Div, Entity, FocusHandle, Focusable, FontWeight, Global, Hsla, Pixels, Point,
    SharedString, Task, WeakEntity, Window,
};
use std::collections::HashMap;
use std::rc::Rc;

/// Where the shell is. Set when the window opens.
#[derive(Clone)]
pub struct ShellRef {
    pub shell: WeakEntity<Shell>,
    pub window: AnyWindowHandle,
}

impl Global for ShellRef {}

/// Runs `f` with the shell after the current update ends. Views call this from their own
/// handlers, where the shell cannot be updated directly.
pub fn with_shell(cx: &mut App, f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static) {
    let Some(r) = cx.try_global::<ShellRef>().cloned() else { return };
    cx.defer(move |cx| {
        let _ = r.window.update(cx, |_, window, cx| {
            let _ = r.shell.update(cx, |shell, cx| f(shell, window, cx));
        });
    });
}

// ---------------------------------------------------------------- menus

pub type Run = Rc<dyn Fn(&mut Window, &mut App)>;
/// A callback that needs no window.
pub type AppFn = Rc<dyn Fn(&mut App)>;
type Once = Box<dyn FnOnce(&mut Window, &mut App)>;

#[derive(Clone)]
pub enum MenuKind {
    Item,
    Separator,
    Title,
}

#[derive(Clone)]
pub struct MenuItem {
    pub kind: MenuKind,
    pub icon: Option<&'static str>,
    pub label: SharedString,
    pub sub: Option<SharedString>,
    pub hint: Option<SharedString>,
    pub danger: bool,
    pub disabled: bool,
    pub active: bool,
    /// Four colors shown before the label (theme menu).
    pub swatch: Option<[Hsla; 4]>,
    pub run: Option<Run>,
    /// Called when the pointer enters the item.
    pub hover: Option<AppFn>,
}

impl MenuItem {
    pub fn new(icon: &'static str, label: impl Into<SharedString>, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        MenuItem {
            kind: MenuKind::Item,
            icon: Some(icon),
            label: label.into(),
            sub: None,
            hint: None,
            danger: false,
            disabled: false,
            active: false,
            swatch: None,
            run: Some(Rc::new(run)),
            hover: None,
        }
    }
    pub fn sep() -> Self {
        MenuItem { kind: MenuKind::Separator, run: None, icon: None, ..MenuItem::new("x", "", |_, _| {}) }
    }
    pub fn title(text: impl Into<SharedString>) -> Self {
        MenuItem { kind: MenuKind::Title, run: None, icon: None, ..MenuItem::new("x", text, |_, _| {}) }
    }
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn sub(mut self, sub: impl Into<SharedString>) -> Self {
        self.sub = Some(sub.into());
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

pub struct Menu {
    pub pos: Point<Pixels>,
    pub items: Vec<MenuItem>,
    /// Called when the menu closes without a choice.
    pub on_dismiss: Option<AppFn>,
    /// The menu grows upward from `pos` (a button at the bottom of the window).
    pub upward: bool,
}

/// Opens a menu at a window position (usually the pointer).
pub fn open_menu(cx: &mut App, pos: Point<Pixels>, items: Vec<MenuItem>) {
    open_menu_with(cx, Menu { pos, items, on_dismiss: None, upward: false });
}

pub fn open_menu_with(cx: &mut App, menu: Menu) {
    if menu.items.is_empty() {
        return;
    }
    with_shell(cx, move |shell, _, cx| {
        shell.menu = Some(menu);
        cx.notify();
    });
}

// ---------------------------------------------------------------- toasts

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub id: u64,
    pub message: SharedString,
    pub kind: ToastKind,
    pub action: Option<(SharedString, Run)>,
}

pub fn toast(cx: &mut App, message: impl Into<SharedString>, kind: ToastKind) {
    push_toast(cx, message.into(), kind, None);
}

pub fn info(cx: &mut App, message: impl Into<SharedString>) {
    toast(cx, message, ToastKind::Info);
}

pub fn success(cx: &mut App, message: impl Into<SharedString>) {
    toast(cx, message, ToastKind::Success);
}

pub fn error(cx: &mut App, message: impl Into<SharedString>) {
    toast(cx, message, ToastKind::Error);
}

/// A notification with a button, for example **Undo**.
pub fn toast_action(
    cx: &mut App,
    message: impl Into<SharedString>,
    kind: ToastKind,
    label: impl Into<SharedString>,
    run: impl Fn(&mut Window, &mut App) + 'static,
) {
    push_toast(cx, message.into(), kind, Some((label.into(), Rc::new(run))));
}

fn push_toast(cx: &mut App, message: SharedString, kind: ToastKind, action: Option<(SharedString, Run)>) {
    with_shell(cx, move |shell, _, cx| {
        shell.next_toast += 1;
        let id = shell.next_toast;
        let secs = if action.is_some() || kind == ToastKind::Error { 9 } else { 4 };
        shell.toasts.push(Toast { id, message, kind, action });
        if shell.toasts.len() > 5 {
            shell.toasts.remove(0);
        }
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(secs)).await;
            let _ = shell.update(cx, |shell, cx| {
                shell.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    });
}

pub fn copy_text(cx: &mut App, text: impl Into<String>, what: &str) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
    info(cx, if what.is_empty() { "Copied to clipboard".to_string() } else { what.to_string() });
}

// ---------------------------------------------------------------- dialogs

/// Opens a dialog. `build` creates its view; the shell focuses it and returns the focus to
/// the window when the dialog closes.
pub fn open_modal<V: Render + Focusable>(cx: &mut App, build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static) {
    with_shell(cx, move |shell, window, cx| {
        let view = cx.new(|cx| build(window, cx));
        let focus = view.focus_handle(cx);
        window.focus(&focus);
        shell.menu = None;
        shell.modals.push((view.into(), focus));
        cx.notify();
    });
}

/// Closes the top dialog.
pub fn close_modal(cx: &mut App) {
    with_shell(cx, |shell, window, cx| shell.pop_modal(window, cx));
}

/// A paragraph of a dialog body.
#[derive(Clone)]
pub enum Para {
    Text(String),
    Muted(String),
    /// A warning with an alert icon.
    Warn(String),
    /// Monospaced block, for example a command or a list of commits.
    Code(String),
}

pub fn paras(items: &[Para], t: &Theme) -> Div {
    let mut el = div().flex().flex_col().gap_2();
    for p in items {
        el = el.child(match p {
            Para::Text(s) => div().child(s.clone()),
            Para::Muted(s) => div().text_color(t.muted).text_size(px(12.)).child(s.clone()),
            Para::Warn(s) => div()
                .flex()
                .gap_2()
                .p_2()
                .rounded(px(6.))
                .bg(t.mod_bg)
                .border_1()
                .border_color(t.mod_border)
                .child(icon("alert", 15., t.text))
                .child(div().flex_1().min_w_0().child(s.clone())),
            Para::Code(s) => code_block(s, t),
        });
    }
    el
}

pub fn code_block(text: &str, t: &Theme) -> Div {
    div()
        .p_2()
        .rounded(px(6.))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .font_family(super::MONO_FONT)
        .text_size(px(12.))
        .children(text.lines().map(|l| div().min_h(px(16.)).child(SharedString::from(l.to_string()))))
}

/// The frame of a dialog: title bar with a close button, body, footer with buttons.
pub fn modal_frame(title: impl Into<SharedString>, width: f32, body: impl IntoElement, footer: Vec<gpui::AnyElement>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .w(px(width))
        .max_h(gpui::relative(0.88))
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
                .flex_none()
                .items_center()
                .justify_between()
                .px_4()
                .h(px(44.))
                .border_b_1()
                .border_color(t.border)
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child(title.into()))
                .child(icon_btn("modal-close", "x").tip("Close (Esc)").on_click(|_, _, cx| close_modal(cx))),
        )
        .child(div().id("modal-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
        .when(!footer.is_empty(), |el| {
            el.child(div().flex().flex_none().justify_end().gap_2().px_4().py_3().border_t_1().border_color(t.border).children(footer))
        })
}

// ---- confirm

pub struct ConfirmOpts {
    pub title: String,
    pub body: Vec<Para>,
    pub confirm: String,
    pub danger: bool,
}

struct ConfirmDialog {
    opts: ConfirmOpts,
    focus: FocusHandle,
    on_ok: Option<Once>,
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmDialog {
    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.on_ok.take() {
            close_modal(cx);
            f(window, cx);
        }
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let ok = btn("confirm-ok", self.opts.confirm.clone()).kind(if self.opts.danger {
            super::widgets::BtnKind::Danger
        } else {
            super::widgets::BtnKind::Primary
        });
        let ok = ok.on_click(cx.listener(|this, _, window, cx| this.accept(window, cx)));
        let cancel = btn("confirm-cancel", "Cancel").on_click(|_, _, cx| close_modal(cx));
        // Enter must not confirm a destructive action by accident.
        let danger = self.opts.danger;
        div()
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(move |this, _: &super::shell::Confirm, window, cx| {
                if !danger {
                    this.accept(window, cx);
                }
            }))
            .child(modal_frame(
                self.opts.title.clone(),
                460.,
                paras(&self.opts.body, &t),
                vec![cancel.into_any_element(), ok.into_any_element()],
                cx,
            ))
    }
}

/// Asks before an action. `on_ok` runs when the user confirms.
pub fn confirm(cx: &mut App, opts: ConfirmOpts, on_ok: impl FnOnce(&mut Window, &mut App) + 'static) {
    open_modal(cx, move |_, cx| ConfirmDialog { opts, focus: cx.focus_handle(), on_ok: Some(Box::new(on_ok)) });
}

// ---- error

struct ErrorDialog {
    title: String,
    command: String,
    output: String,
    focus: FocusHandle,
}

impl Focusable for ErrorDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ErrorDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let text = format!("$ {}\n{}", self.command, self.output);
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_color(t.muted).text_size(px(12.)).child("Command"))
            .child(code_block(&self.command, &t))
            .child(div().text_color(t.muted).text_size(px(12.)).child("Output"))
            .child(code_block(&self.output, &t));
        let copy = btn("error-copy", "Copy").icon("copy").on_click(move |_, _, cx| copy_text(cx, text.clone(), ""));
        div().track_focus(&self.focus).key_context("Dialog").child(modal_frame(
            self.title.clone(),
            720.,
            body,
            vec![copy.into_any_element()],
            cx,
        ))
    }
}

/// Shows an error. A failed git command is shown word for word with the command that
/// caused it; other errors become a notification.
pub fn show_error(cx: &mut App, e: &anyhow::Error) {
    show_error_titled(cx, e, "The operation failed");
}

pub fn show_error_titled(cx: &mut App, e: &anyhow::Error, title: &str) {
    match e.downcast_ref::<graftline::gitcli::GitError>() {
        Some(git) => {
            let (title, command) = (title.to_string(), git.command.clone());
            let output = if git.output.trim().is_empty() { git.to_string() } else { git.output.clone() };
            open_modal(cx, move |_, cx| ErrorDialog { title, command, output, focus: cx.focus_handle() });
        }
        None => error(cx, format!("{e:#}")),
    }
}

// ---- form

pub enum FieldKind {
    Text,
    /// Several lines.
    Area,
    Check,
    /// `(value, label)` pairs.
    Select(Vec<(String, String)>),
}

pub struct Field {
    pub key: &'static str,
    pub label: String,
    pub kind: FieldKind,
    pub value: String,
    pub placeholder: String,
    pub hint: Option<String>,
}

impl Field {
    pub fn text(key: &'static str, label: &str, value: impl Into<String>) -> Self {
        Field { key, label: label.into(), kind: FieldKind::Text, value: value.into(), placeholder: String::new(), hint: None }
    }
    pub fn area(key: &'static str, label: &str, value: impl Into<String>) -> Self {
        Field { kind: FieldKind::Area, ..Field::text(key, label, value) }
    }
    pub fn check(key: &'static str, label: &str, value: bool) -> Self {
        Field { kind: FieldKind::Check, ..Field::text(key, label, if value { "1" } else { "" }) }
    }
    pub fn select(key: &'static str, label: &str, options: Vec<(String, String)>, value: impl Into<String>) -> Self {
        Field { kind: FieldKind::Select(options), ..Field::text(key, label, value) }
    }
    pub fn placeholder(mut self, text: &str) -> Self {
        self.placeholder = text.into();
        self
    }
    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.hint = Some(text.into());
        self
    }
}

/// The values of a form when it is submitted.
#[derive(Clone, Default)]
pub struct Values(HashMap<&'static str, String>);

impl Values {
    pub fn text(&self, key: &str) -> String {
        self.0.get(key).cloned().unwrap_or_default()
    }
    pub fn trimmed(&self, key: &str) -> String {
        self.text(key).trim().to_string()
    }
    pub fn flag(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|v| !v.is_empty())
    }
}

pub type Submit = Rc<dyn Fn(Values, &mut Window, &mut App) -> Task<anyhow::Result<()>>>;

pub struct FormOpts {
    pub title: String,
    pub intro: Vec<Para>,
    pub fields: Vec<Field>,
    pub confirm: String,
    pub danger: bool,
}

struct FormDialog {
    opts: FormOpts,
    inputs: HashMap<&'static str, Entity<TextInput>>,
    values: HashMap<&'static str, String>,
    status: Option<String>,
    busy: bool,
    focus: FocusHandle,
    submit: Submit,
    _subs: Vec<gpui::Subscription>,
}

impl Focusable for FormDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // The first text field takes the keyboard.
        let first =
            self.opts.fields.iter().find(|f| matches!(f.kind, FieldKind::Text | FieldKind::Area)).and_then(|f| self.inputs.get(f.key));
        first.map(|i| i.focus_handle(cx)).unwrap_or_else(|| self.focus.clone())
    }
}

impl FormDialog {
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut values = self.values.clone();
        for (key, input) in &self.inputs {
            values.insert(key, input.read(cx).text().to_string());
        }
        self.busy = true;
        self.status = None;
        cx.notify();
        let task = (self.submit)(Values(values), window, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => close_modal(cx),
                    Err(e) => this.status = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for FormDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let mut body = div().flex().flex_col().gap_3();
        if !self.opts.intro.is_empty() {
            body = body.child(paras(&self.opts.intro, &t));
        }
        for (i, f) in self.opts.fields.iter().enumerate() {
            let key = f.key;
            let row = match &f.kind {
                FieldKind::Text | FieldKind::Area => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(t.muted).text_size(px(12.)).child(f.label.clone()))
                    .child(field(&self.inputs[key], window, cx)),
                FieldKind::Check => {
                    let on = self.values.get(key).is_some_and(|v| !v.is_empty());
                    div().child(checkbox(
                        ("form-check", i),
                        on,
                        f.label.clone(),
                        &t,
                        cx.listener(move |this, value: &bool, _, cx| {
                            this.values.insert(key, if *value { "1".into() } else { String::new() });
                            cx.notify();
                        })
                        .into_toggle(),
                    ))
                }
                FieldKind::Select(options) => {
                    let current = self.values.get(key).cloned().unwrap_or_default();
                    let shown = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone()).unwrap_or(current.clone());
                    let options = options.clone();
                    let this = cx.entity().downgrade();
                    div().flex().flex_col().gap_1().child(div().text_color(t.muted).text_size(px(12.)).child(f.label.clone())).child(
                        div()
                            .id(("form-select", i))
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
                                        let (this, value, active) = (this.clone(), value.clone(), *value == current);
                                        MenuItem::new(if active { "check" } else { "minus" }, label.clone(), move |_, cx| {
                                            let _ = this.update(cx, |this, cx| {
                                                this.values.insert(key, value.clone());
                                                cx.notify();
                                            });
                                        })
                                        .active(active)
                                    })
                                    .collect();
                                open_menu(cx, e.position(), items);
                            }),
                    )
                }
            };
            body = body.child(row.children(f.hint.clone().map(|h| div().text_color(t.dim).text_size(px(12.)).child(h))));
        }
        if let Some(status) = &self.status {
            body = body.child(div().text_color(t.danger).child(status.clone()));
        }
        let ok = btn("form-ok", self.opts.confirm.clone())
            .kind(if self.opts.danger { super::widgets::BtnKind::Danger } else { super::widgets::BtnKind::Primary })
            .disabled(self.busy)
            .on_click(cx.listener(|this, _, window, cx| this.go(window, cx)));
        let cancel = btn("form-cancel", "Cancel").on_click(|_, _, cx| close_modal(cx));
        div()
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &super::shell::Confirm, window, cx| this.go(window, cx)))
            .child(modal_frame(self.opts.title.clone(), 480., body, vec![cancel.into_any_element(), ok.into_any_element()], cx))
    }
}

/// Adapter: a listener that takes `&bool` used where a `Fn(bool, …)` is wanted.
pub trait IntoToggle {
    fn into_toggle(self) -> impl Fn(bool, &mut Window, &mut App) + 'static;
}

impl<F: Fn(&bool, &mut Window, &mut App) + 'static> IntoToggle for F {
    fn into_toggle(self) -> impl Fn(bool, &mut Window, &mut App) + 'static {
        move |v, window, cx| self(&v, window, cx)
    }
}

/// A dialog with input fields. `submit` does the work; when it fails, the message is shown
/// and the dialog stays open.
pub fn form(cx: &mut App, opts: FormOpts, submit: impl Fn(Values, &mut Window, &mut App) -> Task<anyhow::Result<()>> + 'static) {
    open_modal(cx, move |window, cx: &mut Context<FormDialog>| {
        let mut inputs = HashMap::new();
        let mut values = HashMap::new();
        let mut subs = Vec::new();
        for f in &opts.fields {
            match f.kind {
                FieldKind::Text | FieldKind::Area => {
                    let area = matches!(f.kind, FieldKind::Area);
                    let (value, placeholder) = (f.value.clone(), f.placeholder.clone());
                    let input = cx.new(|cx| {
                        let i = TextInput::new(cx).placeholder(placeholder).with_text(value);
                        if area {
                            i.multiline(4)
                        } else {
                            i
                        }
                    });
                    if !area {
                        input.update(cx, |i, cx| i.select_all_text(cx));
                    }
                    subs.push(cx.subscribe_in(&input, window, |this: &mut FormDialog, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Submit | InputEvent::Enter) {
                            this.go(window, cx);
                        }
                    }));
                    inputs.insert(f.key, input);
                }
                _ => {
                    values.insert(f.key, f.value.clone());
                }
            }
        }
        FormDialog { opts, inputs, values, status: None, busy: false, focus: cx.focus_handle(), submit: Rc::new(submit), _subs: subs }
    });
}
