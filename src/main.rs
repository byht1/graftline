//! Development tool: serves the Graftline UI to a browser on a fixed port.
//! The desktop app (`src/bin/desktop.rs`) is the product; this binary exists to work on the
//! UI with browser dev tools and to measure performance from the command line.

use clap::Parser;
use graftline::{default_data_dir, history, repo, router, Options};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "graftline-server", version, about = "Graftline UI in a browser (development tool)")]
struct Cli {
    /// Repositories to open on start.
    paths: Vec<PathBuf>,
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Directory for recent repositories and preferences.
    #[arg(long, env = "GRAFTLINE_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Measure how long the commit graph of a repository takes to load, then exit.
    #[arg(long, value_name = "REPO")]
    bench: Option<PathBuf>,
}

fn bench(path: &std::path::Path) -> anyhow::Result<()> {
    let t0 = Instant::now();
    let root = repo::discover(path)?;
    let r = repo::open(&root)?;
    let tips = history::read_tips(&r)?;
    let t_refs = t0.elapsed();
    let mut cache = history::CommitCache::default();
    let h = history::build(&r, &mut cache, tips, false)?;
    let t_build = t0.elapsed();
    let page = serde_json::to_string(&graftline::graph_rows(&h, 0, 200))?;
    let t_page = t0.elapsed();
    println!("repository:        {}", root.display());
    println!("commits:           {}", h.commits.len());
    println!("graph width:       {} lanes", h.layout.width);
    println!("refs read:         {:>7.1} ms", t_refs.as_secs_f64() * 1e3);
    println!("history + layout:  {:>7.1} ms ({} threads)", (t_build - t_refs).as_secs_f64() * 1e3, std::thread::available_parallelism().map_or(1, |n| n.get()).min(12));
    println!("first page (200):  {:>7.1} ms ({} KB of JSON)", (t_page - t_build).as_secs_f64() * 1e3, page.len() / 1024);
    println!("total, first open: {:>7.1} ms", t_page.as_secs_f64() * 1e3);
    let t1 = Instant::now();
    let tips = history::read_tips(&r)?;
    let h2 = history::build(&r, &mut cache, tips, false)?;
    println!("rebuild, in memory:{:>7.1} ms ({} commits)", t1.elapsed().as_secs_f64() * 1e3, h2.commits.len());
    // The next launch: the commit cache comes from disk.
    let file = std::env::temp_dir().join(format!("graftline-bench-{}.commits", std::process::id()));
    let t3 = Instant::now();
    cache.save(&file, Some(&h.oids()))?;
    let t_save = t3.elapsed();
    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let t4 = Instant::now();
    let r2 = repo::open(&root)?;
    let mut cache2 = history::CommitCache::load_file(&file);
    let t_load = t4.elapsed();
    let tips = history::read_tips(&r2)?;
    let h3 = history::build(&r2, &mut cache2, tips, false)?;
    let _ = serde_json::to_string(&graftline::graph_rows(&h3, 0, 200))?;
    let _ = std::fs::remove_file(&file);
    println!("cache file:        {:>7.1} ms to write, {} KB", t_save.as_secs_f64() * 1e3, size / 1024);
    println!("cache load:        {:>7.1} ms", t_load.as_secs_f64() * 1e3);
    println!("total, next launch:{:>7.1} ms ({} commits read from the repository)", t4.elapsed().as_secs_f64() * 1e3, h3.read);
    let t2 = Instant::now();
    let hits = h.search("fix").len();
    println!("search \"fix\":      {:>7.1} ms ({hits} matches)", t2.elapsed().as_secs_f64() * 1e3);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(path) = &cli.bench {
        return bench(path);
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "graftline=info,warn".into())).init();
    let (app, _) = router(Options { data_dir: cli.data_dir.unwrap_or_else(default_data_dir), app_token: None, open: cli.paths });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cli.port)).await?;
    println!("Graftline UI: http://127.0.0.1:{}", cli.port);
    axum::serve(listener, app).await?;
    Ok(())
}
