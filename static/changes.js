// Graftline — the working tree panel: unstaged and staged files, staging by file, hunk and
// line, discard, and the commit box.

import { h, fill, icon, api, qs, store, toast, copyText, contextMenu, confirmDialog, openModal, fmtNum } from '/ui.js';
import { createDiffPanel } from '/diff.js';

const plural = (n, word) => `${fmtNum(n)} ${word}${n === 1 ? '' : 's'}`;

/**
 * `ctx`: { repoId, getRefs(), showDiff(panel), closeDiff(), currentDiff(), onStatus(status), afterCommit(result) }.
 * Returns { el, reload, reloadSoon, status }.
 */
export function createChanges(ctx) {
  const id = ctx.repoId;
  const st = {
    status: null,
    error: null,
    open: null,          // { path, side } of the file shown in the diff panel
    picked: new Map(),   // hunk index -> Set of line indexes selected in the diff
    lastPick: null,
    busy: false,
    amend: false,
    headMessage: null,
  };

  const lists = h('div', { class: 'wip-lists' });
  const summary = h('input', { class: 'input', placeholder: 'Summary', spellcheck: 'true' });
  const body = h('textarea', { class: 'input area', placeholder: 'Description (optional)', rows: '3', spellcheck: 'true' });
  const amendBox = h('input', { type: 'checkbox' });
  const amendNote = h('div', { class: 'wip-warn', hidden: true });
  const commitBtn = h('button', { class: 'btn primary', onclick: () => doCommit() });
  const counter = h('span', { class: 'dim small' });
  const commitBox = h('div', { class: 'commit-box' },
    h('div', { class: 'commit-row' }, summary, counter),
    body,
    amendNote,
    h('div', { class: 'commit-row' },
      h('label', { class: 'check small', title: 'Replace the last commit with the staged changes and this message' }, amendBox, 'Amend last commit'),
      h('span', { class: 'spacer' }), commitBtn));
  const el = h('div', { class: 'wip-panel' },
    h('div', { class: 'wip-head' }, icon('edit', 15), h('span', { class: 'wip-title' }, 'Uncommitted changes'), h('span', { class: 'spacer' }),
      h('button', { class: 'icon-btn', title: 'Safety snapshots: files saved before a discard', onclick: () => openSnapshots() }, icon('history'))),
    lists, commitBox);

  // ---------------------------------------------------------------- requests
  async function run(fn, { reloadAfter = true } = {}) {
    if (st.busy) return null;
    st.busy = true;
    try {
      return await fn();
    } catch (e) {
      toast(e.message, 'error');
      return null;
    } finally {
      st.busy = false;
      if (reloadAfter) await reload();
    }
  }
  const post = (path, payload) => api(`/repos/${id}${path}`, { body: payload });

  const stage = (files) => run(() => post('/stage', { paths: files.flatMap((f) => (f.oldPath ? [f.path, f.oldPath] : [f.path])) }));
  const unstage = (files) => run(() => post('/unstage', { paths: files.flatMap((f) => (f.oldPath ? [f.path, f.oldPath] : [f.path])) }));

  function undoToast(text, snapshot) {
    if (!snapshot) { toast(text, 'success'); return; }
    toast(text, 'success', {
      label: 'Undo',
      run: () => run(async () => { await post('/snapshots/restore', { name: snapshot.name }); toast('Restored', 'success'); }),
    });
  }

  async function discard(files) {
    const untracked = files.filter((f) => f.status === '?').length;
    const ok = await confirmDialog({
      title: files.length === 1 ? `Discard changes in ${files[0].path}?` : `Discard changes in ${plural(files.length, 'file')}?`,
      body: [
        h('p', {}, 'The unstaged changes of these files are removed from the working tree:'),
        h('pre', { class: 'confirm-list' }, files.slice(0, 12).map((f) => `${f.status}  ${f.path}`).join('\n') + (files.length > 12 ? `\n… and ${files.length - 12} more` : '')),
        untracked ? h('p', {}, h('b', {}, `${plural(untracked, 'untracked file')} will be deleted.`)) : null,
        h('p', { class: 'muted' }, 'The current content is saved in a safety snapshot first. You can undo the discard from the notification or from Safety snapshots.'),
      ],
      confirm: 'Discard', danger: true,
    });
    if (!ok) return;
    const res = await run(() => post('/discard', { paths: files.map((f) => f.path) }));
    if (res) undoToast(`Discarded ${plural(files.length, 'file')}`, res.snapshot);
  }

  // ---------------------------------------------------------------- lists
  function fileRow(f, side) {
    const slash = f.path.lastIndexOf('/');
    const active = st.open && st.open.path === f.path && st.open.side === side;
    const conflict = f.status === 'U';
    const actions = conflict
      ? [h('button', { class: 'btn sm', title: 'Use our version of the whole file', onclick: (e) => { e.stopPropagation(); ctx.resolveSide(f.path, 'ours'); } }, 'Ours'),
        h('button', { class: 'btn sm', title: 'Use their version of the whole file', onclick: (e) => { e.stopPropagation(); ctx.resolveSide(f.path, 'theirs'); } }, 'Theirs'),
        h('button', { class: 'icon-btn', title: 'Mark as resolved: stage the file as it is', onclick: (e) => { e.stopPropagation(); stage([f]); } }, icon('check', 14))]
      : side === 'unstaged'
      ? [h('button', { class: 'icon-btn danger', title: 'Discard changes', onclick: (e) => { e.stopPropagation(); discard([f]); } }, icon('undo', 13)),
        h('button', { class: 'icon-btn', title: 'Stage', onclick: (e) => { e.stopPropagation(); stage([f]); } }, icon('plus', 14))]
      : [h('button', { class: 'icon-btn', title: 'Unstage', onclick: (e) => { e.stopPropagation(); unstage([f]); } }, icon('minus', 14))];
    return h('div', {
      class: `file${active ? ' active' : ''}`, role: 'button', tabindex: '0', title: f.oldPath ? `${f.oldPath} → ${f.path}` : f.path,
      onclick: () => (conflict ? ctx.openConflict(f.path) : openFile(f, side)),
      ondblclick: () => { if (!conflict) (side === 'unstaged' ? stage : unstage)([f]); },
      onkeydown: (e) => { if (e.key === 'Enter') openFile(f, side); if (e.key === ' ') { e.preventDefault(); (side === 'unstaged' ? stage : unstage)([f]); } },
      oncontextmenu: (e) => contextMenu(e, [
        side === 'unstaged' ? { icon: 'plus', label: 'Stage', run: () => stage([f]) } : { icon: 'minus', label: 'Unstage', run: () => unstage([f]) },
        side === 'unstaged' ? { icon: 'undo', label: 'Discard changes…', danger: true, run: () => discard([f]) } : null,
        '-',
        ...(f.status === '?' ? [] : ctx.fileTools(f.path)),
        { icon: 'copy', label: 'Copy path', run: () => copyText(f.path) },
      ]),
    },
    h('span', { class: `st ${f.status === '?' ? 'q' : f.status}`, title: conflict ? 'Conflict: click to resolve' : STATUS_NAME[f.status] || '' }, f.status),
    h('span', { class: 'file-path' }, h('bdi', {}, slash >= 0 ? h('span', { class: 'file-dir' }, f.path.slice(0, slash + 1)) : null, f.path.slice(slash + 1))),
    h('span', { class: 'file-actions' }, actions));
  }

  function draw() {
    const s = st.status;
    if (st.error) { fill(lists, h('div', { class: 'error-box' }, icon('alert'), h('pre', {}, st.error))); return; }
    if (!s) { fill(lists, h('div', { class: 'detail-empty' }, h('span', { class: 'spinner' }))); return; }
    const section = (title, files, side, buttons) => [
      h('div', { class: 'wip-section' }, h('span', {}, title), h('span', { class: 'sb-count' }, fmtNum(files.length)), h('span', { class: 'spacer' }), files.length ? buttons : null),
      files.length ? h('div', { class: 'wip-files' }, files.map((f) => fileRow(f, side))) : h('div', { class: 'sb-empty small' }, side === 'unstaged' ? 'No unstaged changes' : 'Nothing is staged'),
    ];
    fill(lists,
      s.conflicted.length ? h('div', { class: 'notice' }, icon('alert'), `${plural(s.conflicted.length, 'file')} with conflicts`) : null,
      section('Unstaged', s.unstaged, 'unstaged', [
        h('button', { class: 'btn ghost sm', title: 'Discard all unstaged changes', onclick: () => discard(s.unstaged) }, icon('undo', 13), 'Discard all'),
        h('button', { class: 'btn sm', title: 'Stage all changes', onclick: () => stage(s.unstaged) }, icon('plus', 13), 'Stage all')]),
      section('Staged', s.staged, 'staged', [
        h('button', { class: 'btn sm', title: 'Unstage all changes', onclick: () => unstage(s.staged) }, icon('minus', 13), 'Unstage all')]));
    drawCommit();
  }

  function drawCommit() {
    const n = st.status?.staged.length || 0;
    const len = summary.value.length;
    counter.textContent = len ? String(len) : '';
    counter.classList.toggle('over', len > 72);
    counter.title = len > 72 ? 'Summaries longer than 72 characters are cut in many tools' : '';
    const canCommit = !!summary.value.trim() && (n > 0 || st.amend) && !(st.status?.conflicted.length);
    commitBtn.disabled = !canCommit;
    fill(commitBtn, icon('check', 14), st.amend ? 'Amend commit' : n ? `Commit ${plural(n, 'file')}` : 'Commit', h('span', { class: 'kbd' }, 'Ctrl+Enter'));
  }

  // ---------------------------------------------------------------- commit
  function publishedBranch() {
    const refs = ctx.getRefs();
    const b = refs?.branches.find((x) => x.current);
    return b && b.upstream && !b.gone && b.ahead === 0 ? b : null;
  }

  amendBox.addEventListener('change', async () => {
    st.amend = amendBox.checked;
    const pub = st.amend ? publishedBranch() : null;
    amendNote.hidden = !pub;
    if (pub) fill(amendNote, icon('alert', 13), `The last commit is already on ${pub.upstream}. Amending it rewrites published history.`);
    if (st.amend && !summary.value && !body.value) {
      const oid = ctx.getRefs()?.head?.oid;
      if (oid) {
        try {
          const c = await api(`/repos/${id}/commit/${oid}`);
          if (st.amend && !summary.value && !body.value) { summary.value = c.summary; body.value = c.body; }
        } catch { /* the user types the message */ }
      }
    }
    drawCommit();
  });
  summary.addEventListener('input', drawCommit);
  const keys = (e) => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); doCommit(); } };
  summary.addEventListener('keydown', keys);
  body.addEventListener('keydown', keys);

  async function doCommit() {
    if (commitBtn.disabled) return;
    const pub = st.amend ? publishedBranch() : null;
    if (pub) {
      const ok = await confirmDialog({
        title: 'Amend a published commit?',
        body: [h('p', {}, `The last commit is already on ${pub.upstream}. After the amend, your branch and ${pub.upstream} have different histories: a push needs force, and others who pulled the commit must repair their branches.`),
          h('p', { class: 'muted' }, 'The old commit stays in the reflog. You can undo the amend from the notification.')],
        confirm: 'Amend anyway', danger: true,
      });
      if (!ok) return;
    }
    const message = body.value.trim() ? `${summary.value.trim()}\n\n${body.value.trim()}` : summary.value.trim();
    const amend = st.amend;
    const res = await run(() => post('/commit', { message, amend }));
    if (!res) return;
    summary.value = '';
    body.value = '';
    amendBox.checked = false;
    st.amend = false;
    amendNote.hidden = true;
    drawCommit();
    const undo = res.previous ? {
      label: 'Undo',
      run: () => run(async () => { await post('/reset-soft', { to: res.previous, expectHead: res.oid }); toast('Commit undone. The changes are staged again.', 'success'); }),
    } : null;
    toast(`${amend ? 'Amended' : 'Committed'} ${res.oid.slice(0, 7)}`, 'success', undo);
    ctx.afterCommit?.(res);
  }

  // ---------------------------------------------------------------- diff with staging
  const pickedCount = () => [...st.picked.values()].reduce((n, s) => n + s.size, 0);

  function openFile(f, side) {
    st.open = { path: f.path, side, oldPath: f.oldPath || null };
    st.picked.clear();
    st.lastPick = null;
    let diff = null;
    const actions = h('span', { class: 'diff-actions' });
    const hunkSel = (hi, linesOf) => ({ index: hi, header: diff.hunks[hi].header, lines: linesOf });
    const apply = async (op, hunks, label) => {
      const res = await run(() => post('/apply', { op, path: f.path, hunks }));
      if (!res) return;
      if (op === 'discard') undoToast(label, res.snapshot);
    };
    const discardConfirm = async (hunks, what) => {
      const ok = await confirmDialog({
        title: `Discard ${what}?`,
        body: [h('p', {}, `${what[0].toUpperCase()}${what.slice(1)} of ${f.path} will be removed from the working tree.`),
          h('p', { class: 'muted' }, 'The file is saved in a safety snapshot first. You can undo the discard from the notification.')],
        confirm: 'Discard', danger: true,
      });
      if (ok) apply('discard', hunks, `Discarded ${what}`);
    };
    const drawActions = () => {
      const n = pickedCount();
      const sel = () => [...st.picked.entries()].filter(([, s]) => s.size).map(([hi, s]) => hunkSel(hi, [...s]));
      if (!n) {
        fill(actions, h('span', { class: 'dim small' }, store.get('gl.diffMode', 'unified') === 'split' ? 'Switch to Unified to select single lines' : 'Click line numbers to select lines'));
      } else if (side === 'unstaged') {
        fill(actions,
          h('button', { class: 'btn sm danger-ghost', onclick: () => discardConfirm(sel(), plural(n, 'selected line')) }, icon('undo', 13), `Discard ${plural(n, 'line')}`),
          h('button', { class: 'btn sm primary', onclick: () => apply('stage', sel()) }, icon('plus', 13), `Stage ${plural(n, 'line')}`));
      } else {
        fill(actions, h('button', { class: 'btn sm primary', onclick: () => apply('unstage', sel()) }, icon('minus', 13), `Unstage ${plural(n, 'line')}`));
      }
    };
    const panel = createDiffPanel({
      file: f,
      subtitle: side === 'staged' ? 'staged' : 'unstaged',
      extraActions: actions,
      load: async () => {
        diff = await api(`/repos/${id}/diff?${qs({ side, path: f.path, oldPath: f.oldPath })}`);
        st.picked.clear();
        drawActions();
        return diff;
      },
      renderOptions: (d, mode) => (queueMicrotask(drawActions), {
        hunkActions: d.binary ? null : (_hunk, hi) => (side === 'unstaged'
          ? [h('button', { class: 'btn sm ghost', title: 'Discard this hunk', onclick: () => discardConfirm([hunkSel(hi, null)], 'this hunk') }, icon('undo', 12), 'Discard hunk'),
            h('button', { class: 'btn sm', title: 'Stage this hunk', onclick: () => apply('stage', [hunkSel(hi, null)]) }, icon('plus', 12), 'Stage hunk')]
          : [h('button', { class: 'btn sm', title: 'Unstage this hunk', onclick: () => apply('unstage', [hunkSel(hi, null)]) }, icon('minus', 12), 'Unstage hunk')]),
        // Lines are selected in the unified view; the split view shows whole hunks.
        decorate: mode !== 'unified' || d.binary ? null : (lineEl, l, hi, li) => {
          if (l.t !== '+' && l.t !== '-') return;
          lineEl.classList.add('pickable');
          lineEl.dataset.pick = `${hi}:${li}`;
          const toggle = (e) => {
            e.preventDefault();
            const set = st.picked.get(hi) || new Set();
            st.picked.set(hi, set);
            const on = !set.has(li);
            // Shift+click selects the changed lines between the last click and this one.
            const range = e.shiftKey && st.lastPick && st.lastPick.hi === hi ? [Math.min(st.lastPick.li, li), Math.max(st.lastPick.li, li)] : [li, li];
            for (let i = range[0]; i <= range[1]; i++) {
              const t = d.hunks[hi].lines[i].t;
              if (t !== '+' && t !== '-') continue;
              if (on) set.add(i); else set.delete(i);
              panel.el.querySelector(`[data-pick="${hi}:${i}"]`)?.classList.toggle('picked', on);
            }
            st.lastPick = { hi, li };
            drawActions();
          };
          for (const cell of lineEl.querySelectorAll('.dl-no, .dl-sign')) cell.addEventListener('mousedown', toggle);
        },
      }),
      onClose: () => { st.open = null; st.picked.clear(); draw(); },
    });
    ctx.showDiff(panel);
    draw();
  }

  // ---------------------------------------------------------------- snapshots
  async function openSnapshots() {
    let list;
    try { list = await api(`/repos/${id}/snapshots`); } catch (e) { toast(e.message, 'error'); return; }
    const box = h('div', { class: 'snap-list' });
    const drawList = () => fill(box, list.length ? list.map((s) => h('div', { class: 'snap' },
      h('div', { class: 'snap-text' },
        h('div', {}, h('b', {}, s.reason), h('span', { class: 'muted' }, ` · ${new Date(s.time * 1000).toLocaleString()}`)),
        h('div', { class: 'muted small mono', title: s.files.join('\n') }, s.files.slice(0, 3).join(', ') + (s.files.length > 3 ? ` and ${s.files.length - 3} more` : ''))),
      h('button', { class: 'btn sm', title: 'Write these files back into the working tree', onclick: async () => {
        const ok = await confirmDialog({ title: 'Restore this snapshot?', body: [h('p', {}, `${plural(s.files.length, 'file')} in the working tree will be replaced with the saved content. The current content is saved in a new snapshot first.`)], confirm: 'Restore' });
        if (!ok) return;
        const res = await run(() => post('/snapshots/restore', { name: s.name }));
        if (res) { toast(`Restored ${plural(res.files.length, 'file')}`, 'success'); close(); }
      } }, 'Restore'),
      h('button', { class: 'icon-btn danger', title: 'Delete this snapshot', onclick: async () => {
        try { await post('/snapshots/drop', { name: s.name }); list = list.filter((x) => x.name !== s.name); drawList(); } catch (e) { toast(e.message, 'error'); }
      } }, icon('trash', 14)))) : h('div', { class: 'muted' }, 'No snapshots yet. Graftline saves one each time it discards or overwrites files in the working tree.'));
    drawList();
    const close = openModal('Safety snapshots', box, null, { wide: true });
  }

  // ---------------------------------------------------------------- reload
  let timer = 0;
  let seq = 0;
  async function reload() {
    const mine = ++seq;
    try {
      const status = await api(`/repos/${id}/status`);
      if (mine !== seq) return;
      st.status = status;
      st.error = null;
    } catch (e) {
      if (mine !== seq) return;
      st.error = e.message;
    }
    ctx.onStatus?.(st.status);
    // Keep the open diff in step with the file, or close it when the file has no changes left.
    if (st.open && st.status) {
      const list = st.open.side === 'staged' ? st.status.staged : st.status.unstaged;
      if (list.some((f) => f.path === st.open.path)) ctx.currentDiff()?.reload();
      else ctx.closeDiff();
    }
    draw();
  }
  const reloadSoon = () => { clearTimeout(timer); timer = setTimeout(reload, 80); };

  draw();
  return { el, reload, reloadSoon, get status() { return st.status; }, focusMessage: () => summary.focus() };
}

const STATUS_NAME = { A: 'Added', M: 'Modified', D: 'Deleted', R: 'Renamed', C: 'Copied', T: 'Type changed', U: 'Conflict', '?': 'Untracked' };
