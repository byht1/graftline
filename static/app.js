// Graftline — single-page UI (no build step): tabs, sidebar, start page, commit details.

import {
  APP_NAME, $, h, fill, icon, env, store, api, qs, toast, copyText, openMenu, closeMenu, menuItems, contextMenu, openModal,
  currentTheme, openThemeMenu, fmtDate, relTime, fmtNum, avatar, dragResize,
} from '/ui.js';
import { createGraphView } from '/graph.js';
import { createDiffPanel } from '/diff.js';

// ---------------------------------------------------------------- state
const state = {
  info: null,
  tabs: [],        // open repositories, see makeTab()
  active: 'home',  // tab id or 'home'
  recent: [],
};
const hashParams = new URLSearchParams(location.hash.slice(1));
const activeTab = () => state.tabs.find((t) => t.id === state.active) || null;

function saveTabs() {
  store.set('gl.tabs', state.tabs.map((t) => t.path));
  store.set('gl.active', activeTab()?.path || null);
}

// ---------------------------------------------------------------- boot
async function boot() {
  initSidebarResizer();
  try {
    state.info = await api('/info');
    env.info = state.info;
  } catch (e) {
    $('#main').replaceChildren(h('div', { class: 'empty-main' }, icon('alert', 32), h('h2', {}, `Cannot start ${APP_NAME}`), h('p', { class: 'muted' }, e.message)));
    return;
  }
  loadRecent();
  render();
  connectEvents();
  // Tabs of the last session, then repositories from the command line.
  const saved = store.get('gl.tabs', []);
  const wanted = store.get('gl.active', null);
  const extra = [...(state.info.startup || []), ...hashParams.getAll('open')];
  for (const path of saved) await openRepo(path, { activate: false, quiet: true });
  const again = state.tabs.find((t) => t.path === wanted);
  if (again && !extra.length) activate(again.id);
  for (const path of extra) await openRepo(path);
  if (!state.tabs.length) render();
  const commit = hashParams.get('commit');
  if (commit && activeTab()) {
    await activeTab().ready;
    await activeTab().graph.selectOid(commit);
  }
}

async function loadRecent() {
  try { state.recent = await api('/recent'); } catch { state.recent = []; }
  if (state.active === 'home') renderMain();
}

function connectEvents() {
  const es = new EventSource('/api/events');
  es.onmessage = (m) => {
    let ev;
    try { ev = JSON.parse(m.data); } catch { return; }
    if (ev.type === 'repo') {
      const tab = state.tabs.find((t) => t.id === ev.id);
      if (tab) tab.onRepoEvent(ev);
    }
  };
}

// A folder dropped on the desktop window (see src/bin/desktop.rs).
window.__appDrop = (paths) => { for (const p of paths || []) openRepo(p); };

// ---------------------------------------------------------------- repositories
async function openRepo(path, { activate: act = true, quiet = false } = {}) {
  let res;
  try {
    res = await api('/repos/open', { body: { path } });
  } catch (e) {
    if (!quiet) toast(e.message, 'error');
    else saveTabs();
    return null;
  }
  let tab = state.tabs.find((t) => t.id === res.id);
  if (!tab) {
    tab = makeTab(res);
    state.tabs.push(tab);
  }
  if (act) activate(tab.id); else renderTabBar();
  saveTabs();
  loadRecent();
  return tab;
}

async function pickAndOpen() {
  if (state.info?.desktop) {
    try {
      const r = await api('/dialog/folder', { body: {} });
      if (r.path) await openRepo(r.path);
    } catch (e) { toast(e.message, 'error'); }
    return;
  }
  // Browser (development) mode has no native dialog: ask for a path.
  const input = h('input', { class: 'input', placeholder: 'C:\\projects\\my-repo or /home/me/my-repo', style: { width: '100%' } });
  const go = () => { const p = input.value.trim(); if (p) { close(); openRepo(p); } };
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); });
  const close = openModal('Open a repository', h('div', { class: 'form' }, h('label', {}, 'Path to the repository or a folder inside it', input)),
    [h('button', { class: 'btn', onclick: () => close() }, 'Cancel'), h('button', { class: 'btn primary', onclick: go }, 'Open')]);
  queueMicrotask(() => input.focus());
}

function closeTab(id) {
  const i = state.tabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  const [tab] = state.tabs.splice(i, 1);
  tab.destroy();
  api(`/repos/${id}`, { method: 'DELETE' }).catch(() => {});
  if (state.active === id) state.active = (state.tabs[i] || state.tabs[i - 1])?.id || 'home';
  saveTabs();
  render();
}

function activate(id) {
  state.active = id;
  saveTabs();
  render();
  activeTab()?.graph.focus();
}

// ---------------------------------------------------------------- one repository tab
function makeTab({ id, path, name }) {
  const tab = {
    id, path, name,
    refs: null,
    refsError: null,
    filter: '',
    detail: null,     // { oid, data } of the commit shown in the detail panel
    diffPanel: null,
    search: { q: '', timer: 0, pos: 0 },
  };

  // ---- elements
  const center = h('div', { class: 'center' });
  const detailEl = h('div', { class: 'detail' });
  const titleEl = h('div', { class: 'tb-title' });
  const searchInput = h('input', { class: 'input', type: 'search', placeholder: 'Search commits: message, author, hash', spellcheck: 'false' });
  const searchCount = h('span', { class: 'search-count' });
  const prevBtn = h('button', { class: 'icon-btn', title: 'Previous match (Shift+Enter)', disabled: true, onclick: () => step(-1) }, icon('up'));
  const nextBtn = h('button', { class: 'icon-btn', title: 'Next match (Enter)', disabled: true, onclick: () => step(1) }, icon('down'));
  const noticeEl = h('div', { class: 'notice', hidden: true });
  const resizer = h('div', { class: 'pane-resizer' });
  tab.el = h('div', { class: 'repo-view' },
    h('div', { class: 'toolbar' },
      h('div', { class: 'tb-group' }, titleEl),
      h('div', { class: 'tb-group' },
        h('div', { class: 'search-box' }, icon('search'), searchInput), searchCount, prevBtn, nextBtn,
        h('button', { class: 'icon-btn', title: 'Refresh (F5)', onclick: () => tab.refresh(true) }, icon('refresh')))),
    noticeEl,
    h('div', { class: 'repo-body' }, center, resizer, detailEl));

  const detailW = store.get('gl.detailW', 380);
  detailEl.style.width = `${detailW}px`;
  dragResize(resizer, {
    onStart: () => detailEl.offsetWidth,
    onMove: (dx, _dy, w0) => { detailEl.style.width = `${Math.max(260, Math.min(window.innerWidth - 420, w0 - dx))}px`; },
    onEnd: () => store.set('gl.detailW', detailEl.offsetWidth),
  });

  // ---- graph
  tab.graph = createGraphView({
    repoId: id,
    onSelect: (row) => showCommit(row),
    onContext: (e, row, ref) => commitMenu(e, row, ref),
  });
  center.append(tab.graph.el);
  tab.ready = tab.graph.init().catch((e) => {
    center.replaceChildren(h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, e.message)));
  });

  // ---- title
  function drawTitle() {
    const head = tab.refs?.head;
    fill(titleEl, icon('repo'), h('span', {}, tab.name),
      head ? h('span', { class: `badge${head.detached ? ' warn' : ''}`, title: head.detached ? 'HEAD is detached: it points to a commit, not a branch' : 'Current branch' },
        icon(head.detached ? 'commit' : 'branch', 11), head.detached ? (head.oid || '').slice(0, 7) : head.branch || '?') : null,
      tab.refs?.state ? h('span', { class: 'badge warn', title: `A ${tab.refs.state} is in progress` }, tab.refs.state) : null);
  }

  // ---- refs
  let refsTimer = 0;
  async function loadRefs() {
    try {
      tab.refs = await api(`/repos/${id}/refs`);
      tab.refsError = null;
    } catch (e) {
      tab.refsError = e.message;
    }
    drawTitle();
    if (state.active === id) { renderSidebar(); document.title = `${tab.name} — ${APP_NAME}`; }
  }

  tab.refresh = async (manual = false) => {
    await loadRefs();
    try {
      await tab.graph.reload();
      if (manual) toast('Refreshed');
    } catch (e) { toast(e.message, 'error'); }
  };

  tab.onRepoEvent = (ev) => {
    if (ev.git) { clearTimeout(refsTimer); refsTimer = setTimeout(loadRefs, 60); }
    if (ev.graph) tab.graph.reload().then((r) => { if (r?.selectionLost) showCommit(null); }).catch(() => {});
  };

  // ---- search
  async function runSearch() {
    const q = searchInput.value.trim();
    tab.search.q = q;
    if (!q) {
      tab.graph.setMatches(null);
      searchCount.textContent = '';
      prevBtn.disabled = nextBtn.disabled = true;
      return;
    }
    try {
      const { rows } = await api(`/repos/${id}/graph/search?${qs({ q })}`);
      if (tab.search.q !== q) return;
      tab.graph.setMatches(rows);
      prevBtn.disabled = nextBtn.disabled = !rows.length;
      tab.search.pos = 0;
      searchCount.textContent = rows.length ? `${fmtNum(rows.length)} found` : 'No matches';
    } catch (e) { toast(e.message, 'error'); }
  }
  function step(dir) {
    const pos = tab.graph.gotoMatch(dir);
    if (pos) searchCount.textContent = `${fmtNum(pos)} / ${fmtNum(tab.graph.matchCount)}`;
  }
  searchInput.addEventListener('input', () => { clearTimeout(tab.search.timer); tab.search.timer = setTimeout(runSearch, 180); });
  searchInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); clearTimeout(tab.search.timer); (tab.search.q === searchInput.value.trim() ? Promise.resolve() : runSearch()).then(() => step(e.shiftKey ? -1 : 1)); }
    if (e.key === 'Escape') { searchInput.value = ''; runSearch(); tab.graph.focus(); }
  });
  tab.focusSearch = () => { searchInput.focus(); searchInput.select(); };

  // ---- commit details
  let detailReq = 0;
  async function showCommit(row) {
    closeDiff();
    const req = ++detailReq;
    if (!row || row.k === 'wip') {
      tab.detail = null;
      drawDetail();
      return;
    }
    if (tab.detail?.oid === row.o && tab.detail.data) return;
    tab.detail = { oid: row.o, data: null, error: null };
    drawDetail();
    try {
      const data = await api(`/repos/${id}/commit/${row.o}`);
      if (req !== detailReq) return;
      tab.detail.data = data;
    } catch (e) {
      if (req !== detailReq) return;
      tab.detail.error = e.message;
    }
    drawDetail();
    const file = hashParams.get('file');
    if (file && tab.detail?.data) {
      hashParams.delete('file');
      const f = tab.detail.data.files.find((x) => x.path === file);
      if (f) openDiff(f);
    }
  }

  function person(label, p) {
    return h('div', { class: 'who' }, avatar(p.name, p.email, true),
      h('div', { class: 'who-text' },
        h('span', { class: 'who-name', title: `${p.name} <${p.email}>` }, p.name),
        h('span', { class: 'who-sub', title: p.email }, `${label} · ${fmtDate(p.time)}${/ago|now/.test(relTime(p.time)) ? ` · ${relTime(p.time)}` : ''}`)));
  }

  function drawDetail() {
    const d = tab.detail;
    if (!d) {
      detailEl.replaceChildren(h('div', { class: 'detail-empty' }, icon('commit', 28), h('div', {}, 'Select a commit to see its changes')));
      return;
    }
    if (d.error) {
      detailEl.replaceChildren(h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, d.error)));
      return;
    }
    if (!d.data) {
      detailEl.replaceChildren(h('div', { class: 'detail-empty' }, h('span', { class: 'spinner' })));
      return;
    }
    const c = d.data;
    const sameCommitter = c.author.name === c.committer.name && c.author.email === c.committer.email && Math.abs(c.author.time - c.committer.time) < 60;
    const sha = (oid) => h('button', { class: 'sha-chip', title: `${oid}\nClick to copy`, onclick: () => copyText(oid, 'Commit id copied') }, oid.slice(0, 10));
    const parent = (oid) => h('button', { class: 'sha-chip', title: `${oid}\nClick to go to this commit`, onclick: () => tab.graph.selectOid(oid) }, oid.slice(0, 7));
    const head = h('div', { class: 'detail-head' },
      h('h2', { class: 'commit-title' }, c.summary || '(no message)'),
      c.body ? h('pre', { class: 'commit-body' }, c.body) : null,
      person('authored', c.author),
      sameCommitter ? null : person('committed', c.committer),
      h('dl', { class: 'meta' },
        h('dt', {}, 'Commit'), h('dd', {}, sha(c.oid)),
        h('dt', {}, c.parents.length === 1 ? 'Parent' : 'Parents'), h('dd', {}, c.parents.length ? c.parents.map(parent) : h('span', { class: 'muted' }, 'none (root commit)'))));
    const filesHead = h('div', { class: 'files-head' },
      h('span', {}, `${fmtNum(c.files.length)} changed file${c.files.length === 1 ? '' : 's'}`),
      c.parents.length > 1 ? h('span', { class: 'badge muted', title: 'Changes compared with the first parent' }, 'vs first parent') : null,
      h('span', { class: 'spacer' }),
      c.additions ? h('span', { class: 'stat-add' }, `+${fmtNum(c.additions)}`) : null,
      c.deletions ? h('span', { class: 'stat-del' }, `−${fmtNum(c.deletions)}`) : null);
    const files = h('div', { class: 'files' }, c.files.map((f) => fileRow(f, () => openDiff(f))));
    if (!c.files.length) files.append(h('div', { class: 'sb-empty small' }, 'This commit changes no files.'));
    detailEl.replaceChildren(head, filesHead, files);
  }

  function fileRow(f, onclick) {
    const slash = f.path.lastIndexOf('/');
    const el = h('button', { class: 'file', title: f.oldPath ? `${f.oldPath} → ${f.path}` : f.path, dataset: { path: f.path }, onclick },
      h('span', { class: `st ${f.status === '?' ? 'q' : f.status}` }, f.status),
      h('span', { class: 'file-path' }, h('bdi', {}, slash >= 0 ? h('span', { class: 'file-dir' }, f.path.slice(0, slash + 1)) : null, f.path.slice(slash + 1))),
      h('span', { class: 'file-stat' },
        f.binary ? h('span', { class: 'muted' }, 'bin') : [
          f.additions ? h('span', { class: 'stat-add' }, `+${f.additions}`) : null,
          f.deletions ? h('span', { class: 'stat-del' }, `−${f.deletions}`) : null]));
    el.addEventListener('contextmenu', (e) => contextMenu(e, [
      { icon: 'copy', label: 'Copy path', run: () => copyText(f.path) },
      { icon: 'copy', label: 'Copy file name', run: () => copyText(f.path.slice(slash + 1)) },
    ]));
    return el;
  }

  // ---- diff
  function closeDiff() {
    if (!tab.diffPanel) return;
    const p = tab.diffPanel;
    tab.diffPanel = null;
    p.close();
    for (const el of detailEl.querySelectorAll('.file.active')) el.classList.remove('active');
  }
  function openDiff(f) {
    const oid = tab.detail?.oid;
    if (!oid) return;
    closeDiff();
    tab.diffPanel = createDiffPanel({
      file: f,
      subtitle: oid.slice(0, 7),
      load: () => api(`/repos/${id}/diff?${qs({ commit: oid, path: f.path, oldPath: f.oldPath })}`),
      onClose: () => { if (tab.diffPanel) { tab.diffPanel = null; for (const el of detailEl.querySelectorAll('.file.active')) el.classList.remove('active'); } tab.graph.focus(); },
    });
    center.append(tab.diffPanel.el);
    for (const el of detailEl.querySelectorAll('.file')) el.classList.toggle('active', el.dataset.path === f.path);
  }
  tab.closeDiff = () => { const open = !!tab.diffPanel; closeDiff(); return open; };

  // ---- context menu of a commit row
  function commitMenu(e, row, ref) {
    if (row.k === 'wip') return;
    contextMenu(e, [
      ref ? { icon: 'copy', label: `Copy name: ${ref.name}`, run: () => copyText(ref.name) } : null,
      { icon: 'copy', label: 'Copy commit id', sub: row.o.slice(0, 10), run: () => copyText(row.o, 'Commit id copied') },
      { icon: 'copy', label: 'Copy message', run: () => copyText(row.s) },
    ]);
  }

  tab.selectOid = async (oid) => {
    if (!(await tab.graph.selectOid(oid))) toast('This commit is not in the graph', 'error');
  };
  tab.destroy = () => { clearTimeout(refsTimer); tab.graph.destroy(); tab.el.remove(); };

  drawTitle();
  drawDetail();
  loadRefs();
  return tab;
}

// ---------------------------------------------------------------- render
function render() {
  renderSidebar();
  renderMain();
}

function renderTabBar() {
  const bar = $('.tabbar');
  if (!bar) return;
  fill(bar,
    h('div', { class: `tab tab-home${state.active === 'home' ? ' active' : ''}`, title: 'Start page', onclick: () => activate('home') }, icon('home')),
    state.tabs.map((t) => h('div', {
      class: `tab${t.id === state.active ? ' active' : ''}`, title: t.path,
      onclick: () => activate(t.id),
      onauxclick: (e) => { if (e.button === 1) closeTab(t.id); },
      oncontextmenu: (e) => contextMenu(e, [
        { icon: 'x', label: 'Close', hint: 'Ctrl+W', run: () => closeTab(t.id) },
        { icon: 'x', label: 'Close other tabs', disabled: state.tabs.length < 2, run: () => { for (const o of [...state.tabs]) if (o.id !== t.id) closeTab(o.id); } },
        '-',
        { icon: 'copy', label: 'Copy path', run: () => copyText(t.path) },
      ]),
    }, icon('repo', 13), h('span', { class: 'tab-name' }, t.name),
    h('button', { class: 'tab-close', title: 'Close (Ctrl+W)', onclick: (e) => { e.stopPropagation(); closeTab(t.id); } }, icon('x', 12)))),
    h('button', { class: 'icon-btn tab-add', title: 'Open a repository (Ctrl+O)', onclick: pickAndOpen }, icon('plus')),
  );
}

function renderMain() {
  const main = $('#main');
  let bar = $('.tabbar', main);
  let content = $('.tab-content', main);
  if (!bar) {
    bar = h('div', { class: 'tabbar' });
    content = h('div', { class: 'tab-content' });
    main.replaceChildren(bar, content);
  }
  renderTabBar();
  const tab = activeTab();
  const want = tab ? tab.el : homePage();
  if (content.firstChild !== want) content.replaceChildren(want);
  document.title = tab ? `${tab.name} — ${APP_NAME}` : APP_NAME;
  if (tab) tab.graph.redraw();
}

// ---------------------------------------------------------------- start page
function gitNotice() {
  if (state.info?.git) return null;
  return h('div', { class: 'notice', style: { border: '1px solid var(--mod-border)', borderRadius: '6px' } }, icon('alert'),
    h('span', {}, 'Git was not found on PATH. You can browse repositories; fetch, pull, push and history changes need Git. ',
      h('a', { href: 'https://git-scm.com/downloads', target: '_blank' }, 'Install Git'), ', then restart the app.'));
}

function homePage() {
  const grid = h('div', { class: 'home-grid' });
  const search = h('input', { class: 'input home-search', type: 'search', placeholder: 'Filter recent repositories…' });
  const matches = () => {
    const q = search.value.trim().toLowerCase();
    return state.recent.filter((r) => !q || `${r.name} ${r.path}`.toLowerCase().includes(q));
  };
  const card = (r) => h('div', {
    class: `home-card${r.exists ? '' : ' missing'}`, tabindex: '0', role: 'button', title: r.exists ? r.path : `${r.path}\nThis folder no longer exists`,
    onclick: () => openRepo(r.path),
    onkeydown: (e) => { if (e.key === 'Enter') openRepo(r.path); },
  },
    h('div', { class: 'home-card-head' }, icon('repo'), h('span', { class: 'home-card-name' }, r.name),
      h('button', { class: 'icon-btn', title: 'Remove from this list', onclick: async (e) => { e.stopPropagation(); await api('/recent/forget', { body: { path: r.path } }).catch(() => {}); loadRecent(); } }, icon('x', 13))),
    h('div', { class: 'home-card-meta' },
      h('span', { class: 'muted small' }, r.openedAt ? `opened ${relTime(r.openedAt)}` : ''),
      state.tabs.some((t) => t.path === r.path) ? h('span', { class: 'badge' }, 'open') : null,
      r.exists ? null : h('span', { class: 'badge warn' }, 'missing')),
    h('div', { class: 'home-card-url mono' }, r.path));
  const draw = () => {
    const list = matches();
    grid.replaceChildren(...list.map(card),
      h('button', { class: 'home-card home-add', onclick: pickAndOpen }, icon('plus', 18), h('span', {}, 'Open a repository')));
    if (!list.length && state.recent.length) grid.prepend(h('div', { class: 'muted home-none' }, 'No repositories match.'));
  };
  search.addEventListener('input', draw);
  search.addEventListener('keydown', (e) => { if (e.key === 'Enter') { const r = matches()[0]; if (r) openRepo(r.path); } });
  draw();
  return h('div', { class: 'home' },
    h('div', { class: 'home-inner' },
      h('div', { class: 'home-brand' },
        h('img', { src: '/brand/symbol.svg', width: 48, height: 48, alt: '' }),
        h('div', {}, h('h1', { class: 'welcome-title' }, APP_NAME), h('p', { class: 'welcome-tag' }, 'Open a repository to see its history'))),
      gitNotice(),
      h('div', { class: 'home-bar' },
        h('button', { class: 'btn primary', onclick: pickAndOpen }, icon('folder'), 'Open repository', h('span', { class: 'kbd' }, 'Ctrl+O')),
        state.recent.length > 4 ? search : null,
        state.info?.desktop ? h('span', { class: 'muted small' }, 'or drop a folder on the window') : null),
      h('div', { class: 'home-section' }, state.recent.length ? 'Recent repositories' : 'No recent repositories yet'),
      grid));
}

// ---------------------------------------------------------------- sidebar
const SECTION_LIMIT = 200;

function sidebarFooter() {
  const theme = currentTheme();
  return h('div', { class: 'sb-foot' },
    h('span', { class: 'brand' }, h('img', { class: 'brand-mark', src: '/brand/symbol.svg', alt: '' }), h('span', { class: 'brand-name' }, APP_NAME),
      h('span', { class: 'ver' }, state.info ? `v${state.info.version}` : '')),
    h('button', { class: 'btn ghost sm', title: 'Theme', onclick: (e) => openThemeMenu(e.currentTarget, renderSidebar) },
      icon('palette', 14), h('span', { class: 'theme-name' }, theme.name)));
}

function openRepoMenu(anchor) {
  const items = [
    { icon: 'folder', label: 'Open repository…', hint: 'Ctrl+O', run: pickAndOpen },
    { icon: 'home', label: 'Start page', run: () => activate('home') },
  ];
  const recent = state.recent.filter((r) => r.exists).slice(0, 12);
  if (recent.length) {
    items.push('-', { title: 'Recent' });
    for (const r of recent) items.push({ icon: 'repo', label: r.name, sub: r.path, active: activeTab()?.path === r.path, run: () => openRepo(r.path) });
  }
  openMenu(anchor, menuItems(items), 'repo-menu scroll-menu');
}

function renderSidebar() {
  const sb = $('#sidebar');
  const tab = activeTab();
  const head = h('div', { class: 'sb-head' },
    h('button', { class: 'repo-switch', title: tab ? tab.path : 'Open a repository', onclick: (e) => openRepoMenu(e.currentTarget) },
      icon('repo'), h('span', { class: 'repo-name' }, tab ? tab.name : 'No repository'),
      tab?.refs?.head?.branch ? h('span', { class: 'repo-branch' }, tab.refs.head.branch) : null, icon('chevron', 13)));
  if (!tab) {
    sb.replaceChildren(head, h('div', { class: 'sb-empty' }, h('span', {}, 'Open a repository to see its branches, remotes, tags and stashes.'),
      h('button', { class: 'btn', onclick: pickAndOpen }, icon('folder'), 'Open repository')), sidebarFooter());
    return;
  }
  const filter = h('input', { class: 'sb-search', placeholder: 'Filter branches and tags…', value: tab.filter, spellcheck: 'false' });
  const list = h('div', { class: 'sb-scroll' });
  filter.addEventListener('input', () => { tab.filter = filter.value; drawRefs(tab, list); });
  drawRefs(tab, list);
  const keepFocus = document.activeElement?.classList?.contains('sb-search');
  const scrollTop = $('.sb-scroll', sb)?.scrollTop || 0;
  sb.replaceChildren(head, h('div', { class: 'sb-search-wrap' }, icon('search', 13), filter), list, sidebarFooter());
  list.scrollTop = scrollTop;
  if (keepFocus) { filter.focus(); filter.setSelectionRange(filter.value.length, filter.value.length); }
}

function drawRefs(tab, list) {
  if (tab.refsError) { list.replaceChildren(h('div', { class: 'sb-empty small error' }, tab.refsError)); return; }
  const refs = tab.refs;
  if (!refs) { list.replaceChildren(h('div', { class: 'sb-empty small' }, h('span', { class: 'spinner' }))); return; }
  const q = tab.filter.trim().toLowerCase();
  const hit = (name) => !q || name.toLowerCase().includes(q);
  const closed = store.get('gl.sbClosed', { tags: true });
  const expanded = tab.expanded || (tab.expanded = {});
  const out = [];
  const section = (key, title, items, render, empty) => {
    const isClosed = !q && !!closed[key];
    out.push(h('button', { class: `sb-section${isClosed ? ' closed' : ''}`, onclick: () => { closed[key] = !closed[key]; store.set('gl.sbClosed', closed); drawRefs(tab, list); } },
      h('span', { class: 'icon chev' }, icon('chevron', 12)), title, h('span', { class: 'sb-count' }, fmtNum(items.length))));
    if (isClosed) return;
    if (!items.length) { out.push(h('div', { class: 'sb-empty small' }, q ? 'No matches' : empty)); return; }
    const limit = expanded[key] ? items.length : SECTION_LIMIT;
    out.push(...items.slice(0, limit).map(render));
    if (items.length > limit) out.push(h('button', { class: 'sb-item sb-more', onclick: () => { expanded[key] = true; drawRefs(tab, list); } }, `Show all ${fmtNum(items.length)}`));
  };
  const go = (oid) => tab.selectOid(oid);

  section('local', 'Local branches', refs.branches.filter((b) => hit(b.name)), (b) => h('button', {
    class: `sb-item${b.current ? ' current' : ''}`, title: b.upstream ? `${b.name} → ${b.upstream}` : b.name, onclick: () => go(b.oid),
    oncontextmenu: (e) => contextMenu(e, [{ icon: 'copy', label: 'Copy branch name', run: () => copyText(b.name) }]),
  }, icon(b.current ? 'check' : 'branch', 14), h('span', { class: 'sb-name' }, b.name),
  b.gone ? h('span', { class: 'sb-track gone', title: `The upstream ${b.upstream} no longer exists` }, 'gone')
    : (b.ahead || b.behind) ? h('span', { class: 'sb-track', title: `${b.ahead} ahead of, ${b.behind} behind ${b.upstream}` }, `${b.ahead ? `↑${b.ahead}` : ''}${b.ahead && b.behind ? ' ' : ''}${b.behind ? `↓${b.behind}` : ''}`) : null),
  'No local branches');

  const remoteItems = [];
  for (const r of refs.remotes) {
    const branches = r.branches.filter((b) => hit(b.full));
    if (q && !branches.length) continue;
    remoteItems.push({ remote: r }, ...branches.map((b) => ({ branch: b })));
  }
  section('remotes', 'Remotes', remoteItems, (it) => it.remote
    ? h('div', { class: 'sb-group', title: it.remote.url || '' }, icon('cloud', 13), h('span', { class: 'sb-name' }, it.remote.name))
    : h('button', {
      class: 'sb-item nested', title: it.branch.full, onclick: () => go(it.branch.oid),
      oncontextmenu: (e) => contextMenu(e, [{ icon: 'copy', label: 'Copy branch name', run: () => copyText(it.branch.full) }]),
    }, icon('branch', 14), h('span', { class: 'sb-name' }, it.branch.name)), 'No remotes');

  section('tags', 'Tags', refs.tags.filter((t) => hit(t.name)), (t) => h('button', {
    class: 'sb-item', title: t.annotated ? `${t.name} (annotated)` : t.name, onclick: () => go(t.oid),
    oncontextmenu: (e) => contextMenu(e, [{ icon: 'copy', label: 'Copy tag name', run: () => copyText(t.name) }]),
  }, icon('tag', 14), h('span', { class: 'sb-name' }, t.name)), 'No tags');

  section('stashes', 'Stashes', refs.stashes.filter((s) => hit(s.message)), (s) => h('button', {
    class: 'sb-item', title: s.message, onclick: () => go(s.oid),
  }, icon('stash', 14), h('span', { class: 'sb-name' }, s.message || `stash@{${s.index}}`)), 'No stashes');

  list.replaceChildren(...out);
}

// ---------------------------------------------------------------- layout & keys
function initSidebarResizer() {
  const root = document.documentElement;
  const saved = store.get('gl.sidebarW', null);
  if (saved) root.style.setProperty('--sidebar-w', `${saved}px`);
  dragResize($('#sidebar-resizer'), {
    onStart: () => $('#sidebar').offsetWidth,
    onMove: (dx, _dy, w0) => root.style.setProperty('--sidebar-w', `${Math.max(180, Math.min(520, w0 + dx))}px`),
    onEnd: () => store.set('gl.sidebarW', $('#sidebar').offsetWidth),
  });
}

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  const tab = activeTab();
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName || '');
  if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); pickAndOpen(); }
  else if (mod && e.key.toLowerCase() === 'w') { e.preventDefault(); if (tab) closeTab(tab.id); }
  else if (mod && e.key.toLowerCase() === 'f' && tab) { e.preventDefault(); tab.focusSearch(); }
  else if (e.key === 'F5' && tab) { e.preventDefault(); tab.refresh(true); }
  else if (e.ctrlKey && e.key === 'Tab' && state.tabs.length) {
    e.preventDefault();
    const ids = ['home', ...state.tabs.map((t) => t.id)];
    const i = ids.indexOf(state.active);
    activate(ids[(i + (e.shiftKey ? ids.length - 1 : 1)) % ids.length]);
  } else if (e.key === 'Escape' && tab && !typing && !e.defaultPrevented && !$('.overlay')) {
    if (tab.closeDiff()) e.preventDefault();
  }
});
// The page is an app, not a document: no browser context menu outside inputs.
document.addEventListener('contextmenu', (e) => { if (!/^(INPUT|TEXTAREA)$/.test(e.target.tagName) && !getSelection()?.toString()) e.preventDefault(); });

boot();
