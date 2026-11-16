//! Graftline desktop app: a native window drawn with GPUI, without a console or a browser.
//! The Git logic is the `graftline` library; it runs on a tokio runtime next to the UI thread.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ui;

use clap::Parser;
use graftline::{default_data_dir, AppState, Options, APP_ID};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "graftline", version, about = "Graftline: a desktop Git client")]
struct Cli {
    /// Repositories (or folders inside them) to open on start.
    paths: Vec<PathBuf>,
    /// Directory for recent repositories, preferences and window state.
    #[arg(long, env = "GRAFTLINE_DATA_DIR")]
    data_dir: Option<PathBuf>,
}

fn init_logging(data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| format!("{APP_ID}=info,warn").into());
    match std::fs::File::create(data_dir.join(format!("{APP_ID}.log"))) {
        Ok(file) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

fn main() -> anyhow::Result<()> {
    // git starts this program as the editor of an interactive rebase.
    if let Some(code) = graftline::gitcli::run_as_editor() {
        std::process::exit(code);
    }
    let cli = Cli::parse();
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    init_logging(&data_dir);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name(format!("{APP_ID}-core")).build()?;
    let st = {
        let _guard = rt.enter();
        AppState::new(Options { data_dir, desktop: true })
    };
    ui::run(st, rt.handle().clone(), cli.paths);
    Ok(())
}
