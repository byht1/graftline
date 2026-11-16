//! Development tool: measures how long the commit graph of a repository takes to load.

use clap::Parser;
use graftline::{history, repo};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "graftline-bench", version, about = "Measures the commit graph load time of a repository")]
struct Cli {
    /// The repository to measure.
    repo: PathBuf,
}

/// The work the window does for the first screen: the layout of 200 rows.
fn first_page(h: &history::History) -> usize {
    h.layout.rows(0, h.commits.len().min(200)).len()
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
    let page = first_page(&h);
    let t_page = t0.elapsed();
    println!("repository:        {}", root.display());
    println!("commits:           {}", h.commits.len());
    println!("graph width:       {} lanes", h.layout.width);
    println!("refs read:         {:>7.1} ms", t_refs.as_secs_f64() * 1e3);
    println!(
        "history + layout:  {:>7.1} ms ({} threads)",
        (t_build - t_refs).as_secs_f64() * 1e3,
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(12)
    );
    println!("first page:        {:>7.1} ms ({page} rows)", (t_page - t_build).as_secs_f64() * 1e3);
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
    let _ = first_page(&h3);
    let _ = std::fs::remove_file(&file);
    println!("cache file:        {:>7.1} ms to write, {} KB", t_save.as_secs_f64() * 1e3, size / 1024);
    println!("cache load:        {:>7.1} ms", t_load.as_secs_f64() * 1e3);
    println!("total, next launch:{:>7.1} ms ({} commits read from the repository)", t4.elapsed().as_secs_f64() * 1e3, h3.read);
    let t2 = Instant::now();
    let hits = h.search("fix").len();
    println!("search \"fix\":      {:>7.1} ms ({hits} matches)", t2.elapsed().as_secs_f64() * 1e3);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // The tests use this program as the editor of an interactive rebase, as git uses the app.
    if let Some(code) = graftline::gitcli::run_as_editor() {
        std::process::exit(code);
    }
    bench(&Cli::parse().repo)
}
