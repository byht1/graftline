// Graftline — diff rendering: unified and side-by-side views of one file.

import { h, icon, store } from '/ui.js';

/** Lines rendered at once. A larger diff shows a button for the rest. */
const CHUNK = 3000;

/** Hook for syntax highlighting: returns HTML for one line of code. */
let highlighter = null;
export function setHighlighter(fn) { highlighter = fn; }

function textCell(text, lang) {
  const el = h('span', { class: 'dl-text' });
  const html = highlighter ? highlighter(text, lang) : null;
  if (html != null) el.innerHTML = html;
  else el.textContent = text;
  return el;
}

const SIGN = { '+': '+', '-': '−', ' ': '' };
const CLS = { '+': 'add', '-': 'del', ' ': 'ctx', '\\': 'eof' };

function unifiedLine(l, lang) {
  if (l.t === '\\') {
    return h('div', { class: 'dl eof' }, h('span', { class: 'dl-no o' }), h('span', { class: 'dl-no n' }), h('span', { class: 'dl-sign' }), h('span', { class: 'dl-text' }, `\\ ${l.s}`));
  }
  return h('div', { class: `dl ${CLS[l.t]}` },
    h('span', { class: 'dl-no o' }, l.o ?? ''), h('span', { class: 'dl-no n' }, l.n ?? ''),
    h('span', { class: 'dl-sign' }, SIGN[l.t]), textCell(l.s, lang));
}

function half(l, side, lang) {
  if (!l) return h('div', { class: 'split-half blank' });
  const cls = l.t === ' ' ? '' : CLS[l.t];
  return h('div', { class: `split-half ${cls}` },
    h('span', { class: 'dl-no' }, (side === 'o' ? l.o : l.n) ?? ''), h('span', { class: 'dl-sign' }, SIGN[l.t] ?? ''), textCell(l.s, lang));
}

/** Pairs removed and added lines of one hunk for the side-by-side view. */
export function splitRows(lines) {
  const rows = [];
  let dels = [], adds = [];
  const flush = () => {
    for (let i = 0; i < Math.max(dels.length, adds.length); i++) rows.push([dels[i] || null, adds[i] || null]);
    dels = []; adds = [];
  };
  for (const l of lines) {
    if (l.t === '-') { if (adds.length) flush(); dels.push(l); }
    else if (l.t === '+') adds.push(l);
    else if (l.t === ' ') { flush(); rows.push([l, l]); }
  }
  flush();
  return rows;
}

const EXT_LANG = {
  rs: 'rust', js: 'js', mjs: 'js', cjs: 'js', jsx: 'js', ts: 'js', tsx: 'js', json: 'json', py: 'python', go: 'go',
  c: 'c', h: 'c', cc: 'c', cpp: 'c', hpp: 'c', cs: 'c', java: 'c', kt: 'c', swift: 'c', php: 'c',
  css: 'css', scss: 'css', html: 'html', htm: 'html', xml: 'html', svg: 'html', vue: 'html',
  sh: 'shell', bash: 'shell', zsh: 'shell', ps1: 'shell', yml: 'yaml', yaml: 'yaml', toml: 'toml', ini: 'toml',
  md: 'md', sql: 'sql', rb: 'ruby', lua: 'lua',
};
export const langOf = (path) => EXT_LANG[(path.split('/').pop().split('.').pop() || '').toLowerCase()] || null;

/**
 * Builds the body of a diff. `decorate(lineEl, line, hunkIndex, lineIndex)` lets the caller add
 * controls to a line (staging); `hunkActions(hunk, index)` returns buttons for the hunk header.
 */
export function renderDiff(diff, { mode = 'unified', decorate = null, hunkActions = null } = {}) {
  const body = h('div', { class: `diff-body ${mode}` });
  if (diff.binary) {
    body.append(h('div', { class: 'diff-note' }, 'Binary file. The content is not shown.'));
    return body;
  }
  if (!diff.hunks.length) {
    const mode = diff.oldMode && diff.newMode && diff.oldMode !== diff.newMode ? `File mode changed: ${diff.oldMode} → ${diff.newMode}` : null;
    body.append(h('div', { class: 'diff-note' }, mode || (diff.status === 'R' ? 'Renamed without changes.' : 'No changes in the content.')));
    return body;
  }
  const lang = langOf(diff.path);
  let budget = CHUNK;
  const pending = [];
  diff.hunks.forEach((hunk, hi) => {
    const el = h('div', { class: 'hunk' }, h('div', { class: 'hunk-head' }, hunk.header, hunkActions ? hunkActions(hunk, hi) : null));
    const build = () => {
      if (mode === 'split') {
        for (const [l, r] of splitRows(hunk.lines)) el.append(h('div', { class: 'split-row' }, half(l && l.t !== '+' ? l : null, 'o', lang), half(r && r.t !== '-' ? r : null, 'n', lang)));
      } else {
        hunk.lines.forEach((l, li) => {
          const line = unifiedLine(l, lang);
          decorate?.(line, l, hi, li);
          el.append(line);
        });
      }
    };
    if (budget > 0) { build(); budget -= hunk.lines.length; body.append(el); } else pending.push([el, build, hunk.lines.length]);
  });
  if (pending.length) {
    const left = pending.reduce((n, p) => n + p[2], 0);
    const more = h('div', { class: 'diff-note' }, h('button', {
      class: 'btn',
      onclick: () => { more.remove(); for (const [el, build] of pending) { build(); body.append(el); } if (diff.truncated) body.append(truncatedNote()); },
    }, `Show ${left.toLocaleString('en-US')} more lines`));
    body.append(more);
  } else if (diff.truncated) body.append(truncatedNote());
  return body;
}

const truncatedNote = () => h('div', { class: 'diff-note' }, 'The diff is too large. The rest is not shown.');

/**
 * A panel that shows one file diff over the graph. `load()` returns the diff; `subtitle` is
 * a short context text (commit id). Returns { el, close, reload }.
 */
export function createDiffPanel({ file, subtitle, load, onClose, extraActions = null, renderOptions = null }) {
  let mode = store.get('gl.diffMode', 'unified');
  let diff = null;
  const bodyWrap = h('div', { style: { flex: '1', minHeight: '0', display: 'flex', flexDirection: 'column' } });
  const seg = h('div', { class: 'seg' });
  const drawSeg = () => seg.replaceChildren(
    h('button', { class: mode === 'unified' ? 'on' : '', title: 'One column', onclick: () => setMode('unified') }, 'Unified'),
    h('button', { class: mode === 'split' ? 'on' : '', title: 'Old and new side by side', onclick: () => setMode('split') }, 'Split'));
  const setMode = (m) => { mode = m; store.set('gl.diffMode', m); drawSeg(); draw(); };
  const draw = () => {
    if (!diff) return;
    const opts = renderOptions ? renderOptions(diff, mode) : {};
    bodyWrap.replaceChildren(renderDiff(diff, { mode, ...opts }));
  };
  const el = h('div', { class: 'diff-view' },
    h('div', { class: 'diff-head' },
      h('div', { class: 'diff-title' },
        h('span', { class: `st ${file.status === '?' ? 'q' : file.status}` }, file.status),
        h('span', { class: 'path', title: file.path }, file.oldPath ? `${file.oldPath} → ${file.path}` : file.path),
        subtitle ? h('span', { class: 'muted' }, subtitle) : null),
      extraActions, seg,
      h('button', { class: 'icon-btn', title: 'Close (Esc)', onclick: () => close() }, icon('x'))),
    bodyWrap);
  const reload = async () => {
    if (!diff) bodyWrap.replaceChildren(h('div', { class: 'diff-note' }, h('span', { class: 'spinner' })));
    try {
      diff = await load();
      draw();
    } catch (e) {
      bodyWrap.replaceChildren(h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, e.message)));
    }
  };
  const close = () => { el.remove(); onClose?.(); };
  drawSeg();
  reload();
  return { el, close, reload, file };
}
