//! Commit graph layout: assigns a column and a color to every commit and records the lines
//! between commits. The UI only draws what this module returns.
//!
//! Input: commits in display order where every parent comes after all of its children
//! (see [`topo_order`]). A lane keeps its column for its whole life, so a line that passes
//! a row is always vertical; only the lines that enter or leave a commit node bend.
//!
//! Rules:
//! - The first parent continues the lane of its child. A first-parent chain (a branch)
//!   therefore stays in one column.
//! - Another parent (a merge) joins a lane that already leads to it, or opens a new lane
//!   in the leftmost free column.
//! - Lanes that lead to the same commit end in it; the commit takes the leftmost column.
//!
//! Only the lines that start or end in a row are stored. Lanes that pass a row are rebuilt
//! on demand from snapshots taken every [`SNAPSHOT_EVERY`] rows, which keeps memory small
//! on histories with many parallel branches.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Number of branch colors a theme defines (`--graph-1` … `--graph-8`).
pub const COLORS: u8 = 8;
const SNAPSHOT_EVERY: usize = 256;
const NONE: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// A lane that passes the row in its column, top to bottom.
    Through = 0,
    /// A lane that comes from the top of the row in `col` and ends in the commit node.
    In = 1,
    /// A line from the commit node to the bottom of the row in `col` (towards a parent).
    Out = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub col: u16,
    pub color: u8,
}

#[derive(Debug, Clone, Copy)]
struct RowHead {
    col: u16,
    color: u8,
    /// Index of the row's first stored edge.
    start: u32,
}

/// One row, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowGraph {
    pub col: u16,
    pub color: u8,
    pub edges: Vec<Edge>,
}

/// Layout of all rows.
#[derive(Debug, Default)]
pub struct Layout {
    rows: Vec<RowHead>,
    /// `In` and `Out` edges of all rows.
    edges: Vec<Edge>,
    /// Lanes (column, color) that are active at the top of every `SNAPSHOT_EVERY`-th row.
    snapshots: Vec<Vec<(u16, u8)>>,
    /// Number of columns the widest row uses.
    pub width: u16,
}

impl Layout {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Column of the commit node in `row`.
    pub fn col(&self, row: usize) -> u16 {
        self.rows[row].col
    }

    /// Color index of the commit node in `row`.
    pub fn color(&self, row: usize) -> u8 {
        self.rows[row].color
    }

    fn stored(&self, row: usize) -> &[Edge] {
        let start = self.rows[row].start as usize;
        let end = self.rows.get(row + 1).map_or(self.edges.len(), |r| r.start as usize);
        &self.edges[start..end]
    }

    /// Rows `from..to` with all their edges, including the lanes that pass through.
    pub fn rows(&self, from: usize, to: usize) -> Vec<RowGraph> {
        let to = to.min(self.rows.len());
        if from >= to {
            return Vec::new();
        }
        let snap = from / SNAPSHOT_EVERY;
        let mut lanes: Vec<Option<u8>> = Vec::new();
        for &(col, color) in &self.snapshots[snap] {
            if lanes.len() <= col as usize {
                lanes.resize(col as usize + 1, None);
            }
            lanes[col as usize] = Some(color);
        }
        let mut out = Vec::with_capacity(to - from);
        for row in snap * SNAPSHOT_EVERY..to {
            let stored = self.stored(row);
            for e in stored.iter().filter(|e| e.kind == EdgeKind::In) {
                lanes[e.col as usize] = None;
            }
            if row >= from {
                let mut edges: Vec<Edge> = stored.iter().filter(|e| e.kind == EdgeKind::In).copied().collect();
                edges.extend(
                    lanes.iter().enumerate().filter_map(|(c, l)| l.map(|color| Edge { kind: EdgeKind::Through, col: c as u16, color })),
                );
                edges.extend(stored.iter().filter(|e| e.kind == EdgeKind::Out));
                out.push(RowGraph { col: self.rows[row].col, color: self.rows[row].color, edges });
            }
            for e in stored.iter().filter(|e| e.kind == EdgeKind::Out) {
                if lanes.len() <= e.col as usize {
                    lanes.resize(e.col as usize + 1, None);
                }
                lanes[e.col as usize] = Some(e.color);
            }
        }
        out
    }

    /// All edges of one row.
    pub fn edges(&self, row: usize) -> Vec<Edge> {
        self.rows(row, row + 1).pop().map(|r| r.edges).unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
struct Lane {
    color: u8,
    /// The next lane with the same target, or `NONE`.
    next: u16,
}

#[derive(Default)]
struct Lanes {
    slots: Vec<Option<Lane>>,
    /// Free columns below `slots.len()`.
    free: BinaryHeap<Reverse<u16>>,
    /// Target row -> first lane that leads to it.
    head: HashMap<u32, u16>,
}

impl Lanes {
    /// The leftmost free column.
    fn alloc(&mut self) -> u16 {
        match self.free.pop() {
            Some(Reverse(c)) => c,
            None => {
                self.slots.push(None);
                (self.slots.len() - 1).min(NONE as usize - 1) as u16
            }
        }
    }

    fn release(&mut self, col: u16) {
        self.slots[col as usize] = None;
        self.free.push(Reverse(col));
    }

    fn set(&mut self, col: u16, target: u32, color: u8) {
        let next = self.head.insert(target, col).unwrap_or(NONE);
        self.slots[col as usize] = Some(Lane { color, next });
    }
}

/// Computes the layout. `parents[i]` holds the row indices of the parents of row `i`;
/// every parent index must be greater than `i`.
pub fn layout(parents: &[Vec<u32>]) -> Layout {
    let mut out =
        Layout { rows: Vec::with_capacity(parents.len()), edges: Vec::with_capacity(parents.len() * 2 + 16), ..Default::default() };
    let mut lanes = Lanes::default();
    let mut next_color: u8 = 0;
    let mut new_color = || {
        let c = next_color;
        next_color = (next_color + 1) % COLORS;
        c
    };
    let mut ending: Vec<(u16, u8)> = Vec::new();
    let mut max_col: u16 = 0;

    for (i, row_parents) in parents.iter().enumerate() {
        if i % SNAPSHOT_EVERY == 0 {
            out.snapshots.push(lanes.slots.iter().enumerate().filter_map(|(c, l)| l.map(|l| (c as u16, l.color))).collect());
        }
        let i = i as u32;
        let start = out.edges.len() as u32;

        // Lanes that lead to this commit end here. The leftmost one gives the node its column.
        ending.clear();
        let mut at = lanes.head.remove(&i).unwrap_or(NONE);
        while at != NONE {
            let lane = lanes.slots[at as usize].expect("a linked lane exists");
            ending.push((at, lane.color));
            at = lane.next;
        }
        ending.sort_unstable();
        let (col, color) = match ending.first() {
            Some(&first) => first,
            // No lane leads to it: the commit is a branch tip and opens a new lane.
            None => (lanes.alloc(), new_color()),
        };
        for &(c, lane_color) in &ending {
            out.edges.push(Edge { kind: EdgeKind::In, col: c, color: lane_color });
            if c != col {
                lanes.release(c);
            }
        }
        lanes.slots[col as usize] = None;
        max_col = max_col.max(col);

        let mut node_lane_used = false;
        for (k, &p) in row_parents.iter().enumerate() {
            debug_assert!(p > i, "parents must come after children");
            if k == 0 {
                // The first parent continues the lane of the commit.
                lanes.set(col, p, color);
                node_lane_used = true;
                out.edges.push(Edge { kind: EdgeKind::Out, col, color });
            } else if let Some(&c) = lanes.head.get(&p) {
                // A lane already leads to this parent: join it.
                let lane_color = lanes.slots[c as usize].map_or(color, |l| l.color);
                // Two parents may be the same commit; the edge is drawn once.
                if !out.edges[start as usize..].iter().any(|e| e.kind == EdgeKind::Out && e.col == c) {
                    out.edges.push(Edge { kind: EdgeKind::Out, col: c, color: lane_color });
                }
            } else {
                let c = lanes.alloc();
                let lane_color = new_color();
                lanes.set(c, p, lane_color);
                max_col = max_col.max(c);
                out.edges.push(Edge { kind: EdgeKind::Out, col: c, color: lane_color });
            }
        }
        if !node_lane_used {
            // A root commit: its column is free again.
            lanes.free.push(Reverse(col));
        }
        out.rows.push(RowHead { col, color, start });
    }
    out.width = if parents.is_empty() { 0 } else { max_col + 1 };
    out
}

/// Orders commits so that every commit comes before its parents, newest first
/// (the order of `git log --date-order`). `parents[i]` holds indices into the same slice;
/// `times[i]` is the commit time. `pinned` lists commits that come first when possible
/// (the working-tree row). Returns the order as indices into the input.
pub fn topo_order(parents: &[Vec<u32>], times: &[i64], pinned: &[u32]) -> Vec<u32> {
    let n = parents.len();
    let mut children = vec![0u32; n];
    for ps in parents {
        for &p in ps {
            children[p as usize] += 1;
        }
    }
    let key = |i: u32| {
        let pin = pinned.contains(&i);
        // Max-heap: pinned first, then newer commits, then the lower input index.
        (pin, times[i as usize], Reverse(i))
    };
    let mut heap: BinaryHeap<(bool, i64, Reverse<u32>)> = BinaryHeap::new();
    for i in 0..n as u32 {
        if children[i as usize] == 0 {
            heap.push(key(i));
        }
    }
    let mut order = Vec::with_capacity(n);
    while let Some((_, _, Reverse(i))) = heap.pop() {
        order.push(i);
        for &p in &parents[i as usize] {
            children[p as usize] -= 1;
            if children[p as usize] == 0 {
                heap.push(key(p));
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(l: &Layout, row: usize, kind: EdgeKind) -> Vec<u16> {
        let mut v: Vec<u16> = l.edges(row).iter().filter(|e| e.kind == kind).map(|e| e.col).collect();
        v.sort_unstable();
        v
    }

    /// Every line that leaves the bottom of a row must continue at the top of the next row
    /// with the same color, and the other way round. This holds for any correct layout.
    fn assert_continuous(l: &Layout) {
        for r in 0..l.len() {
            let mut bottom: Vec<(u16, u8)> = l.edges(r).iter().filter(|e| e.kind != EdgeKind::In).map(|e| (e.col, e.color)).collect();
            bottom.sort_unstable();
            bottom.dedup();
            let mut top: Vec<(u16, u8)> = if r + 1 < l.len() {
                l.edges(r + 1).iter().filter(|e| e.kind != EdgeKind::Out).map(|e| (e.col, e.color)).collect()
            } else {
                Vec::new()
            };
            top.sort_unstable();
            top.dedup();
            assert_eq!(bottom, top, "lines break between rows {r} and {}", r + 1);
        }
    }

    /// Checks the layout against the input: every commit sends one line towards each parent
    /// and receives lines only from its children.
    fn assert_matches_parents(l: &Layout, parents: &[Vec<u32>]) {
        for (r, ps) in parents.iter().enumerate() {
            let mut distinct = ps.clone();
            distinct.sort_unstable();
            distinct.dedup();
            let outs = kinds(l, r, EdgeKind::Out);
            assert!(outs.len() <= distinct.len() && (outs.is_empty() == distinct.is_empty()), "row {r}: {outs:?} for parents {ps:?}");
            let has_children = parents[..r].iter().any(|c| c.contains(&(r as u32)));
            assert_eq!(!kinds(l, r, EdgeKind::In).is_empty(), has_children, "row {r}: incoming lines");
            assert!(!kinds(l, r, EdgeKind::Through).contains(&l.col(r)), "row {r}: a lane crosses the node");
        }
    }

    #[test]
    fn linear_history_uses_one_column() {
        let parents = [vec![1], vec![2], vec![]];
        let l = layout(&parents);
        assert_eq!(l.width, 1);
        assert_eq!((l.col(0), l.col(1), l.col(2)), (0, 0, 0));
        assert_eq!(l.color(0), l.color(2));
        assert_eq!(kinds(&l, 0, EdgeKind::Out), vec![0]);
        assert_eq!(kinds(&l, 1, EdgeKind::In), vec![0]);
        assert!(l.edges(2).iter().all(|e| e.kind == EdgeKind::In));
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn merge_opens_a_second_lane_and_closes_it_at_the_fork_point() {
        // 0 = merge(1, 2); 1 -> 3; 2 -> 3; 3 root
        let parents = [vec![1, 2], vec![3], vec![3], vec![]];
        let l = layout(&parents);
        assert_eq!(l.width, 2);
        assert_eq!(l.col(0), 0);
        assert_eq!(kinds(&l, 0, EdgeKind::Out), vec![0, 1]);
        assert_eq!(l.col(1), 0);
        assert_eq!(kinds(&l, 1, EdgeKind::Through), vec![1]);
        assert_eq!(l.col(2), 1);
        assert_ne!(l.color(1), l.color(2));
        // Both lanes end in the fork point, which takes the leftmost column.
        assert_eq!(l.col(3), 0);
        assert_eq!(kinds(&l, 3, EdgeKind::In), vec![0, 1]);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn octopus_merge_gives_each_parent_its_own_lane() {
        // 0 = merge(1, 2, 3); all three have root 4
        let parents = [vec![1, 2, 3], vec![4], vec![4], vec![4], vec![]];
        let l = layout(&parents);
        assert_eq!(l.width, 3);
        assert_eq!(kinds(&l, 0, EdgeKind::Out), vec![0, 1, 2]);
        assert_eq!((l.col(1), l.col(2), l.col(3)), (0, 1, 2));
        let colors = [l.color(1), l.color(2), l.color(3)];
        assert!(colors[0] != colors[1] && colors[1] != colors[2] && colors[0] != colors[2]);
        assert_eq!(kinds(&l, 4, EdgeKind::In), vec![0, 1, 2]);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn several_roots_reuse_the_freed_column() {
        // Two unrelated histories: 0 -> 1 (root), 2 -> 3 (root)
        let parents = [vec![1], vec![], vec![3], vec![]];
        let l = layout(&parents);
        assert_eq!(l.width, 1);
        assert_eq!(l.col(2), 0);
        assert_ne!(l.color(0), l.color(2));
        assert!(kinds(&l, 1, EdgeKind::Out).is_empty());
        assert!(kinds(&l, 2, EdgeKind::In).is_empty());
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn two_tips_on_one_base_keep_their_columns_until_the_base() {
        // 0 and 1 are branch tips, both children of 2
        let parents = [vec![2], vec![2], vec![]];
        let l = layout(&parents);
        assert_eq!((l.col(0), l.col(1), l.col(2)), (0, 1, 0));
        assert_eq!(kinds(&l, 1, EdgeKind::Through), vec![0]);
        assert_eq!(kinds(&l, 1, EdgeKind::Out), vec![1]);
        assert_eq!(kinds(&l, 2, EdgeKind::In), vec![0, 1]);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn first_parent_chain_stays_in_its_column() {
        // main: 0 -> 2 -> 4; feature: 1 -> 3 -> 4. The feature lane exists when main reaches 4,
        // but main must not bend into it.
        let parents = [vec![2], vec![3], vec![4], vec![4], vec![]];
        let l = layout(&parents);
        assert_eq!((l.col(0), l.col(2), l.col(4)), (0, 0, 0));
        assert_eq!((l.col(1), l.col(3)), (1, 1));
        assert_eq!(kinds(&l, 2, EdgeKind::Out), vec![0]);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn merge_parent_joins_a_lane_that_already_leads_to_it() {
        // 0 -> 2; 1 = merge(3, 2): the second parent 2 already has lane 0.
        let parents = [vec![2], vec![3, 2], vec![3], vec![]];
        let l = layout(&parents);
        assert_eq!(l.col(1), 1);
        assert_eq!(kinds(&l, 1, EdgeKind::Out), vec![0, 1]);
        assert_eq!(l.width, 2);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn a_freed_column_is_reused_by_the_next_branch() {
        // 0 = merge(1, 2), both end in 3; then tip 4 -> 5 starts after the lanes closed... the
        // order must keep parents after children, so the second history comes last.
        let parents = [vec![1, 2], vec![3], vec![3], vec![], vec![5], vec![]];
        let l = layout(&parents);
        assert_eq!(l.col(4), 0);
        assert_eq!(l.width, 2);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn rows_can_be_read_from_any_offset() {
        // Long enough to cross several snapshots, with lanes that stay open across them.
        let n = SNAPSHOT_EVERY as u32 * 3 + 17;
        let mut parents: Vec<Vec<u32>> = Vec::new();
        for i in 0..n {
            // Two interleaved chains that share the last commit, plus a merge now and then.
            let mut ps = Vec::new();
            if i + 2 < n {
                ps.push(i + 2);
            } else if i + 1 < n {
                ps.push(i + 1);
            }
            if i % 50 == 0 && i + 7 < n {
                ps.push(i + 7);
            }
            parents.push(ps);
        }
        let l = layout(&parents);
        assert_continuous(&l);
        let all = l.rows(0, l.len());
        assert_eq!(all.len(), l.len());
        for from in [1, 100, SNAPSHOT_EVERY - 1, SNAPSHOT_EVERY, SNAPSHOT_EVERY + 1, 2 * SNAPSHOT_EVERY + 5] {
            let part = l.rows(from, from + 40);
            assert_eq!(part, all[from..from + 40], "rows from {from}");
        }
        assert!(l.rows(l.len(), l.len() + 10).is_empty());
    }

    #[test]
    fn layout_is_consistent_on_a_dense_generated_history() {
        // Deterministic pseudo-random DAG with merges and several roots.
        let n = 2000u32;
        let mut seed = 0x2545_f491u32;
        let mut rnd = move |m: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % m
        };
        let mut parents = Vec::new();
        for i in 0..n {
            let mut ps = Vec::new();
            let left = n - 1 - i;
            if left > 0 && rnd(40) != 0 {
                ps.push(i + 1 + rnd(left.min(6)));
                if rnd(5) == 0 && left > 1 {
                    let p = i + 1 + rnd(left.min(30));
                    if !ps.contains(&p) {
                        ps.push(p);
                    }
                }
            }
            parents.push(ps);
        }
        let l = layout(&parents);
        assert_eq!(l.len(), n as usize);
        assert_continuous(&l);
        assert_matches_parents(&l, &parents);
    }

    #[test]
    fn topo_order_puts_children_first_even_with_skewed_clocks() {
        // 0 -> 1 -> 2, but commit 1 has an older time than its parent 2.
        let parents = vec![vec![1], vec![2], vec![]];
        let order = topo_order(&parents, &[30, 5, 20], &[]);
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn topo_order_interleaves_branches_by_time_and_pins_first() {
        // tips 0 (t=10) and 1 (t=50), common parent 2 (t=1); 3 is a pinned extra tip (t=0) on 2
        let parents = vec![vec![2], vec![2], vec![], vec![2]];
        let order = topo_order(&parents, &[10, 50, 1, 0], &[3]);
        assert_eq!(order, vec![3, 1, 0, 2]);
    }
}
