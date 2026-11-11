// Graftline — shared UI helpers (no build step): DOM, icons, storage, API, menus, modals, toasts, themes.

export const APP_NAME = 'Graftline';

export const $ = (sel, root = document) => root.querySelector(sel);

export function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  let value;
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'value') value = v;
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k === 'checked' || k === 'disabled' || k === 'selected' || k === 'indeterminate') el[k] = !!v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid == null || kid === false) continue;
    el.append(kid instanceof Node ? kid : String(kid));
  }
  if (value !== undefined) el.value = value;
  return el;
}

/** Replaces the children of `el`. Unlike replaceChildren, it accepts arrays and skips null and false. */
export function fill(el, ...kids) {
  el.replaceChildren(...kids.flat(Infinity).filter((k) => k != null && k !== false));
  return el;
}

const ICONS = {
  search: '<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/>',
  trash: '<path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14"/>',
  left: '<path d="m15 18-6-6 6-6"/>',
  right: '<path d="m9 18 6-6-6-6"/>',
  up: '<path d="m18 15-6-6-6 6"/>',
  down: '<path d="m6 9 6 6 6-6"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  copy: '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  home: '<path d="M3 11l9-7 9 7"/><path d="M5 10v10h14V10"/><path d="M10 20v-6h4v6"/>',
  edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M14 6l4 4"/>',
  external: '<path d="M14 4h6v6M20 4l-9 9M19 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h5"/>',
  palette: '<path d="M12 22a10 10 0 1 1 10-10c0 2.8-2.2 4-4 4h-2a2 2 0 0 0-1.5 3.3A1.6 1.6 0 0 1 12 22z"/><circle cx="7.5" cy="10.5" r="1"/><circle cx="10.5" cy="7" r="1"/><circle cx="15" cy="7.5" r="1"/>',
  download: '<path d="M12 3v12M7 10l5 5 5-5M4 21h16"/>',
  upload: '<path d="M12 21V9M7 14l5-5 5 5M4 3h16"/>',
  undo: '<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>',
  history: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l3 2"/>',
  keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  user: '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
  alert: '<path d="M12 3 2 20h20z"/><path d="M12 10v5M12 18h.01"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5h.01"/>',
  file: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>',
  minus: '<path d="M5 12h14"/>',
  play: '<path d="M7 4v16l13-8z"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-2.8 1.2V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-2.8-1.2l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0-1.2-2.8H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.2-2.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 2.8-1.2V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 2.8 1.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0 1.2 2.8h.1a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.4 1.1z"/>',
  // git
  branch: '<circle cx="6" cy="5" r="2.5"/><circle cx="6" cy="19" r="2.5"/><circle cx="18" cy="7" r="2.5"/><path d="M6 7.5v9M18 9.5c0 4-6 3-12 6"/>',
  commit: '<circle cx="12" cy="12" r="3.5"/><path d="M12 3v5.5M12 15.5V21"/>',
  merge: '<circle cx="6" cy="5" r="2.5"/><circle cx="6" cy="19" r="2.5"/><circle cx="18" cy="13" r="2.5"/><path d="M6 7.5v9M6 8c0 4 5 5 9.5 5"/>',
  tag: '<path d="M3 12V4a1 1 0 0 1 1-1h8l9 9-9 9z"/><circle cx="7.5" cy="7.5" r="1.3"/>',
  cloud: '<path d="M7 18a4.5 4.5 0 0 1-.6-8.96A6 6 0 0 1 18 10.5a3.75 3.75 0 0 1-.5 7.5z"/>',
  stash: '<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9M10 13h4"/>',
  repo: '<path d="M5 4a2 2 0 0 1 2-2h12v16H7a2 2 0 0 0-2 2z"/><path d="M5 20a2 2 0 0 0 2 2h12v-4"/><path d="M9 6h1M9 9h1M9 12h1"/>',
  diff: '<path d="M12 3v8M8 7h8M8 17h8"/><rect x="3" y="2" width="18" height="20" rx="2"/>',
  columns: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16"/>',
  rows: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 12h18"/>',
  fetch: '<path d="M12 4v11M7 11l5 5 5-5"/><path d="M5 20h14"/>',
  pull: '<path d="M12 3v13M6 11l6 6 6-6"/>',
  push: '<path d="M12 21V8M6 13l6-6 6 6"/>',
};

export function icon(name, size = 15) {
  const span = document.createElement('span');
  span.className = 'icon';
  span.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${ICONS[name] || ''}</svg>`;
  return span;
}

// ---------------------------------------------------------------- storage
// Set by app.js once /api/info is known.
export const env = { info: null };

export const store = {
  get(key, fallback) {
    try { const v = localStorage.getItem(key); return v == null ? fallback : JSON.parse(v); } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* storage unavailable */ }
    prefsChanged();
  },
};

// Desktop app: the page origin changes at each launch (a new port), so localStorage starts empty.
// Copy the gl.* keys to the server; it puts them back into the next page (see src/prefs.rs).
let prefsTimer = null;
function prefsChanged() {
  if (!env.info?.desktop) return;
  clearTimeout(prefsTimer);
  prefsTimer = setTimeout(syncPrefs, 400);
}
function prefsSnapshot() {
  const items = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k.startsWith('gl.') && k !== 'gl.__rev') items[k] = localStorage.getItem(k);
    }
  } catch { /* storage unavailable */ }
  return items;
}
async function syncPrefs() {
  prefsTimer = null;
  try {
    const { rev } = await api('/prefs', { method: 'PUT', body: { items: prefsSnapshot() } });
    localStorage.setItem('gl.__rev', String(rev));
  } catch { /* the next change tries again */ }
}
// A change in the last moment before the window closes.
window.addEventListener('pagehide', () => {
  if (!prefsTimer) return;
  clearTimeout(prefsTimer);
  prefsTimer = null;
  fetch('/api/prefs', { method: 'PUT', keepalive: true, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ items: prefsSnapshot() }) }).catch(() => {});
});

// ---------------------------------------------------------------- API
/** An API error. `command` and `output` are set when a git command failed. */
export class ApiError extends Error {
  constructor(message, data) {
    super(message);
    this.command = data?.command || null;
    this.output = data?.output || null;
    this.data = data || null;
  }
}

export async function api(path, { method, body, signal } = {}) {
  let res;
  try {
    res = await fetch('/api' + path, {
      method: method || (body !== undefined ? 'POST' : 'GET'),
      headers: body !== undefined ? { 'content-type': 'application/json' } : {},
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal,
    });
  } catch (e) {
    if (e?.name === 'AbortError') throw e;
    throw new Error(env.info?.desktop ? `The ${APP_NAME} engine stopped. Restart the app.` : `Cannot reach the ${APP_NAME} server at ${location.host}.`);
  }
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = { error: text }; }
  if (!res.ok) throw new ApiError((data && data.error) || `${res.status} ${res.statusText}`, data);
  return data;
}

export const qs = (params) => new URLSearchParams(Object.entries(params).filter(([, v]) => v != null)).toString();

// ---------------------------------------------------------------- toasts
export function toast(message, kind = 'info', action = null) {
  const el = h('div', { class: `toast ${kind}` }, h('span', { style: { flex: '1', minWidth: '0' } }, message),
    action ? h('button', { class: 'toast-action', onclick: () => { action.run(); el.remove(); } }, action.label) : null);
  $('#toasts').append(el);
  const ttl = action ? 9000 : kind === 'error' ? 7000 : 2800;
  setTimeout(() => el.classList.add('hide'), ttl);
  setTimeout(() => el.remove(), ttl + 400);
}

export function copyText(text, what = 'Copied to clipboard') {
  navigator.clipboard?.writeText(text).then(() => toast(what), () => toast('Copy failed', 'error'));
}

// ---------------------------------------------------------------- menus & modals
let menuEl = null;
export function openMenu(anchor, items, cls = '') {
  closeMenu();
  const r = anchor.getBoundingClientRect();
  menuEl = h('div', { class: `menu ${cls}` }, items);
  document.body.append(menuEl);
  const mw = menuEl.offsetWidth;
  const left = Math.min(r.left, window.innerWidth - mw - 8);
  menuEl.style.left = `${Math.max(8, left)}px`;
  menuEl.style.top = `${r.bottom + 4}px`;
  // Near the bottom edge, keep the menu inside the window.
  const over = r.bottom + 4 + menuEl.offsetHeight + 8 - window.innerHeight;
  if (over > 0) menuEl.style.top = `${Math.max(8, r.bottom + 4 - over)}px`;
  setTimeout(() => document.addEventListener('mousedown', menuOutside), 0);
  return menuEl;
}
function menuOutside(e) { if (menuEl && !menuEl.contains(e.target)) closeMenu(); }
export function closeMenu() {
  document.removeEventListener('mousedown', menuOutside);
  menuEl?.remove();
  menuEl = null;
}
document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && menuEl) { closeMenu(); e.preventDefault(); } });

/** Menu rows from descriptions: { icon, label, sub, hint, run, danger, disabled }, '-' for a separator, { title } for a label. */
export function menuItems(items) {
  return items.filter(Boolean).map((it) => {
    if (it === '-') return h('div', { class: 'menu-sep' });
    if (it.title) return h('div', { class: 'menu-label' }, it.title);
    return h('div', {
      class: `menu-item${it.danger ? ' danger' : ''}${it.disabled ? ' disabled' : ''}${it.active ? ' active' : ''}`,
      title: it.tip || null,
      onclick: () => { closeMenu(); it.run(); },
    }, it.icon ? icon(it.icon) : null, h('span', { class: 'menu-text' }, it.label, it.sub ? h('small', {}, it.sub) : null),
    it.hint ? h('span', { class: 'kbd' }, it.hint) : null);
  });
}

/** Opens a menu at the mouse pointer. */
export function contextMenu(e, items) {
  e.preventDefault();
  e.stopPropagation();
  const at = { getBoundingClientRect: () => ({ left: e.clientX, bottom: e.clientY - 4 }) };
  return openMenu(at, menuItems(items), 'ctx-menu');
}

export function openModal(title, body, footer, { wide = false, onClose = null } = {}) {
  const overlay = h('div', { class: 'overlay', onmousedown: (e) => { if (e.target === overlay) close(); } });
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    overlay.remove();
    document.removeEventListener('keydown', onKey);
    if (onClose) onClose();
  };
  const onKey = (e) => { if (e.key === 'Escape' && !e.defaultPrevented) { e.preventDefault(); close(); } };
  document.addEventListener('keydown', onKey);
  overlay.append(h('div', { class: `modal${wide ? ' wide' : ''}` },
    h('div', { class: 'modal-head' }, h('h3', {}, title), h('button', { class: 'icon-btn', onclick: close }, icon('x'))),
    h('div', { class: 'modal-body' }, body),
    footer ? h('div', { class: 'modal-foot' }, footer) : null,
  ));
  document.body.append(overlay);
  return close;
}

// ---------------------------------------------------------------- themes
// Each theme is a set of CSS variables in style.css under :root[data-theme="<id>"].
// The swatch shows background, accent and two graph colors.
export const THEMES = [
  { id: 'graftline-light', name: 'Graftline Light', swatch: ['#ffffff', '#0d9488', '#b45309', '#7c3aed'] },
  { id: 'graftline', name: 'Graftline Dark', swatch: ['#0b1211', '#2dd4bf', '#fbbf24', '#a78bfa'] },
  { id: 'dark', name: 'Graphite', swatch: ['#0b0b0b', '#c5f74f', '#60a5fa', '#f472b6'] },
  { id: 'light', name: 'Paper', swatch: ['#ffffff', '#1f2937', '#2563eb', '#b45309'] },
  { id: 'one-dark-pro', name: 'One Dark Pro', swatch: ['#282c34', '#61afef', '#e5c07b', '#c678dd'] },
  { id: 'dracula', name: 'Dracula', swatch: ['#282a36', '#bd93f9', '#f1fa8c', '#ff79c6'] },
  { id: 'nord', name: 'Nord', swatch: ['#2e3440', '#88c0d0', '#ebcb8b', '#b48ead'] },
  { id: 'github-light', name: 'GitHub Light', swatch: ['#ffffff', '#1f883d', '#0969da', '#9a6700'] },
];

const themeListeners = new Set();
/** Calls `fn` after the theme changes (also for the hover preview). */
export function onThemeChange(fn) { themeListeners.add(fn); return () => themeListeners.delete(fn); }
function applyTheme(id) {
  document.documentElement.dataset.theme = id;
  for (const fn of themeListeners) fn(id);
}

export function setTheme(id) {
  applyTheme(id);
  store.set('gl.theme', id);
}

export const currentTheme = () => THEMES.find((t) => t.id === document.documentElement.dataset.theme) || THEMES[1];

// Re-apply the saved theme here too, and fall back to the default for an unknown value.
{
  // No saved choice: follow the system light / dark setting.
  const fallback = window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'graftline-light' : 'graftline';
  const forced = new URLSearchParams(location.hash.slice(1)).get('theme');
  const saved = forced || store.get('gl.theme', fallback);
  document.documentElement.dataset.theme = THEMES.some((t) => t.id === saved) ? saved : fallback;
}

export function openThemeMenu(anchor, onChosen) {
  const original = document.documentElement.dataset.theme;
  let chosen = false;
  const menu = openMenu(anchor, THEMES.map((t) => h('div', {
    class: `menu-item${t.id === original ? ' active' : ''}`,
    // Live preview on hover; the click keeps it.
    onmouseenter: () => applyTheme(t.id),
    onclick: () => { chosen = true; setTheme(t.id); closeMenu(); onChosen?.(); },
  },
  h('span', { class: 'swatch' }, t.swatch.map((c) => h('span', { style: { background: c } }))),
  h('span', { class: 'menu-text' }, t.name),
  t.id === original ? icon('check', 13) : null,
  )), 'theme-menu');
  menu.addEventListener('mouseleave', () => { if (!chosen) applyTheme(original); });
  // Reposition above the anchor: the button sits at the bottom of the sidebar.
  const r = anchor.getBoundingClientRect();
  menu.style.top = `${Math.max(8, r.top - menu.offsetHeight - 4)}px`;
}

// ---------------------------------------------------------------- formatting
const pad = (n) => String(n).padStart(2, '0');

/** `2026-11-03 14:20` in local time. */
export function fmtDate(sec) {
  const d = new Date(sec * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Short relative time for recent dates, the date otherwise. */
export function relTime(sec, now = Date.now() / 1000) {
  const s = Math.max(0, now - sec);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)} d ago`;
  return fmtDate(sec).slice(0, 10);
}

export const fmtNum = (n) => (n == null ? '?' : Number(n).toLocaleString('en-US'));

const AVATAR_COLORS = ['#0d9488', '#b45309', '#7c3aed', '#be123c', '#0369a1', '#4d7c0f', '#c2410c', '#a21caf', '#475569', '#0f766e'];
/** A colored circle with the author's initials. No network request, so no avatar service learns who is in the log. */
export function avatar(name, email, large = false) {
  const key = (email || name || '?').toLowerCase();
  let hash = 0;
  for (let i = 0; i < key.length; i++) hash = (hash * 31 + key.charCodeAt(i)) >>> 0;
  const words = (name || email || '?').trim().split(/[\s._-]+/).filter(Boolean);
  const initials = ((words[0]?.[0] || '?') + (words.length > 1 ? words[words.length - 1][0] : '')).toUpperCase();
  return h('span', { class: `avatar${large ? ' lg' : ''}`, style: { background: AVATAR_COLORS[hash % AVATAR_COLORS.length] }, title: email ? `${name} <${email}>` : name }, initials);
}

/** Drag on `handle` resizes something: `onMove(dx, dy, ctx)` gets the distance from the start. */
export function dragResize(handle, { onStart, onMove, onEnd, vertical = false }) {
  handle.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const x0 = e.clientX, y0 = e.clientY;
    const ctx = onStart?.(e);
    document.body.classList.add(vertical ? 'resizing-v' : 'resizing');
    const move = (ev) => onMove(ev.clientX - x0, ev.clientY - y0, ctx);
    const up = () => {
      document.body.classList.remove('resizing', 'resizing-v');
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
      onEnd?.(ctx);
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  });
}
