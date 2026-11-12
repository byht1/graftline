//! Every theme in assets/ui/themes.css must define the same variables, and its graph colors
//! must stand out from the backgrounds they are drawn on.

use std::collections::BTreeMap;

/// WCAG 2 asks for 3:1 for graphical objects such as the lines and nodes of the graph.
const MIN_CONTRAST: f64 = 3.0;

fn themes() -> BTreeMap<String, BTreeMap<String, String>> {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui/themes.css")).unwrap();
    let mut out = BTreeMap::new();
    let mut rest = css.as_str();
    while let Some(at) = rest.find(":root[data-theme=\"") {
        rest = &rest[at + ":root[data-theme=\"".len()..];
        let name = rest[..rest.find('"').unwrap()].to_string();
        let open = rest.find('{').unwrap();
        // Only blocks that define a theme, not rules such as `:root[data-theme="light"] .x`.
        if rest[..open].trim_end().ends_with("\"]") {
            let close = rest.find('}').unwrap();
            let vars: BTreeMap<String, String> = rest[open + 1..close]
                .split(';')
                .filter_map(|d| d.trim().split_once(':'))
                .filter(|(k, _)| k.trim().starts_with("--"))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            if !vars.is_empty() {
                out.insert(name, vars);
            }
        }
    }
    out
}

fn rgb(hex: &str) -> (f64, f64, f64) {
    let h = hex.trim().trim_start_matches('#');
    assert_eq!(h.len(), 6, "expected a #rrggbb color, got {hex}");
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64 / 255.0;
    (c(0), c(2), c(4))
}

fn luminance(hex: &str) -> f64 {
    let lin = |v: f64| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = rgb(hex);
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn all_eight_themes_exist_and_define_the_same_variables() {
    let themes = themes();
    let names: Vec<&str> = themes.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["dark", "dracula", "github-light", "graftline", "graftline-light", "light", "nord", "one-dark-pro"]);
    let reference: Vec<&String> = themes["graftline"].keys().collect();
    for (name, vars) in &themes {
        let keys: Vec<&String> = vars.keys().collect();
        assert_eq!(keys, reference, "theme {name} defines a different set of variables");
    }
    for i in 1..=8 {
        assert!(themes["graftline"].contains_key(&format!("--graph-{i}")));
    }
}

#[test]
fn graph_colors_have_enough_contrast_in_every_theme() {
    let mut failures = Vec::new();
    for (name, vars) in themes() {
        for i in 1..=8 {
            let color = &vars[&format!("--graph-{i}")];
            // The graph is drawn on the page background; ref labels sit on it too.
            for bg in ["--bg", "--panel"] {
                let ratio = contrast(color, &vars[bg]);
                if ratio < MIN_CONTRAST {
                    failures.push(format!("{name}: --graph-{i} {color} on {bg} {} = {ratio:.2}", vars[bg]));
                }
            }
        }
    }
    assert!(failures.is_empty(), "graph colors below {MIN_CONTRAST}:1:\n{}", failures.join("\n"));
}

#[test]
fn graph_colors_of_a_theme_are_distinct() {
    for (name, vars) in themes() {
        let mut colors: Vec<&String> = (1..=8).map(|i| &vars[&format!("--graph-{i}")]).collect();
        colors.sort();
        colors.dedup();
        assert_eq!(colors.len(), 8, "theme {name} repeats a graph color");
    }
}

#[test]
fn text_colors_are_readable_in_every_theme() {
    for (name, vars) in themes() {
        let text = contrast(&vars["--text"], &vars["--bg"]);
        // WCAG AA for body text. One Dark Pro's own palette gives 6.6:1, below AAA.
        assert!(text >= 4.5, "{name}: --text on --bg = {text:.2}");
        let muted = contrast(&vars["--muted"], &vars["--bg"]);
        assert!(muted >= 4.5, "{name}: --muted on --bg = {muted:.2}");
    }
}
