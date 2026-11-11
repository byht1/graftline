# Graftline

A desktop Git client built around the commit graph. One binary, written in Rust, for
Windows, macOS and Linux.

> Work in progress. This file grows with each stage; the full documentation comes with the
> first release.

## Run from source

```
cargo run                       # the desktop app
cargo run -- path/to/repo       # open a repository on start
cargo run --bin graftline-server -- --port 7878   # the same UI in a browser (development)
```

Building needs a stable Rust toolchain and a C compiler (libgit2 is built from source).
Node.js is not needed.

## Performance

Measured, not estimated. Repository: [git/git](https://github.com/git/git), 86,230 commits
reachable from all refs, 1,018 tags. Machine: Windows 11, 12 logical cores,
release build.

| Step | Time |
|---|---|
| First open: read all commits (8 threads), order them, lay out the graph, first page of 200 rows | 0.75 s |
| Later launches (commits come from the disk cache) | 0.27 s |
| Refresh after a change in the repository (commits already in memory) | 0.21 s |
| Search 86,230 commits for a word | 0.02 s |
| Scrolling, 240 frames at 260 px per frame | median 16.5 ms, 95th percentile 22.6 ms |

The first three rows are `graftline-server --bench <repo>`; the scroll numbers come from the
running UI. Run the benchmark on your own repository to compare.

How it stays fast:

- Commits are read once, on several threads, and cached on disk per repository.
- The graph layout is computed in Rust. The UI receives rows in pages and draws only the
  visible ones on a canvas.
- Lanes that pass a row are not stored. They are rebuilt from snapshots taken every 256
  rows, so memory does not grow with the number of parallel branches.
