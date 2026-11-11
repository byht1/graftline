// Graftline — the commit graph: a virtual list of rows plus one canvas that draws the lanes.
// The server computes the layout (src/graph.rs); this file only draws the visible rows.

import { h, icon, api, qs, store, avatar, relTime, fmtDate, dragResize, onThemeChange } from '/ui.js';

const ROW = 28;        // row height, px
const PAGE = 200;      // rows per request
const LANE = 14;       // distance between lanes, px
const PAD = 12;        // left padding of the graph column, px
const OVERSCAN = 8;    // rows rendered above and below the viewport
const MAX_PAGES = 80;  // pages kept in memory
// Browsers limit the height of an element. Above this height the list maps scroll position
// to rows proportionally instead of 1:1.
const MAX_SCROLL = 12_000_000;
const THROUGH = 0, IN = 1, OUT = 2;
const DEFAULT_COLS = { refs: 136, graph: 0, author: 140, date: 96, sha: 68 };
const AUTO_LANES = 10; // lanes the graph column shows before the user resizes it

export function createGraphView({ repoId, onSelect, onContext }) {
  const st = {
    total: 0, width: 0, version: null,
    pages: new Map(), loading: new Map(),
    selected: null,   // oid of the selected row; '' is the working-tree row
    selRow: -1,
    matches: null,    // Set of matching rows while a search is active
    matchList: [],
    cols: { ...DEFAULT_COLS, ...store.get('gl.graphCols', {}) },
    colors: null,
    raf: 0,
    gen: 0,           // increases on every reload; late answers of an older generation are dropped
  };

  const canvas = h('canvas');
  const space = h('div', { class: 'g-space' });
  const scroller = h('div', { class: 'g-scroll' }, h('div', { class: 'g-sticky' }, canvas), space);
  const head = h('div', { class: 'g-head' });
  const root = h('div', { class: 'graph', tabindex: '0' }, head, scroller);
  const rowEls = new Map();

  // ---------------------------------------------------------------- geometry
  const graphWidth = () => st.cols.graph || Math.min(Math.max(st.width, 2), AUTO_LANES) * LANE + PAD * 2 - LANE + 8;
  const laneX = (col) => PAD + col * LANE;
  const fullHeight = () => st.total * ROW;
  const spaceHeight = () => Math.min(fullHeight(), MAX_SCROLL);
  /** Scroll position in row coordinates (equal to scrollTop unless the list is scaled). */
  function virtualTop() {
    const view = scroller.clientHeight;
    const maxScroll = spaceHeight() - view;
    if (fullHeight() <= MAX_SCROLL || maxScroll <= 0) return scroller.scrollTop;
    return (scroller.scrollTop / maxScroll) * (fullHeight() - view);
  }
  function setVirtualTop(vt) {
    const view = scroller.clientHeight;
    const maxScroll = spaceHeight() - view;
    if (fullHeight() <= MAX_SCROLL || maxScroll <= 0) scroller.scrollTop = vt;
    else scroller.scrollTop = (vt / (fullHeight() - view)) * maxScroll;
  }

  function applyCols() {
    root.style.setProperty('--w-refs', `${st.cols.refs}px`);
    root.style.setProperty('--w-graph', `${graphWidth()}px`);
    root.style.setProperty('--w-author', `${st.cols.author}px`);
    root.style.setProperty('--w-date', `${st.cols.date}px`);
    root.style.setProperty('--w-sha', `${st.cols.sha}px`);
    canvas.style.left = `${st.cols.refs}px`;
  }

  function buildHead() {
    const cell = (cls, label, key, sign = 1, min = 60) => {
      const el = h('div', { class: `g-hcell ${cls}` }, label);
      if (key) {
        const res = h('div', { class: 'g-hres' });
        if (sign < 0) { res.style.right = 'auto'; res.style.left = '0'; res.style.borderRight = '0'; res.style.borderLeft = '1px solid var(--border)'; }
        dragResize(res, {
          onStart: () => (key === 'graph' ? graphWidth() : st.cols[key]),
          onMove: (dx, _dy, w0) => { st.cols[key] = Math.max(min, Math.min(900, w0 + dx * sign)); applyCols(); schedule(); },
          onEnd: () => store.set('gl.graphCols', st.cols),
        });
        res.addEventListener('dblclick', () => { st.cols[key] = DEFAULT_COLS[key]; applyCols(); schedule(); store.set('gl.graphCols', st.cols); });
        el.append(res);
      }
      return el;
    };
    head.replaceChildren(
      cell('g-refs', 'Branch / Tag', 'refs', 1, 40),
      cell('g-graph', 'Graph', 'graph', 1, 30),
      cell('flex', 'Message'),
      cell('g-author', 'Author', 'author', -1, 60),
      cell('g-date', 'Date'),
      cell('g-sha', 'Hash'),
    );
  }

  // ---------------------------------------------------------------- data
  const rowData = (i) => st.pages.get(Math.floor(i / PAGE))?.[i % PAGE];

  function loadPage(p) {
    if (st.pages.has(p)) return Promise.resolve(st.pages.get(p));
    if (st.loading.has(p)) return st.loading.get(p);
    const gen = st.gen;
    const promise = api(`/repos/${repoId}/graph?${qs({ offset: p * PAGE, limit: PAGE })}`).then((res) => {
      st.loading.delete(p);
      if (gen !== st.gen) return null;
      if (st.version && res.version !== st.version) { reload(); return null; }
      st.version = res.version;
      if (st.total !== res.total || st.width !== res.width) {
        st.total = res.total;
        st.width = res.width;
        space.style.height = `${spaceHeight()}px`;
        applyCols();
      }
      st.pages.set(p, res.rows);
      if (st.pages.size > MAX_PAGES) evict(p);
      schedule();
      return res.rows;
    }, (e) => { st.loading.delete(p); throw e; });
    st.loading.set(p, promise);
    return promise;
  }

  // Drops the pages farthest from the page just loaded.
  function evict(keep) {
    const far = [...st.pages.keys()].sort((a, b) => Math.abs(b - keep) - Math.abs(a - keep));
    for (const p of far.slice(0, st.pages.size - MAX_PAGES)) st.pages.delete(p);
  }

  // ---------------------------------------------------------------- rows
  function refLabel(l, color) {
    const ic = l.kind === 'tag' ? 'tag' : l.kind === 'remote' ? 'cloud' : l.kind === 'stash' ? 'stash' : l.kind === 'head' ? 'commit' : l.current ? 'check' : 'branch';
    return h('span', { class: `ref ${l.kind}${l.current ? ' current' : ''} c${color + 1}`, title: l.name, dataset: { ref: l.name, kind: l.kind } }, icon(ic, 11), h('span', {}, l.name));
  }

  function buildRow(i, d) {
    const el = h('div', { class: 'g-row', dataset: { row: i } });
    if (!d) {
      el.append(h('div', { class: 'g-cell g-refs' }), h('div', { class: 'g-cell g-graph' }), h('div', { class: 'g-cell g-msg' }, h('div', { class: 'g-skel' })));
      return el;
    }
    el.dataset.loaded = '1';
    if (d.k === 'wip') el.classList.add('wip');
    if (d.k === 'stash') el.classList.add('stash');
    const labels = d.r || [];
    const shown = labels.slice(0, 2).map((l) => refLabel(l, d.cl));
    if (labels.length > 2) shown.push(h('span', { class: 'ref more', title: labels.slice(2).map((l) => l.name).join('\n') }, `+${labels.length - 2}`));
    el.append(
      h('div', { class: 'g-cell g-refs' }, shown),
      h('div', { class: 'g-cell g-graph' }),
      h('div', { class: 'g-cell g-msg' }, h('span', { class: 'g-sum' }, d.k === 'wip' ? 'Uncommitted changes' : d.s || '(no message)')),
      h('div', { class: 'g-cell g-author' }, d.k === 'wip' ? null : [avatar(d.a, d.ae), h('span', { title: d.ae }, d.a)]),
      h('div', { class: 'g-cell g-date', title: d.k === 'wip' ? null : fmtDate(d.t) }, d.k === 'wip' ? '' : relTime(d.t)),
      h('div', { class: 'g-cell g-sha' }, d.o.slice(0, 7)),
    );
    return el;
  }

  function decorate(el, i, d) {
    el.classList.toggle('sel', !!d && st.selected !== null && d.o === st.selected);
    el.classList.toggle('match', !!st.matches && st.matches.has(i));
    el.classList.toggle('dimmed', !!st.matches && !st.matches.has(i));
  }

  function renderRows() {
    const view = scroller.clientHeight;
    const vt = virtualTop();
    const shift = vt - scroller.scrollTop;
    const first = Math.max(0, Math.floor(vt / ROW) - OVERSCAN);
    const last = Math.min(st.total - 1, Math.ceil((vt + view) / ROW) + OVERSCAN);
    for (const [i, el] of rowEls) {
      if (i < first || i > last) { el.remove(); rowEls.delete(i); }
    }
    if (st.total > 0) {
      for (let p = Math.floor(first / PAGE); p <= Math.floor(last / PAGE); p++) loadPage(p).catch(() => {});
    }
    for (let i = first; i <= last; i++) {
      const d = rowData(i);
      let el = rowEls.get(i);
      if (el && !el.dataset.loaded && d) { el.remove(); el = null; }
      if (!el) {
        el = buildRow(i, d);
        rowEls.set(i, el);
        space.append(el);
      }
      el.style.top = `${i * ROW - shift}px`;
      decorate(el, i, d);
    }
  }

  // ---------------------------------------------------------------- canvas
  function readColors() {
    const cs = getComputedStyle(root);
    st.colors = {
      lanes: Array.from({ length: 8 }, (_, i) => cs.getPropertyValue(`--graph-${i + 1}`).trim() || '#888'),
      bg: cs.getPropertyValue('--bg').trim() || '#000',
      muted: cs.getPropertyValue('--muted').trim() || '#888',
    };
  }

  function draw() {
    if (!st.colors) readColors();
    const dpr = window.devicePixelRatio || 1;
    const w = graphWidth();
    const view = scroller.clientHeight;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(view * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(view * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${view}px`;
    }
    const ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, view);
    if (!st.total) return;
    const vt = virtualTop();
    const first = Math.max(0, Math.floor(vt / ROW));
    const last = Math.min(st.total - 1, Math.floor((vt + view) / ROW));
    const mid = ROW / 2;
    const R = 7;
    const paths = st.colors.lanes.map(() => new Path2D());
    const nodes = [];
    for (let i = first; i <= last; i++) {
      const d = rowData(i);
      if (!d) continue;
      const y = i * ROW - vt;
      const nx = laneX(d.c);
      const e = d.e;
      for (let j = 0; j < e.length; j += 3) {
        const kind = e[j];
        const x = laneX(e[j + 1]);
        // Lanes right of the visible column are clipped; skip them.
        if (Math.min(x, nx) - LANE > w) continue;
        const path = paths[e[j + 2] % paths.length];
        if (kind === THROUGH) {
          path.moveTo(x, y);
          path.lineTo(x, y + ROW);
        } else if (kind === IN) {
          path.moveTo(x, y);
          if (x === nx) path.lineTo(nx, y + mid);
          else { path.arcTo(x, y + mid, nx, y + mid, Math.min(R, Math.abs(nx - x))); path.lineTo(nx, y + mid); }
        } else {
          path.moveTo(nx, y + mid);
          if (x === nx) path.lineTo(x, y + ROW);
          else { path.arcTo(x, y + mid, x, y + ROW, Math.min(R, Math.abs(nx - x))); path.lineTo(x, y + ROW); }
        }
      }
      nodes.push([d, nx, y + mid]);
    }
    ctx.lineWidth = 2;
    ctx.lineJoin = 'round';
    paths.forEach((p, c) => { ctx.strokeStyle = st.colors.lanes[c]; ctx.stroke(p); });
    for (const [d, x, y] of nodes) {
      const color = st.colors.lanes[d.cl % st.colors.lanes.length];
      if (x > w - 6) {
        // The node lies right of the visible column: show an arrow at the edge instead.
        ctx.fillStyle = st.colors.bg;
        ctx.fillRect(w - 13, y - 7, 13, 14);
        ctx.beginPath(); ctx.moveTo(w - 10, y - 4.5); ctx.lineTo(w - 4, y); ctx.lineTo(w - 10, y + 4.5);
        ctx.strokeStyle = color; ctx.lineWidth = 2; ctx.stroke();
        continue;
      }
      const head = (d.r || []).some((l) => l.current);
      ctx.fillStyle = st.colors.bg;
      ctx.strokeStyle = color;
      if (d.k === 'wip') {
        ctx.beginPath(); ctx.arc(x, y, 5, 0, Math.PI * 2); ctx.fill();
        ctx.setLineDash([2.5, 2]); ctx.lineWidth = 1.6; ctx.strokeStyle = st.colors.muted; ctx.stroke(); ctx.setLineDash([]);
      } else if (d.k === 'stash') {
        ctx.beginPath(); ctx.rect(x - 4.5, y - 4.5, 9, 9); ctx.fill(); ctx.lineWidth = 2; ctx.stroke();
      } else if (head) {
        ctx.beginPath(); ctx.arc(x, y, 6, 0, Math.PI * 2); ctx.fill(); ctx.lineWidth = 2; ctx.stroke();
        ctx.beginPath(); ctx.arc(x, y, 2.6, 0, Math.PI * 2); ctx.fillStyle = color; ctx.fill();
      } else {
        const r = d.p.length > 1 ? 3.4 : 4.6;
        ctx.beginPath(); ctx.arc(x, y, r + 2, 0, Math.PI * 2); ctx.fill();
        ctx.beginPath(); ctx.arc(x, y, r, 0, Math.PI * 2); ctx.fillStyle = color; ctx.fill();
      }
    }
  }

  function schedule() {
    if (st.raf) return;
    st.raf = requestAnimationFrame(() => { st.raf = 0; renderRows(); draw(); });
  }

  // ---------------------------------------------------------------- selection
  function rowFromEvent(e) {
    const el = e.target.closest?.('.g-row');
    return el ? Number(el.dataset.row) : -1;
  }

  function selectRow(i, { notify = true, reveal = false } = {}) {
    if (i < 0 || i >= st.total) return;
    const apply = (d) => {
      if (!d) return;
      st.selRow = i;
      st.selected = d.o;
      if (reveal) revealRow(i);
      schedule();
      if (notify) onSelect?.(d, i);
    };
    const d = rowData(i);
    if (d) apply(d);
    else loadPage(Math.floor(i / PAGE)).then(() => apply(rowData(i))).catch(() => {});
  }

  /** Scrolls so that row `i` is visible; `center` puts it in the middle. */
  function revealRow(i, center = false) {
    const view = scroller.clientHeight;
    const vt = virtualTop();
    const top = i * ROW;
    if (center) setVirtualTop(Math.max(0, top - view / 2 + ROW / 2));
    else if (top < vt) setVirtualTop(top);
    else if (top + ROW > vt + view) setVirtualTop(top + ROW - view);
    schedule();
  }

  space.addEventListener('click', (e) => { const i = rowFromEvent(e); if (i >= 0) selectRow(i); });
  space.addEventListener('contextmenu', (e) => {
    const i = rowFromEvent(e);
    const d = i >= 0 ? rowData(i) : null;
    if (!d) return;
    selectRow(i);
    const ref = e.target.closest?.('.ref[data-ref]');
    onContext?.(e, d, ref ? { name: ref.dataset.ref, kind: ref.dataset.kind } : null);
  });
  scroller.addEventListener('scroll', schedule, { passive: true });
  root.addEventListener('keydown', (e) => {
    if (e.target !== root) return;
    const page = Math.max(1, Math.floor(scroller.clientHeight / ROW) - 1);
    const cur = st.selRow < 0 ? Math.floor(virtualTop() / ROW) - 1 : st.selRow;
    const to = { ArrowDown: cur + 1, ArrowUp: cur - 1, PageDown: cur + page, PageUp: cur - page, Home: 0, End: st.total - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    selectRow(Math.max(0, Math.min(st.total - 1, to)), { reveal: true });
  });
  // A narrow pane drops the hash column first, then the date.
  const resizeObs = new ResizeObserver(() => {
    const w = root.clientWidth;
    root.classList.toggle('no-sha', w < 860);
    root.classList.toggle('no-date', w < 700);
    schedule();
  });
  resizeObs.observe(scroller);
  const offTheme = onThemeChange(() => { readColors(); schedule(); });

  // ---------------------------------------------------------------- public
  async function locate(oids) {
    const res = await api(`/repos/${repoId}/graph/locate?${qs({ oids: oids.join(',') })}`);
    return res.rows;
  }

  /** First load. */
  async function init() {
    buildHead();
    applyCols();
    await loadPage(0);
    schedule();
  }

  let reloading = null;
  /**
   * Loads the graph again after the repository changed. The row at the top of the viewport
   * stays where it is: its commit is found in the new graph and the scroll position follows.
   * The old rows stay on screen until the new ones are loaded, so nothing flickers.
   */
  function reload() {
    if (reloading) return reloading;
    reloading = (async () => {
      const vt = virtualTop();
      const anchorRow = Math.floor(vt / ROW);
      const anchor = rowData(anchorRow);
      const offset = vt - anchorRow * ROW;
      const gen = st.gen + 1;
      const firstPage = await api(`/repos/${repoId}/graph?${qs({ offset: 0, limit: PAGE })}`);
      let newTop = null, newSel = st.selRow;
      const want = [anchor?.o, st.selected].map((o) => o || '');
      if (want.some(Boolean)) {
        const rows = await api(`/repos/${repoId}/graph/locate?${qs({ oids: want.join(',') })}`).then((r) => r.rows, () => []);
        if (anchor?.o && rows[0] != null) newTop = rows[0] * ROW + offset;
        newSel = st.selected ? (rows[1] ?? -1) : (st.selected === '' ? 0 : -1);
      }
      // At the top of the list, stay at the top: new commits appear there.
      if (anchorRow === 0 && offset === 0) newTop = 0;
      if (newTop == null) newTop = Math.min(vt, Math.max(0, firstPage.total * ROW - scroller.clientHeight));
      const view = scroller.clientHeight;
      const need = new Set([Math.floor(newTop / ROW / PAGE), Math.floor((newTop + view) / ROW / PAGE)]);
      const loaded = new Map([[0, firstPage.rows]]);
      await Promise.all([...need].filter((p) => p > 0 && p * PAGE < firstPage.total).map(async (p) => {
        const res = await api(`/repos/${repoId}/graph?${qs({ offset: p * PAGE, limit: PAGE })}`);
        if (res.version === firstPage.version) loaded.set(p, res.rows);
      }));
      // Swap everything in one step.
      st.gen = gen;
      st.loading.clear();
      st.pages = loaded;
      st.version = firstPage.version;
      st.total = firstPage.total;
      st.width = firstPage.width;
      st.selRow = newSel;
      if (newSel < 0 && st.selected !== null) st.selected = null;
      if (st.matches) { st.matches = null; st.matchList = []; }
      for (const el of rowEls.values()) el.remove();
      rowEls.clear();
      space.style.height = `${spaceHeight()}px`;
      applyCols();
      setVirtualTop(newTop);
      renderRows();
      draw();
      return { selectionLost: newSel < 0 };
    })().finally(() => { reloading = null; });
    return reloading;
  }

  /** Selects a commit by id and scrolls to it. Returns false when the graph does not show it. */
  async function selectOid(oid, opts = {}) {
    const [row] = await locate([oid]);
    if (row == null) return false;
    revealRow(row, true);
    selectRow(row, opts);
    return true;
  }

  /** Marks the rows that match a search. `rows` is a sorted list; null clears the search. */
  function setMatches(rows) {
    st.matchList = rows || [];
    st.matches = rows ? new Set(rows) : null;
    schedule();
  }

  /** Moves to the next (dir = 1) or previous (dir = -1) match. Returns its position, 1-based. */
  function gotoMatch(dir) {
    const list = st.matchList;
    if (!list.length) return 0;
    const cur = st.selRow;
    let idx;
    if (dir > 0) { idx = list.findIndex((r) => r > cur); if (idx < 0) idx = 0; }
    else { idx = list.findLastIndex((r) => r < cur); if (idx < 0) idx = list.length - 1; }
    revealRow(list[idx], true);
    selectRow(list[idx]);
    return idx + 1;
  }

  function destroy() {
    resizeObs.disconnect();
    offTheme();
    cancelAnimationFrame(st.raf);
  }

  return {
    el: root, init, reload, selectOid, selectRow, setMatches, gotoMatch, destroy,
    focus: () => root.focus({ preventScroll: true }),
    get total() { return st.total; },
    get selected() { return st.selected; },
    get matchCount() { return st.matchList.length; },
    redraw: schedule,
  };
}
