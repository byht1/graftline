//! Small building blocks: icons, buttons, tooltips, badges, avatars, text helpers.

use super::theme::{theme, Theme};
use gpui::prelude::*;
use gpui::{
    div, px, svg, AnyView, App, ClickEvent, Div, ElementId, FontWeight, Hsla, MouseButton, Pixels, SharedString, Stateful, Svg, Window,
};

pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).flex_none().text_color(color)
}

struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div().pt_2().child(
            div()
                .max_w(px(420.))
                .px_2()
                .py_1()
                .rounded(px(5.))
                .bg(t.elev)
                .border_1()
                .border_color(t.border_strong)
                .text_color(t.text)
                .text_size(px(12.))
                .children(self.0.split('\n').map(|line| div().child(SharedString::from(line.to_string())))),
        )
    }
}

/// A tooltip builder for `.tooltip(…)`.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

#[derive(Clone, Copy, PartialEq)]
pub enum BtnKind {
    Normal,
    Primary,
    Danger,
    /// No border and no background until the pointer is on it.
    Ghost,
}

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    icon_size: f32,
    kind: BtnKind,
    disabled: bool,
    active: bool,
    tip: Option<SharedString>,
    suffix: Option<SharedString>,
    on_click: Option<Click>,
}

/// A button with a label.
pub fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Btn {
    Btn {
        id: id.into(),
        label: Some(label.into()),
        icon: None,
        icon_size: 14.,
        kind: BtnKind::Normal,
        disabled: false,
        active: false,
        tip: None,
        suffix: None,
        on_click: None,
    }
}

/// A square button with an icon only.
pub fn icon_btn(id: impl Into<ElementId>, icon: &'static str) -> Btn {
    Btn {
        id: id.into(),
        label: None,
        icon: Some(icon),
        icon_size: 15.,
        kind: BtnKind::Ghost,
        disabled: false,
        active: false,
        tip: None,
        suffix: None,
        on_click: None,
    }
}

impl Btn {
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }
    pub fn kind(mut self, kind: BtnKind) -> Self {
        self.kind = kind;
        self
    }
    pub fn primary(self) -> Self {
        self.kind(BtnKind::Primary)
    }
    pub fn danger(self) -> Self {
        self.kind(BtnKind::Danger)
    }
    pub fn ghost(self) -> Self {
        self.kind(BtnKind::Ghost)
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// Shown as pressed (a toggle that is on).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }
    /// Small dim text after the label: a count or a shortcut.
    pub fn suffix(mut self, text: impl Into<SharedString>) -> Self {
        self.suffix = Some(text.into());
        self
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let (bg, fg, border, hover) = match self.kind {
            BtnKind::Normal => (t.panel2, t.text, t.border_strong, t.hover),
            BtnKind::Primary => (t.accent, t.accent_text, t.accent, t.sel),
            BtnKind::Danger => (t.danger_soft, t.danger, t.danger, t.danger_soft),
            BtnKind::Ghost => (gpui::transparent_black(), t.muted, gpui::transparent_black(), t.hover),
        };
        let icon_only = self.label.is_none();
        let mut el = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(28.))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(if self.active { t.accent_soft } else { bg })
            .text_color(if self.active { t.accent } else { fg })
            .text_size(px(13.));
        el = if icon_only { el.w(px(28.)) } else { el.px(px(10.)) };
        if self.disabled {
            el = el.opacity(0.45);
        } else {
            el = el.cursor_pointer().hover(move |s| s.bg(hover));
            if let Some(f) = self.on_click {
                el = el.on_click(f);
            }
        }
        if let Some(text) = self.tip {
            el = el.tooltip(tip(text));
        }
        let icon_color = if self.active { t.accent } else { fg };
        el.children(self.icon.map(|name| icon(name, self.icon_size, icon_color)))
            .children(self.label.map(|l| div().whitespace_nowrap().child(l)))
            .children(self.suffix.map(|s| div().text_size(px(11.)).opacity(0.7).whitespace_nowrap().child(s)))
    }
}

/// A small rounded label.
pub fn badge(text: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .px(px(6.))
        .h(px(18.))
        .rounded(px(9.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .child(text.into())
}

/// Text that is cut with an ellipsis when it does not fit.
pub fn clip(text: impl Into<SharedString>) -> Div {
    div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text.into())
}

const AVATAR_COLORS: [u32; 10] = [0x0d9488, 0xb45309, 0x7c3aed, 0xbe123c, 0x0369a1, 0x4d7c0f, 0xc2410c, 0xa21caf, 0x475569, 0x0f766e];

/// A colored circle with the author's initials. No network request, so no avatar service
/// learns who is in the log.
pub fn avatar(name: &str, email: &str, size: f32) -> Div {
    let key = (if email.is_empty() { name } else { email }).to_lowercase();
    let mut hash: u32 = 0;
    for b in key.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u32);
    }
    let source = if name.trim().is_empty() { email } else { name };
    let words: Vec<&str> =
        source.trim().split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-')).filter(|w| !w.is_empty()).collect();
    let first = words.first().and_then(|w| w.chars().next()).unwrap_or('?');
    let mut initials = first.to_uppercase().to_string();
    if words.len() > 1 {
        if let Some(c) = words[words.len() - 1].chars().next() {
            initials.extend(c.to_uppercase());
        }
    }
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(gpui::rgb(AVATAR_COLORS[(hash % 10) as usize]))
        .text_color(gpui::white())
        .text_size(px(size * 0.42))
        .font_weight(FontWeight::SEMIBOLD)
        .child(initials)
}

/// A checkbox with a label. `on_toggle` gets the new value.
pub fn checkbox(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    t: &Theme,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let mark = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(15.))
        .rounded(px(3.))
        .border_1()
        .border_color(if checked { t.accent } else { t.border_strong })
        .bg(if checked { t.accent } else { t.panel })
        .children(checked.then(|| icon("check", 11., t.accent_text)));
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .child(mark)
        .child(div().child(label.into()))
        .on_click(move |_, window, cx| on_toggle(!checked, window, cx))
}

/// A thin handle between two panes. `on_drag(dx, dy)` gets the pointer movement since the
/// last call while the left button is down.
pub fn splitter(id: impl Into<ElementId>, vertical: bool, t: &Theme) -> Stateful<Div> {
    let el = div().id(id.into()).flex_none().bg(t.border).hover(|s| s.bg(t.accent));
    if vertical {
        el.h(px(3.)).w_full().cursor_row_resize()
    } else {
        el.w(px(3.)).h_full().cursor_col_resize()
    }
}

/// `2026-11-03 14:20` in local time.
pub fn fmt_date(sec: i64) -> String {
    let (y, m, d, hh, mm) = local_parts(sec);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// Short relative time for recent dates, the date otherwise.
pub fn rel_time(sec: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let s = (now - sec).max(0);
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 86400 {
        format!("{} h ago", s / 3600)
    } else if s < 86400 * 7 {
        format!("{} d ago", s / 86400)
    } else {
        fmt_date(sec)[..10].to_string()
    }
}

/// Offset of local time from UTC in seconds, read once from the system.
fn utc_offset() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(local_offset)
}

#[cfg(unix)]
fn local_offset() -> i64 {
    // `date +%z` prints the offset as +hhmm.
    let out = std::process::Command::new("date").arg("+%z").output().ok();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    parse_offset(&text)
}

#[cfg(windows)]
fn local_offset() -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime([u16; 8]);
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }
    let mut info = TimeZoneInformation {
        bias: 0,
        standard_name: [0; 32],
        standard_date: SystemTime::default(),
        standard_bias: 0,
        daylight_name: [0; 32],
        daylight_date: SystemTime::default(),
        daylight_bias: 0,
    };
    // SAFETY: the structure has the layout of TIME_ZONE_INFORMATION and lives for the call.
    let kind = unsafe { GetTimeZoneInformation(&mut info) };
    let bias = info.bias + if kind == 2 { info.daylight_bias } else { info.standard_bias };
    -(bias as i64) * 60
}

#[cfg(unix)]
fn parse_offset(text: &str) -> i64 {
    if text.len() != 5 {
        return 0;
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let h: i64 = text[1..3].parse().unwrap_or(0);
    let m: i64 = text[3..5].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

/// Year, month, day, hour, minute of a Unix time in local time.
fn local_parts(sec: i64) -> (i64, i64, i64, i64, i64) {
    let t = sec + utc_offset();
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, rem / 3600, rem % 3600 / 60)
}

/// `12,345`.
pub fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub const LEFT: MouseButton = MouseButton::Left;
pub const RIGHT: MouseButton = MouseButton::Right;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_get_thousands_separators() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(999), "999");
        assert_eq!(fmt_num(1000), "1,000");
        assert_eq!(fmt_num(86230), "86,230");
        assert_eq!(fmt_num(1234567), "1,234,567");
    }

    #[test]
    fn civil_date_is_computed() {
        // 2026-11-03 14:20:00 UTC, read without the local offset.
        let (y, m, d, hh, mm) = local_parts(1_793_715_600 - utc_offset());
        assert_eq!((y, m, d, hh, mm), (2026, 11, 3, 14, 20));
    }
}

/// Drag state of one scrollbar: the pointer position and the scroll offset at the start.
#[derive(Clone, Default)]
pub struct ScrollbarState(std::rc::Rc<std::cell::Cell<Option<(Pixels, Pixels)>>>);

/// A vertical scrollbar for a scroll handle. Place it in a `relative` container; it takes
/// the right edge.
pub fn scrollbar(handle: gpui::ScrollHandle, state: ScrollbarState, t: &Theme) -> impl IntoElement {
    use gpui::{canvas, fill, point, size, Bounds, DispatchPhase, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    let (thumb_color, active_color) = (t.border_strong, t.dim);
    canvas(
        |_, _, _| (),
        move |track: Bounds<Pixels>, _, window, _| {
            let view = handle.bounds().size.height;
            let max = handle.max_offset().height;
            if max <= px(0.5) || view <= px(0.) {
                state.0.set(None);
                return;
            }
            let thumb_h = (track.size.height * (view / (view + max))).max(px(28.)).min(track.size.height);
            let range = track.size.height - thumb_h;
            let ratio = (-handle.offset().y / max).clamp(0., 1.);
            let thumb = Bounds::new(point(track.left() + px(2.), track.top() + range * ratio), size(track.size.width - px(4.), thumb_h));
            let dragging = state.0.get().is_some();
            window.paint_quad(fill(thumb, if dragging { active_color } else { thumb_color }).corner_radii(px(3.)));

            let (h1, s1) = (handle.clone(), state.clone());
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || e.button != MouseButton::Left || !track.contains(&e.position) {
                    return;
                }
                if !thumb.contains(&e.position) && range > px(0.) {
                    // A click on the track moves the thumb there.
                    let r = ((e.position.y - track.top() - thumb_h / 2.) / range).clamp(0., 1.);
                    h1.set_offset(point(h1.offset().x, -(max * r)));
                }
                s1.0.set(Some((e.position.y, h1.offset().y)));
                window.refresh();
                cx.stop_propagation();
            });
            let (h2, s2) = (handle.clone(), state.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, _, window, _| {
                let Some((y0, offset0)) = s2.0.get() else { return };
                if range <= px(0.) {
                    return;
                }
                let moved = (e.position.y - y0) / range;
                let y = (offset0 - max * moved).clamp(-max, px(0.));
                h2.set_offset(point(h2.offset().x, y));
                window.refresh();
            });
            let s3 = state.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, _, window, _| {
                if s3.0.take().is_some() {
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .right_0()
    .w(px(11.))
}
