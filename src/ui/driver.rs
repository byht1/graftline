//! Debug builds only: runs UI commands from a text file, so the interface can be checked
//! without the keyboard of the person at the computer. The file is named by
//! `GRAFTLINE_UI_DRIVER`; each new line is one command:
//!
//! ```text
//! key KEYSTROKE          (for example ctrl-p, enter, escape)
//! type TEXT
//! open PATH
//! theme ID
//! ```
//!
//! Mouse input is sent to the window from outside (window messages), not from here.

use super::shell::Shell;
use gpui::prelude::*;
use gpui::{Keystroke, Modifiers, Window};
use std::time::Duration;

pub fn start(window: &mut Window, cx: &mut Context<Shell>) {
    let Ok(file) = std::env::var("GRAFTLINE_UI_DRIVER") else { return };
    let handle = window.window_handle();
    cx.spawn(async move |shell, cx| {
        let mut done = 0usize;
        loop {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            // A shorter file is a new script.
            if lines.len() < done {
                done = 0;
            }
            let Some(line) = lines.get(done).map(|l| l.trim().to_string()) else { continue };
            // The last line is run only when it ends with a line break (it is complete).
            if done + 1 == lines.len() && !text.ends_with('\n') {
                continue;
            }
            done += 1;
            let shell = shell.clone();
            let ok = handle.update(cx, |_, window, cx| {
                run(&line, &shell, window, cx);
                window.refresh();
            });
            if ok.is_err() {
                break;
            }
        }
    })
    .detach();
}

fn run(line: &str, shell: &gpui::WeakEntity<Shell>, window: &mut Window, cx: &mut gpui::App) {
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
    let none = Modifiers::default();
    match cmd {
        "key" => {
            if let Ok(keystroke) = Keystroke::parse(rest.trim()) {
                window.dispatch_keystroke(keystroke, cx);
            }
        }
        "type" => {
            for ch in rest.chars() {
                let key = if ch == ' ' { "space".to_string() } else { ch.to_string() };
                window.dispatch_keystroke(Keystroke { modifiers: none, key, key_char: Some(ch.to_string()) }, cx);
            }
        }
        "open" => {
            let path = rest.trim().to_string();
            let _ = shell.update(cx, |shell, cx| shell.open_repo(path, true, false, cx));
        }
        "theme" => {
            super::theme::set_theme(cx, rest.trim());
        }
        _ => tracing::warn!("driver: unknown command {line}"),
    }
}
