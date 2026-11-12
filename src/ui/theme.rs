//! Color themes. The values live in `assets/ui/themes.css` as blocks of `--name: value`
//! pairs; `tests/theme_contrast.rs` checks them. This module parses the blocks at start.

use gpui::{App, Global, Hsla, Rgba};
use std::collections::HashMap;
use std::sync::Arc;

const SOURCE: &str = include_str!("../../assets/ui/themes.css");

/// Theme ids in menu order, with the names shown to the user.
pub const THEMES: &[(&str, &str)] = &[
    ("graftline-light", "Graftline Light"),
    ("graftline", "Graftline Dark"),
    ("dark", "Graphite"),
    ("light", "Paper"),
    ("one-dark-pro", "One Dark Pro"),
    ("dracula", "Dracula"),
    ("nord", "Nord"),
    ("github-light", "GitHub Light"),
];

macro_rules! theme {
    ($($field:ident => $var:literal),* $(,)?) => {
        #[derive(Clone, Debug)]
        pub struct Theme {
            pub id: &'static str,
            pub name: &'static str,
            /// Branch colors of the commit graph.
            pub graph: [Hsla; 8],
            $(pub $field: Hsla,)*
        }

        fn build(id: &'static str, name: &'static str, vars: &HashMap<&str, &str>) -> Theme {
            let get = |key: &str| color(vars.get(key).copied().unwrap_or_else(|| panic!("theme {id} has no {key}")));
            Theme {
                id,
                name,
                graph: std::array::from_fn(|i| get(&format!("--graph-{}", i + 1))),
                $($field: get($var),)*
            }
        }
    };
}

theme! {
    bg => "--bg", panel => "--panel", panel2 => "--panel-2", elev => "--elev", hover => "--hover",
    border => "--border", border_strong => "--border-strong",
    text => "--text", muted => "--muted", dim => "--dim",
    accent => "--accent", accent_text => "--accent-text", accent_soft => "--accent-soft",
    danger => "--danger", danger_soft => "--danger-soft",
    mod_bg => "--mod-bg", mod_border => "--mod-border", new_bg => "--new-bg", del_bg => "--del-bg",
    sel => "--sel", row_sel => "--row-sel",
    t_kw => "--t-kw", t_str => "--t-str", t_num => "--t-num", t_com => "--t-com", t_id => "--t-id",
    t_fn => "--t-fn", t_type => "--t-type", t_op => "--t-op",
    t_const => "--t-const", t_punct => "--t-punct",
    ok => "--true",
}

/// `#rrggbb` or `rgba(r, g, b, a)`.
pub fn color(text: &str) -> Hsla {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).unwrap_or(0);
        return gpui::rgb(v).into();
    }
    if let Some(inner) = text.strip_prefix("rgba(").and_then(|t| t.strip_suffix(')')) {
        let p: Vec<f32> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if p.len() == 4 {
            return Rgba { r: p[0] / 255.0, g: p[1] / 255.0, b: p[2] / 255.0, a: p[3] }.into();
        }
    }
    panic!("cannot read the color {text}");
}

fn parse() -> Vec<Arc<Theme>> {
    let mut blocks: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    let mut rest = SOURCE;
    while let Some(at) = rest.find("[data-theme=\"") {
        rest = &rest[at + "[data-theme=\"".len()..];
        let id = &rest[..rest.find('"').unwrap_or(0)];
        let (Some(open), Some(close)) = (rest.find('{'), rest.find('}')) else { break };
        let vars = rest[open + 1..close].split(';').filter_map(|d| d.trim().split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect();
        blocks.insert(id, vars);
        rest = &rest[close..];
    }
    THEMES.iter().map(|(id, name)| Arc::new(build(id, name, blocks.get(id).unwrap_or_else(|| panic!("no theme {id}"))))).collect()
}

pub struct ThemeState {
    pub all: Vec<Arc<Theme>>,
    /// The theme the user chose.
    pub chosen: usize,
    /// A theme shown while the pointer is on it in the theme menu.
    pub preview: Option<usize>,
}

impl Global for ThemeState {}

pub fn init(cx: &mut App, saved: Option<&str>, system_dark: bool) {
    let all = parse();
    let fallback = if system_dark { "graftline" } else { "graftline-light" };
    let find = |id: &str| all.iter().position(|t| t.id == id);
    let chosen = saved.and_then(find).or_else(|| find(fallback)).unwrap_or(0);
    cx.set_global(ThemeState { all, chosen, preview: None });
}

/// The theme to draw with.
pub fn theme(cx: &App) -> Arc<Theme> {
    let s = cx.global::<ThemeState>();
    s.all[s.preview.unwrap_or(s.chosen)].clone()
}

pub fn chosen(cx: &App) -> Arc<Theme> {
    let s = cx.global::<ThemeState>();
    s.all[s.chosen].clone()
}

pub fn set_preview(cx: &mut App, id: Option<&str>) {
    let s = cx.global_mut::<ThemeState>();
    let next = id.and_then(|id| s.all.iter().position(|t| t.id == id));
    if s.preview != next {
        s.preview = next;
        cx.refresh_windows();
    }
}

/// Makes `id` the chosen theme. Returns false for an unknown id.
pub fn set_theme(cx: &mut App, id: &str) -> bool {
    let s = cx.global_mut::<ThemeState>();
    let Some(at) = s.all.iter().position(|t| t.id == id) else { return false };
    s.chosen = at;
    s.preview = None;
    cx.refresh_windows();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_is_read() {
        let all = parse();
        assert_eq!(all.len(), 8);
        assert!(all.iter().any(|t| t.id == "graftline"));
        assert!(all.iter().any(|t| t.id == "github-light"));
    }

    #[test]
    fn colors_are_parsed() {
        let c: Rgba = color("#ff0000").into();
        assert!((c.r - 1.0).abs() < 0.01 && c.g < 0.01);
        let c: Rgba = color("rgba(45, 212, 191, 0.14)").into();
        assert!((c.a - 0.14).abs() < 0.001);
    }
}
