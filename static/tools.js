// Graftline — file history, blame, command palette, keyboard shortcuts, settings, updates.

import { APP_NAME, h, fill, icon, api, qs, store, toast, openModal, showError, fmtDate, relTime, avatar, setTheme, THEMES } from '/ui.js';
import { renderDiff } from '/diff.js';
import { highlightLine } from '/highlight.js';
import { langOf } from '/diff.js';

// ---------------------------------------------------------------- settings
const DEFAULTS = { autoFetch: 0, tabSize: 4, autoUpdate: true };
export const settings = () => ({ ...DEFAULTS, ...store.get('gl.settings', {}) });
export function applySettings() {
  document.documentElement.style.setProperty('--tab-size', String(settings().tabSize));
}

export function openSettings({ onChange } = {}) {
  const s = settings();
  const save = (patch) => { Object.assign(s, patch); store.set('gl.settings', s); applySettings(); onChange?.(s); };
  const select = (value, options, set) => h('select', { class: 'input', onchange: (e) => set(e.target.value) }, options.map(([v, label]) => h('option', { value: v, selected: String(v) === String(value) }, label)));
  const body = h('div', { class: 'form' },
    h('label', {}, 'Theme', select(document.documentElement.dataset.theme, THEMES.map((t) => [t.id, t.name]), (v) => setTheme(v))),
    h('label', {}, 'Fetch from the remotes automatically', select(s.autoFetch, [[0, 'Never'], [5, 'Every 5 minutes'], [15, 'Every 15 minutes'], [30, 'Every 30 minutes']], (v) => save({ autoFetch: Number(v) }))),
    h('div', { class: 'hint' }, 'A fetch only downloads new commits. It never changes your branches or files.'),
    h('label', {}, 'Tab width in diffs', select(s.tabSize, [[2, '2 spaces'], [4, '4 spaces'], [8, '8 spaces']], (v) => save({ tabSize: Number(v) }))),
    h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: s.autoUpdate, onchange: (e) => save({ autoUpdate: e.target.checked }) }), 'Check for updates on start'),
    h('div', { class: 'hint' }, `The check asks GitHub for the latest ${APP_NAME} release. It sends the app version and nothing about your repositories.`));
  const close = openModal('Settings', body, [h('button', { class: 'btn', onclick: () => close() }, 'Close')]);
}

// ---------------------------------------------------------------- shortcuts
export const SHORTCUTS = [
  ['Ctrl+P', 'Command palette'],
  ['Ctrl+O', 'Open a repository'],
  ['Ctrl+W', 'Close the tab'],
  ['Ctrl+Tab', 'Next tab'],
  ['Ctrl+F', 'Search commits'],
  ['Enter / Shift+Enter', 'Next / previous search match'],
  ['↑ ↓ PageUp PageDown Home End', 'Move in the commit graph'],
  ['F5', 'Refresh'],
  ['Ctrl+Shift+F', 'Fetch'],
  ['Ctrl+Shift+L', 'Pull'],
  ['Ctrl+Shift+P', 'Push'],
  ['Ctrl+B', 'Create a branch'],
  ['Ctrl+Shift+S', 'Stash the changes'],
  ['Ctrl+Shift+C', 'Show the uncommitted changes'],
  ['Ctrl+Enter', 'Commit (in the message box)'],
  ['Space', 'Stage or unstage the focused file'],
  ['Esc', 'Close the diff, menu or dialog'],
  ['Ctrl+,', 'Settings'],
  ['F1', 'Keyboard shortcuts'],
];

export function openShortcuts() {
  const mac = /Mac/.test(navigator.platform);
  const close = openModal('Keyboard shortcuts', h('div', { class: 'keys' }, SHORTCUTS.map(([k, what]) => h('div', { class: 'key-row' },
    h('span', {}, what), h('span', { class: 'key-combo' }, (mac ? k.replace(/Ctrl/g, '⌘') : k).split(' / ').map((c, i) => [i ? ' / ' : null, h('kbd', { class: 'kbd' }, c)]))))),
  [h('button', { class: 'btn', onclick: () => close() }, 'Close')]);
}

// ---------------------------------------------------------------- command palette
/** `commands`: [{ label, sub, icon, hint, run }]. Typed words must all occur in the label or sub text. */
export function openPalette(commands) {
  const input = h('input', { class: 'input palette-input', placeholder: 'Type a command, branch or repository…', spellcheck: 'false' });
  const list = h('div', { class: 'palette-list' });
  let shown = [];
  let active = 0;
  const overlay = h('div', { class: 'overlay palette-overlay', onmousedown: (e) => { if (e.target === overlay) close(); } },
    h('div', { class: 'modal palette' }, h('div', { class: 'palette-head' }, icon('search', 15), input), list));
  const close = () => { overlay.remove(); document.removeEventListener('keydown', onKey, true); };
  const pick = (c) => { close(); c.run(); };
  const draw = () => {
    const words = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    shown = commands.filter((c) => { const text = `${c.label} ${c.sub || ''}`.toLowerCase(); return words.every((w) => text.includes(w)); }).slice(0, 60);
    active = Math.min(active, Math.max(0, shown.length - 1));
    fill(list, shown.length ? shown.map((c, i) => h('div', { class: `menu-item${i === active ? ' active' : ''}`, onclick: () => pick(c), onmousemove: () => { if (active !== i) { active = i; mark(); } } },
      icon(c.icon || 'right'), h('span', { class: 'menu-text' }, c.label, c.sub ? h('small', {}, c.sub) : null), c.hint ? h('span', { class: 'kbd' }, c.hint) : null))
      : h('div', { class: 'sb-empty small' }, 'No command matches'));
  };
  const mark = () => { [...list.children].forEach((el, i) => el.classList.toggle('active', i === active)); list.children[active]?.scrollIntoView({ block: 'nearest' }); };
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); active = Math.min(shown.length - 1, active + 1); mark(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); active = Math.max(0, active - 1); mark(); }
    else if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); if (shown[active]) pick(shown[active]); }
  };
  input.addEventListener('input', () => { active = 0; draw(); });
  document.addEventListener('keydown', onKey, true);
  document.body.append(overlay);
  draw();
  input.focus();
}

// ---------------------------------------------------------------- file history
/** A panel over the graph: the commits that changed a file, and the diff of the selected one. */
export function createFileHistory({ repoId, path, onClose, selectOid }) {
  const list = h('div', { class: 'fh-list' });
  const diffBox = h('div', { class: 'fh-diff' }, h('div', { class: 'diff-note' }, 'Select a commit to see what it changed in this file.'));
  let selected = null;
  const el = h('div', { class: 'diff-view' },
    h('div', { class: 'diff-head' },
      h('div', { class: 'diff-title' }, icon('history', 15), h('span', { class: 'path', title: path }, path), h('span', { class: 'muted' }, 'file history')),
      h('button', { class: 'icon-btn', title: 'Close (Esc)', onclick: () => panel.close() }, icon('x'))),
    h('div', { class: 'fh' }, list, diffBox));
  const show = async (c, rowEl) => {
    selected = c.oid;
    for (const r of list.children) r.classList.toggle('active', r === rowEl);
    fill(diffBox, h('div', { class: 'diff-note' }, h('span', { class: 'spinner' })));
    try {
      const diff = await api(`/repos/${repoId}/diff?${qs({ commit: c.oid, path })}`);
      if (selected === c.oid) fill(diffBox, renderDiff(diff, { mode: store.get('gl.diffMode', 'unified') }));
    } catch (e) {
      if (selected === c.oid) fill(diffBox, h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, e.message)));
    }
  };
  (async () => {
    fill(list, h('div', { class: 'diff-note' }, h('span', { class: 'spinner' })));
    try {
      const res = await api(`/repos/${repoId}/file-history?${qs({ path })}`);
      if (!res.commits.length) { fill(list, h('div', { class: 'diff-note' }, 'No commit on the current branch changed this file.')); return; }
      fill(list, res.commits.map((c) => {
        const row = h('div', { class: 'fh-row', onclick: () => show(c, row), ondblclick: () => { panel.close(); selectOid(c.oid); }, title: 'Double click to go to this commit in the graph' },
          h('span', { class: `st ${c.status}` }, c.status), avatar(c.author, c.email),
          h('span', { class: 'fh-text' }, h('span', { class: 'fh-sum' }, c.summary), h('small', { class: 'muted' }, `${c.author} · ${relTime(c.time)} · ${c.oid.slice(0, 7)}`)));
        return row;
      }), res.more ? h('div', { class: 'diff-note' }, 'Only the newest 500 commits are listed.') : null);
      list.firstChild?.click();
    } catch (e) {
      fill(list, h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, e.message)));
    }
  })();
  const panel = { el, close: () => { el.remove(); onClose?.(panel); }, reload: () => {}, file: { path } };
  return panel;
}

// ---------------------------------------------------------------- blame
export function createBlame({ repoId, path, commit, onClose, selectOid }) {
  const body = h('div', { class: 'diff-body blame' }, h('div', { class: 'diff-note' }, h('span', { class: 'spinner' })));
  const el = h('div', { class: 'diff-view' },
    h('div', { class: 'diff-head' },
      h('div', { class: 'diff-title' }, icon('user', 15), h('span', { class: 'path', title: path }, path), h('span', { class: 'muted' }, commit ? `blame at ${commit.slice(0, 7)}` : 'blame')),
      h('button', { class: 'icon-btn', title: 'Close (Esc)', onclick: () => panel.close() }, icon('x'))),
    body);
  (async () => {
    try {
      const b = await api(`/repos/${repoId}/blame?${qs({ path, commit })}`);
      const lang = langOf(path);
      const starts = new Map(b.hunks.map((x) => [x.start, x]));
      let hunk = null;
      let band = 0;
      fill(body, b.lines.map((text, i) => {
        const first = starts.get(i + 1);
        if (first) { hunk = first; band ^= 1; }
        const code = h('span', { class: 'dl-text' });
        const html = highlightLine(text, lang);
        if (html != null) code.innerHTML = html; else code.textContent = text;
        return h('div', { class: `dl bl${band ? ' alt' : ''}${first ? ' first' : ''}` },
          h('span', { class: 'bl-info', title: hunk ? `${hunk.oid.slice(0, 10)}  ${hunk.author}  ${fmtDate(hunk.time)}\n${hunk.summary}\n\nClick to go to this commit` : '', onclick: () => { if (hunk) { const oid = hunk.oid; panel.close(); selectOid(oid); } } },
            first ? [h('span', { class: 'bl-sha' }, first.oid.slice(0, 7)), h('span', { class: 'bl-who' }, first.author), h('span', { class: 'bl-when' }, relTime(first.time))] : null),
          h('span', { class: 'dl-no' }, i + 1), code);
      }));
    } catch (e) {
      fill(body, h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, e.message)));
    }
  })();
  const panel = { el, close: () => { el.remove(); onClose?.(panel); }, reload: () => {}, file: { path } };
  return panel;
}

// ---------------------------------------------------------------- updates
export const updateState = { info: null };
export const updateAvailable = () => !!(updateState.info?.available && updateState.info.latest !== store.get('gl.skipVersion', null));

export async function checkForUpdates({ auto = false, force = false, onChange } = {}) {
  if (auto && !settings().autoUpdate) return updateState.info;
  try {
    updateState.info = await api(`/update/check${force ? '?force=1' : ''}`);
  } catch (e) {
    if (!auto) throw e;
    return null;
  }
  onChange?.();
  if (auto && updateAvailable() && store.get('gl.updateToasted', null) !== updateState.info.latest) {
    store.set('gl.updateToasted', updateState.info.latest);
    toast(`${APP_NAME} ${updateState.info.latest} is available`, 'info', { label: 'Details', run: () => openUpdateModal({ onChange }) });
  }
  return updateState.info;
}

export function openUpdateModal({ onChange, version } = {}) {
  const body = h('div', { class: 'form' });
  const foot = h('div', { class: 'update-foot' });
  let timer = null;
  const close = openModal('Updates', body, foot, { wide: true, onClose: () => clearInterval(timer) });
  const fmtMb = (n) => `${(n / 1048576).toFixed(1)} MB`;
  const draw = (u, progress = null, error = null) => {
    const parts = [];
    if (!u) parts.push(h('p', { class: error ? 'form-status error' : 'muted' }, error || 'Checking for updates…'));
    else if (!u.available) parts.push(h('div', { class: 'update-head' }, icon('check', 18), h('div', {}, h('b', {}, `${APP_NAME} is up to date`), h('div', { class: 'muted small' }, `Version ${version}. Latest release: ${u.latest}.`))));
    else {
      parts.push(h('div', { class: 'update-head' }, icon('download', 18),
        h('div', {}, h('b', {}, `${APP_NAME} ${u.latest} is available`), h('div', { class: 'muted small' }, `You have ${version}.${u.size ? ` Download: ${fmtMb(u.size)}.` : ''}`))));
      if (u.release?.notes) parts.push(h('pre', { class: 'update-notes' }, u.release.notes.trim()));
      if (progress?.phase === 'downloading') {
        const pct = progress.total ? Math.min(100, Math.round((progress.received / progress.total) * 100)) : 0;
        parts.push(h('div', { class: 'update-progress' }, h('div', { style: { width: `${pct}%` } })),
          h('div', { class: 'muted small' }, `Downloading… ${fmtMb(progress.received)}${progress.total ? ` of ${fmtMb(progress.total)}` : ''}`));
      }
      if (progress?.phase === 'ready') parts.push(h('div', { class: 'form-status ok' }, u.method === 'msi'
        ? `Downloaded and verified. ${APP_NAME} closes, Windows asks for permission once, installs the update and starts ${APP_NAME} again.`
        : `Downloaded and verified. ${APP_NAME} restarts with the new version.`));
      if (progress?.phase === 'restarting') parts.push(h('div', { class: 'form-status' }, h('span', { class: 'spinner' }), ' Restarting…'));
      if (progress?.phase === 'error' || error) parts.push(h('div', { class: 'form-status error' }, error || progress.error));
      if (!u.canInstall) parts.push(h('div', { class: 'hint' }, u.method === 'none' ? 'This is the development server. Build the new version from source.'
        : 'This install cannot update itself (for example, a system package). Download the new version from the release page.'));
    }
    fill(body, parts);
    const btns = [];
    if (u?.available) {
      btns.push(h('a', { class: 'btn ghost', href: u.release.url, target: '_blank', rel: 'noopener' }, icon('external', 13), 'Release page'));
      btns.push(h('button', { class: 'btn ghost', onclick: () => { store.set('gl.skipVersion', u.latest); close(); onChange?.(); } }, 'Skip this version'));
      if (u.canInstall) {
        const phase = progress?.phase || 'idle';
        if (phase === 'ready') btns.push(h('button', { class: 'btn primary', onclick: restart }, icon('refresh', 13), 'Restart and update'));
        else if (phase === 'downloading' || phase === 'restarting') btns.push(h('button', { class: 'btn primary', disabled: true }, h('span', { class: 'spinner' }), phase === 'restarting' ? 'Restarting…' : 'Downloading…'));
        else btns.push(h('button', { class: 'btn primary', onclick: download }, icon('download', 13), phase === 'error' ? 'Try again' : 'Download and install'));
      }
    } else btns.push(h('button', { class: 'btn', onclick: () => load(true) }, icon('refresh', 13), 'Check now'));
    btns.push(h('button', { class: 'btn', onclick: () => close() }, 'Close'));
    fill(foot, btns);
  };
  const poll = () => {
    clearInterval(timer);
    timer = setInterval(async () => {
      try {
        const p = await api('/update/progress');
        draw(updateState.info, p);
        if (p.phase !== 'downloading') clearInterval(timer);
      } catch { clearInterval(timer); }
    }, 400);
  };
  async function download() {
    try { const p = await api('/update/download', { body: {} }); draw(updateState.info, p); poll(); } catch (e) { draw(updateState.info, null, e.message); }
  }
  async function restart() {
    draw(updateState.info, { phase: 'restarting' });
    try { await api('/update/restart', { body: {} }); } catch (e) { draw(updateState.info, null, e.message); }
  }
  async function load(force) {
    draw(null);
    try {
      const u = await checkForUpdates({ force, onChange });
      const p = await api('/update/progress').catch(() => null);
      draw(u, p && p.version === u.latest ? p : null);
      if (p?.phase === 'downloading') poll();
    } catch (e) { draw(null, null, e.message); }
  }
  load(false);
}

export { showError };
