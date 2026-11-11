//! Loads the commits reachable from all refs, orders them and lays the graph out.
//!
//! The result ([`History`]) is immutable and shared; the server keeps one per open repository
//! and replaces it when the refs change. Parsed commits stay in [`CommitCache`], so a rebuild
//! reads only the commits that are new. The cache is also saved to disk, so the next launch
//! does not read the object database again.

use crate::graph::{self, Layout};
use anyhow::Result;
use git2::{Oid, Repository};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

/// Hasher for maps keyed by commit id. An id is already a hash, so its first eight bytes
/// are used as they are. The default hasher costs a large part of the build on big histories.
#[derive(Default)]
pub struct OidHasher(u64);

impl Hasher for OidHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        match bytes.first_chunk::<8>() {
            Some(head) => self.0 = u64::from_ne_bytes(*head),
            None => bytes.iter().for_each(|&b| self.0 = (self.0 << 8) | b as u64),
        }
    }

    // The length prefix of a byte slice carries no information here.
    fn write_usize(&mut self, _: usize) {}
}

pub type OidMap<V> = HashMap<Oid, V, BuildHasherDefault<OidHasher>>;
pub type OidSet = HashSet<Oid, BuildHasherDefault<OidHasher>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Commit,
    Stash,
    /// The row for uncommitted changes above HEAD. Its oid is zero.
    Wip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefLabel {
    /// `head` (detached HEAD), `branch`, `remote`, `tag` or `stash`.
    pub kind: &'static str,
    pub name: String,
    /// The checked-out branch.
    pub current: bool,
}

#[derive(Debug, Clone)]
struct Raw {
    parents: Vec<Oid>,
    time: i64,
    author: u32,
    summary: Arc<str>,
}

/// A commit as read from the object database, before its author is interned.
struct Parsed {
    oid: Oid,
    parents: Vec<Oid>,
    time: i64,
    name: Box<str>,
    email: Box<str>,
    summary: Arc<str>,
}

fn parse(repo: &Repository, oid: Oid) -> Result<Parsed> {
    let c = repo.find_commit(oid)?;
    let sig = c.author();
    Ok(Parsed {
        oid,
        parents: c.parent_ids().collect(),
        time: c.time().seconds(),
        name: String::from_utf8_lossy(sig.name_bytes()).into(),
        email: String::from_utf8_lossy(sig.email_bytes()).into(),
        summary: c.summary_bytes().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default().into(),
    })
}

/// Commits a worker takes from the shared queue at once.
const BATCH: usize = 32;

/// Parsed commits and interned authors, kept between rebuilds of one repository.
#[derive(Default)]
pub struct CommitCache {
    commits: OidMap<Raw>,
    authors: Vec<(Box<str>, Box<str>)>,
    author_ids: HashMap<(Box<str>, Box<str>), u32>,
}

impl CommitCache {
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    fn intern(&mut self, name: Box<str>, email: Box<str>) -> u32 {
        let key = (name, email);
        match self.author_ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.authors.len() as u32;
                self.authors.push(key.clone());
                self.author_ids.insert(key, id);
                id
            }
        }
    }

    fn insert(&mut self, p: Parsed) {
        let author = self.intern(p.name, p.email);
        self.commits.insert(p.oid, Raw { parents: p.parents, time: p.time, author, summary: p.summary });
    }

    /// Returns the commit, reading it when the cache does not have it. `None` when the
    /// object is missing (the parent of a shallow clone's boundary commit).
    fn load(&mut self, repo: &Repository, oid: Oid) -> Option<&Raw> {
        if !self.commits.contains_key(&oid) {
            self.insert(parse(repo, oid).ok()?);
        }
        self.commits.get(&oid)
    }

    /// Commits reachable from `tips` that the cache does not have yet, found by walking the
    /// cached part of the history.
    fn missing(&self, tips: &[Oid]) -> Vec<Oid> {
        let mut seen = OidSet::default();
        let mut stack: Vec<Oid> = Vec::new();
        let mut missing = Vec::new();
        for &t in tips {
            if seen.insert(t) {
                stack.push(t);
            }
        }
        while let Some(oid) = stack.pop() {
            match self.commits.get(&oid) {
                Some(raw) => {
                    for &p in &raw.parents {
                        if seen.insert(p) {
                            stack.push(p);
                        }
                    }
                }
                None => missing.push(oid),
            }
        }
        missing
    }

    /// Reads every commit reachable from `tips` that is not cached yet, on several threads.
    /// Each thread has its own repository handle: reading and inflating commit objects is
    /// what makes the first open of a big repository slow. Returns the number of commits read.
    pub fn preload(&mut self, root: &Path, tips: &[Oid]) -> usize {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 12);
        let mut read = 0;
        // A second pass finds nothing unless a cached commit had an uncached ancestor.
        for _ in 0..4 {
            let missing = self.missing(tips);
            if missing.is_empty() {
                break;
            }
            let parsed = read_parallel(root, &self.commits, missing, threads);
            if parsed.is_empty() {
                break;
            }
            read += parsed.len();
            for p in parsed {
                self.insert(p);
            }
        }
        read
    }

    // ---- disk format: magic, author table, commit table; all integers little-endian.

    const MAGIC: &'static [u8; 8] = b"GLCACHE1";

    /// Writes the commits in `keep` (all commits when `None`) to `path`.
    pub fn save(&self, path: &Path, keep: Option<&OidSet>) -> Result<()> {
        fn put_str(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        let mut out = Vec::with_capacity(self.commits.len() * 96);
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(&(self.authors.len() as u32).to_le_bytes());
        for (name, email) in &self.authors {
            put_str(&mut out, name);
            put_str(&mut out, email);
        }
        let count_at = out.len();
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut count = 0u32;
        for (oid, raw) in &self.commits {
            // The disk format holds SHA-1 ids only.
            let sha1 = oid.as_bytes().len() == 20 && raw.parents.iter().all(|p| p.as_bytes().len() == 20);
            if !sha1 || keep.is_some_and(|k| !k.contains(oid)) {
                continue;
            }
            out.extend_from_slice(oid.as_bytes());
            out.extend_from_slice(&raw.time.to_le_bytes());
            out.extend_from_slice(&raw.author.to_le_bytes());
            out.push(raw.parents.len().min(255) as u8);
            for p in raw.parents.iter().take(255) {
                out.extend_from_slice(p.as_bytes());
            }
            put_str(&mut out, &raw.summary);
            count += 1;
        }
        out[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Reads a cache file. Any defect in the file gives an empty cache: the commits are
    /// then read from the repository again.
    pub fn load_file(path: &Path) -> CommitCache {
        std::fs::read(path).ok().and_then(|data| Self::decode(&data)).unwrap_or_default()
    }

    fn decode(data: &[u8]) -> Option<CommitCache> {
        struct Reader<'a>(&'a [u8]);
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                if self.0.len() < n {
                    return None;
                }
                let (head, rest) = self.0.split_at(n);
                self.0 = rest;
                Some(head)
            }
            fn u32(&mut self) -> Option<u32> {
                Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }
            fn str(&mut self) -> Option<Box<str>> {
                let n = self.u32()? as usize;
                Some(std::str::from_utf8(self.take(n)?).ok()?.into())
            }
        }
        let mut r = Reader(data);
        if r.take(8)? != Self::MAGIC {
            return None;
        }
        let mut cache = CommitCache::default();
        for _ in 0..r.u32()? {
            let (name, email) = (r.str()?, r.str()?);
            cache.intern(name, email);
        }
        let n = r.u32()?;
        cache.commits.reserve(n as usize);
        for _ in 0..n {
            let oid = Oid::from_bytes(r.take(20)?).ok()?;
            let time = i64::from_le_bytes(r.take(8)?.try_into().ok()?);
            let author = r.u32()?;
            if author as usize >= cache.authors.len() {
                return None;
            }
            let np = r.take(1)?[0] as usize;
            let mut parents = Vec::with_capacity(np);
            for _ in 0..np {
                parents.push(Oid::from_bytes(r.take(20)?).ok()?);
            }
            cache.commits.insert(oid, Raw { parents, time, author, summary: r.str()?.into() });
        }
        Some(cache)
    }
}

struct Queue {
    pending: Vec<Oid>,
    seen: OidSet,
    /// Workers that hold a batch and may still add commits.
    active: usize,
}

/// Walks the unknown part of the history from `start` on `threads` threads. Commits in
/// `known` are the boundary of the walk. Missing objects are skipped.
fn read_parallel(root: &Path, known: &OidMap<Raw>, start: Vec<Oid>, threads: usize) -> Vec<Parsed> {
    let queue = Mutex::new(Queue { seen: start.iter().copied().collect(), pending: start, active: 0 });
    let wake = Condvar::new();
    let worker = || -> Vec<Parsed> {
        let mut out = Vec::new();
        // Opened with the first batch: a worker that never gets work costs nothing.
        let mut repo: Option<Repository> = None;
        let mut batch: Vec<Oid> = Vec::with_capacity(BATCH);
        let mut found: Vec<Oid> = Vec::new();
        loop {
            {
                let mut q = queue.lock().unwrap();
                if !batch.is_empty() {
                    q.active -= 1;
                    batch.clear();
                }
                for p in found.drain(..) {
                    if q.seen.insert(p) {
                        q.pending.push(p);
                    }
                }
                while q.pending.is_empty() {
                    // Nothing is queued and nobody can add more: the walk is complete.
                    if q.active == 0 {
                        wake.notify_all();
                        return out;
                    }
                    q = wake.wait(q).unwrap();
                }
                let take = q.pending.len().min(BATCH);
                let at = q.pending.len() - take;
                batch.extend(q.pending.drain(at..));
                q.active += 1;
                if !q.pending.is_empty() {
                    wake.notify_all();
                }
            }
            if repo.is_none() {
                repo = Repository::open(root).ok();
            }
            if let Some(repo) = &repo {
                for &oid in &batch {
                    if let Ok(p) = parse(repo, oid) {
                        found.extend(p.parents.iter().copied().filter(|p| !known.contains_key(p)));
                        out.push(p);
                    }
                }
            }
        }
    };
    let worker = &worker;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads).map(|_| scope.spawn(worker)).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

#[derive(Debug)]
pub struct Commit {
    pub oid: Oid,
    pub time: i64,
    pub author: u32,
    pub summary: Arc<str>,
    pub kind: Kind,
}

/// Ordered commits with their graph layout and ref labels.
#[derive(Debug, Default)]
pub struct History {
    pub commits: Vec<Commit>,
    /// Parent rows of each row.
    pub parents: Vec<Vec<u32>>,
    pub layout: Layout,
    pub authors: Vec<(Box<str>, Box<str>)>,
    pub index: OidMap<u32>,
    pub labels: OidMap<Vec<RefLabel>>,
    /// Changes when refs, HEAD, stashes or the working-tree row change.
    pub fingerprint: u64,
    /// Commits read from the object database for this build (the rest came from the cache).
    pub read: usize,
}

/// Everything that decides how the graph looks, read from the repository.
pub struct Tips {
    pub tips: Vec<(Oid, Kind)>,
    pub labels: OidMap<Vec<RefLabel>>,
    /// The commit HEAD points to, if any.
    pub head: Option<Oid>,
    pub fingerprint: u64,
}

/// Collects branch, remote, tag, HEAD and stash tips with their labels.
pub fn read_tips(repo: &Repository) -> Result<Tips> {
    let mut tips = Vec::new();
    let mut labels: OidMap<Vec<RefLabel>> = OidMap::default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    let head_ref = repo.find_reference("HEAD").ok();
    let head_branch = head_ref.as_ref().and_then(|h| h.symbolic_target().ok().flatten().map(str::to_string));
    let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok()).map(|c| c.id());
    head_branch.hash(&mut hasher);
    head.map(|o| o.as_bytes().to_vec()).hash(&mut hasher);

    let mut named: Vec<(String, Oid, &'static str)> = Vec::new();
    for r in repo.references()? {
        let Ok(r) = r else { continue };
        let Some(name) = r.name().ok().map(str::to_string) else { continue };
        let kind = if name.starts_with("refs/heads/") {
            "branch"
        } else if name.starts_with("refs/remotes/") {
            "remote"
        } else if name.starts_with("refs/tags/") {
            "tag"
        } else {
            continue;
        };
        // A remote's HEAD is a pointer to another remote branch, not a branch of its own.
        if kind == "remote" && name.ends_with("/HEAD") {
            continue;
        }
        let Ok(commit) = r.peel_to_commit() else { continue };
        named.push((name, commit.id(), kind));
    }
    named.sort();
    for (name, oid, kind) in named {
        name.hash(&mut hasher);
        oid.as_bytes().hash(&mut hasher);
        let short = match kind {
            "branch" => &name["refs/heads/".len()..],
            "remote" => &name["refs/remotes/".len()..],
            _ => &name["refs/tags/".len()..],
        };
        let current = head_branch.as_deref() == Some(name.as_str());
        labels.entry(oid).or_default().push(RefLabel { kind, name: short.to_string(), current });
        tips.push((oid, Kind::Commit));
    }
    if let Some(oid) = head {
        tips.push((oid, Kind::Commit));
        if head_branch.is_none() || repo.head_detached().unwrap_or(false) {
            labels.entry(oid).or_default().insert(0, RefLabel { kind: "head", name: "HEAD".into(), current: true });
        }
    }
    for (i, (oid, _)) in stashes(repo).into_iter().enumerate() {
        oid.as_bytes().hash(&mut hasher);
        labels.entry(oid).or_default().push(RefLabel { kind: "stash", name: format!("stash@{{{i}}}"), current: false });
        tips.push((oid, Kind::Stash));
    }
    // Current branch first, then local branches, remotes, tags.
    for list in labels.values_mut() {
        let rank = |l: &RefLabel| match (l.current, l.kind) {
            (true, _) => 0,
            (_, "head") => 1,
            (_, "branch") => 2,
            (_, "remote") => 3,
            (_, "tag") => 4,
            _ => 5,
        };
        list.sort_by_key(rank);
    }
    Ok(Tips { tips, labels, head, fingerprint: hasher.finish() })
}

/// Stash entries, newest first: (commit, message).
pub fn stashes(repo: &Repository) -> Vec<(Oid, String)> {
    let Ok(log) = repo.reflog("refs/stash") else { return Vec::new() };
    log.iter().map(|e| (e.id_new(), e.message().ok().flatten().unwrap_or("").to_string())).filter(|(o, _)| !o.is_zero()).collect()
}

/// Builds the history. `wip` adds the working-tree row above HEAD.
pub fn build(repo: &Repository, cache: &mut CommitCache, tips: Tips, wip: bool) -> Result<History> {
    // Read what is missing, then collect every reachable commit.
    let tip_oids: Vec<Oid> = tips.tips.iter().map(|t| t.0).collect();
    let before = cache.len();
    cache.preload(repo.workdir().unwrap_or_else(|| repo.path()), &tip_oids);

    // `ids` maps a commit to its number in visit order.
    let mut ids: OidMap<u32> = OidMap::default();
    let mut oids: Vec<Oid> = Vec::new();
    let mut kinds: Vec<Kind> = Vec::new();
    let mut stack: Vec<Oid> = Vec::new();
    for &(oid, kind) in &tips.tips {
        if !ids.contains_key(&oid) && cache.load(repo, oid).is_some() {
            ids.insert(oid, oids.len() as u32);
            oids.push(oid);
            kinds.push(kind);
            stack.push(oid);
        }
    }
    let mut parent_oids: Vec<Vec<Oid>> = vec![Vec::new(); oids.len()];
    while let Some(oid) = stack.pop() {
        let me = ids[&oid] as usize;
        let all = cache.commits[&oid].parents.clone();
        // A stash commit also has the index and untracked-files commits as parents.
        // Only its base belongs in the graph.
        let take = if kinds[me] == Kind::Stash { 1 } else { all.len() };
        let mut parents = Vec::with_capacity(take);
        for &p in all.iter().take(take) {
            if !ids.contains_key(&p) {
                // A parent that is not in the object database: the edge of a shallow clone.
                if cache.load(repo, p).is_none() {
                    continue;
                }
                ids.insert(p, oids.len() as u32);
                oids.push(p);
                kinds.push(Kind::Commit);
                parent_oids.push(Vec::new());
                stack.push(p);
            }
            parents.push(p);
        }
        parent_oids[me] = parents;
    }
    let read = cache.len() - before;

    let mut times: Vec<i64> = oids.iter().map(|o| cache.commits[o].time).collect();
    let mut parents: Vec<Vec<u32>> = parent_oids.iter().map(|ps| ps.iter().map(|p| ids[p]).collect()).collect();
    let mut pinned = Vec::new();
    if wip {
        // The working-tree row sits above HEAD; on an unborn branch it stands alone.
        let id = oids.len() as u32;
        oids.push(Oid::ZERO_SHA1);
        kinds.push(Kind::Wip);
        times.push(i64::MAX);
        parents.push(tips.head.and_then(|h| ids.get(&h).copied()).into_iter().collect());
        pinned.push(id);
    }

    let order = graph::topo_order(&parents, &times, &pinned);
    let mut row_of = vec![0u32; order.len()];
    for (row, &i) in order.iter().enumerate() {
        row_of[i as usize] = row as u32;
    }
    let mut commits = Vec::with_capacity(order.len());
    let mut row_parents = Vec::with_capacity(order.len());
    let mut index = OidMap::with_capacity_and_hasher(order.len(), Default::default());
    for (row, &i) in order.iter().enumerate() {
        let i = i as usize;
        let oid = oids[i];
        let (time, author, summary) = match kinds[i] {
            Kind::Wip => (0, u32::MAX, Arc::from("")),
            _ => {
                let raw = &cache.commits[&oid];
                (raw.time, raw.author, raw.summary.clone())
            }
        };
        commits.push(Commit { oid, time, author, summary, kind: kinds[i] });
        row_parents.push(parents[i].iter().map(|&p| row_of[p as usize]).collect::<Vec<u32>>());
        index.insert(oid, row as u32);
    }
    let layout = graph::layout(&row_parents);
    let fingerprint = fingerprint(&tips, wip);
    Ok(History { commits, parents: row_parents, layout, authors: cache.authors.clone(), index, labels: tips.labels, fingerprint, read })
}

/// Identifies the state a history is built from: refs, HEAD, stashes, working-tree row.
pub fn fingerprint(tips: &Tips, wip: bool) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tips.fingerprint.hash(&mut hasher);
    wip.hash(&mut hasher);
    hasher.finish()
}

impl History {
    pub fn author(&self, id: u32) -> (&str, &str) {
        self.authors.get(id as usize).map_or(("", ""), |(n, e)| (n, e))
    }

    /// The ids of all commits in the history, for trimming the disk cache.
    pub fn oids(&self) -> OidSet {
        self.commits.iter().map(|c| c.oid).collect()
    }

    /// Rows whose summary, author or hash contain `query` (case-insensitive).
    pub fn search(&self, query: &str) -> Vec<u32> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let is_hex = q.len() >= 4 && q.bytes().all(|b| b.is_ascii_hexdigit());
        let author_hit: Vec<bool> =
            self.authors.iter().map(|(n, e)| n.to_lowercase().contains(&q) || e.to_lowercase().contains(&q)).collect();
        let mut rows = Vec::new();
        for (row, c) in self.commits.iter().enumerate() {
            if c.kind == Kind::Wip {
                continue;
            }
            let hit = author_hit.get(c.author as usize).copied().unwrap_or(false)
                || contains_ci(&c.summary, &q)
                || (is_hex && c.oid.to_string().starts_with(&q));
            if hit {
                rows.push(row as u32);
            }
        }
        rows
    }
}

/// Case-insensitive `contains`; `needle` is already lowercase.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() && needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        n.len() <= h.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
    } else {
        hay.to_lowercase().contains(needle)
    }
}
