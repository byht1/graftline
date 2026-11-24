//! The window of the app, drawn with GPUI. The core (`graftline` library) runs on a tokio
//! runtime; `call` moves a request to it and brings the answer back to the UI thread.

pub mod actions;
pub mod changes;
pub mod detail;
pub mod diff_view;
#[cfg(debug_assertions)]
pub mod driver;
pub mod graph_view;
pub mod highlight;
pub mod history_ui;
pub mod icons;
pub mod input;
pub mod overlay;
pub mod repo_view;
pub mod shell;
pub mod sidebar;
pub mod terminal;
pub mod theme;
pub mod tools;
pub mod widgets;

use gpui::{App, AssetSource, Global, SharedString};
use graftline::Shared;
use std::borrow::Cow;
use std::future::Future;
use std::path::PathBuf;

pub const UI_FONT: &str = if cfg!(target_os = "windows") {
    "Segoe UI"
} else if cfg!(target_os = "macos") {
    ".SystemUIFont"
} else {
    "DejaVu Sans"
};
pub const MONO_FONT: &str = if cfg!(target_os = "windows") {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// The core and the runtime it lives on.
pub struct Core {
    pub st: Shared,
    pub rt: tokio::runtime::Handle,
}

impl Global for Core {}

pub fn core(cx: &App) -> Shared {
    cx.global::<Core>().st.clone()
}

/// Runs a request on the core's runtime. The returned future can be awaited on the UI thread.
pub fn call<T, Fut>(cx: &App, f: impl FnOnce(Shared) -> Fut + Send + 'static) -> impl Future<Output = anyhow::Result<T>> + 'static
where
    T: Send + 'static,
    Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    let core = cx.global::<Core>();
    let (tx, rx) = futures::channel::oneshot::channel();
    let st = core.st.clone();
    core.rt.spawn(async move {
        let _ = tx.send(f(st).await);
    });
    async move { rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("The operation stopped unexpectedly"))) }
}

/// UI preferences: a copy of `ui-prefs.json` on the UI thread. Values are JSON text under
/// `gl.*` keys. A change is written through the core in the background.
#[derive(Default)]
pub struct Prefs(std::collections::BTreeMap<String, String>);

impl Global for Prefs {}

pub fn pref<T: serde::de::DeserializeOwned>(cx: &App, key: &str) -> Option<T> {
    cx.try_global::<Prefs>().and_then(|p| p.0.get(key)).and_then(|text| serde_json::from_str(text).ok())
}

pub fn set_pref<T: serde::Serialize>(cx: &mut App, key: &str, value: &T) {
    let Ok(text) = serde_json::to_string(value) else { return };
    if cx.global::<Prefs>().0.get(key) == Some(&text) {
        return;
    }
    cx.global_mut::<Prefs>().0.insert(key.to_string(), text.clone());
    let key = key.to_string();
    let task = call(cx, move |st| async move { st.set_pref(&key, Some(text)).await });
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = task.await {
                tracing::warn!("cannot save the preferences: {e:#}");
            }
        })
        .detach();
}

/// Icons and brand images, embedded in the binary.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == "brand/symbol.svg" {
            return Ok(Some(Cow::Borrowed(include_bytes!("../../assets/brand/symbol.svg"))));
        }
        let name = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg"));
        Ok(name.and_then(|n| icons::ICONS.iter().find(|(k, _)| *k == n)).map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// Starts the UI. Does not return until the app quits.
pub fn run(st: Shared, rt: tokio::runtime::Handle, open: Vec<PathBuf>) {
    gpui::Application::new().with_assets(Assets).run(move |cx: &mut App| {
        let prefs = rt.block_on(st.prefs());
        cx.set_global(Prefs(prefs));
        cx.set_global(Core { st, rt });
        shell::open_window(cx, open);
    });
}
