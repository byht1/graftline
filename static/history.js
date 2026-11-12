// Graftline — operations that change history (merge, rebase, cherry-pick, revert, reset,
// interactive rebase), the banner of an operation in progress, and the conflict editor.

import { h, fill, icon, api, qs, toast, openModal, confirmDialog, showError, fmtNum } from '/ui.js';

const plural = (n, word) => `${fmtNum(n)} ${word}${n === 1 ? '' : 's'}`;
const STATE_NAME = { merge: 'Merge', rebase: 'Rebase', 'cherry-pick': 'Cherry-pick', revert: 'Revert', bisect: 'Bisect', am: 'Patch apply' };

/**
 * Splits a file with conflict markers into plain text and conflict blocks.
 * Returns [{ text: [lines] } | { ours: [lines], base: [lines], theirs: [lines] }].
 */
export function parseConflicts(text) {
  const out = [];
  let plain = [];
  let block = null;
  let side = null;
  for (const line of text.split('\n')) {
    if (!block && line.startsWith('<<<<<<<')) {
      if (plain.length) out.push({ text: plain });
      plain = [];
      block = { ours: [], base: [], theirs: [] };
      side = 'ours';
    } else if (block && side === 'ours' && line.startsWith('|||||||')) side = 'base';
    else if (block && side !== 'theirs' && line.startsWith('=======')) side = 'theirs';
    else if (block && side === 'theirs' && line.startsWith('>>>>>>>')) {
      out.push(block);
      block = null;
      side = null;
    } else if (block) block[side].push(line);
    else plain.push(line);
  }
  // An unfinished block is not a conflict: give its lines back as text.
  if (block) plain.push('<<<<<<<', ...block.ours, ...block.theirs);
  if (plain.length) out.push({ text: plain });
  return out;
}

/** `ctx`: { repoId, getRefs(), getStatus(), actions, showPanel(panel), closePanel(), banner (element) }. */
export function createHistory(ctx) {
  const id = ctx.repoId;
  const refs = () => ctx.getRefs();
  const cur = () => refs()?.branches.find((b) => b.current)?.name || null;

  async function run(payload) {
    try {
      return await api(`/repos/${id}/history`, { body: payload });
    } catch (e) {
      showError(e);
      return null;
    }
  }

  /** Offers to undo an operation by moving the branch back to the commit it had before. */
  function done(text, before) {
    toast(text, 'success', before ? {
      label: 'Undo',
      run: async () => { const r = await run({ op: 'reset', to: before, mode: 'hard' }); if (r) toast('Undone', 'success'); },
    } : null);
  }

  /** Handles the answer of an operation that can stop for conflicts. */
  function finish(res, text) {
    if (!res) return;
    if (res.stopped) {
      toast(`${STATE_NAME[res.stopped] || 'The operation'} stopped because of conflicts. Resolve them, then continue.`, 'error');
      ctx.showChanges();
    } else done(text, res.before);
  }

  // ---------------------------------------------------------------- operations
  async function merge(branch, into) {
    const res = await ctx.actions.op({ op: 'merge', branch, into });
    if (res && !res.stopped && /Already up to date/.test(res.output)) { toast(`${into} already contains ${branch}`); return; }
    finish(res, `Merged ${branch} into ${into}`);
  }

  async function rebase(branch, onto) {
    let info;
    try { info = await api(`/repos/${id}/rebase-info?${qs({ branch, onto })}`); } catch (e) { showError(e); return; }
    if (!info.commits) { toast(`${branch} is already based on ${onto}`); return; }
    if (info.published) {
      const ok = await confirmDialog({
        title: `Rebase ${plural(info.published, 'published commit')}?`,
        body: [h('p', {}, `${plural(info.published, 'commit')} of ${branch} ${info.published === 1 ? 'is' : 'are'} already on its upstream. A rebase replaces them with new commits.`),
          h('p', {}, 'After the rebase the branch and its upstream have different histories: a push needs force, and others who use the branch must repair their copies.'),
          h('p', { class: 'muted' }, 'The old commits stay in the reflog. You can undo the rebase from the notification.')],
        confirm: 'Rebase anyway', danger: true,
      });
      if (!ok) return;
    }
    finish(await ctx.actions.op({ op: 'rebase', branch, onto }), `Rebased ${branch} onto ${onto}`);
  }

  const cherryPick = async (row) => finish(await run({ op: 'cherry-pick', oids: [row.o] }), `Cherry-picked ${row.o.slice(0, 7)}`);
  const revert = async (row) => finish(await run({ op: 'revert', oid: row.o }), `Reverted ${row.o.slice(0, 7)}`);

  async function reset(row, mode) {
    const branch = cur() || 'HEAD';
    let lost = 0;
    try { lost = (await api(`/repos/${id}/rebase-info?${qs({ onto: row.o })}`)).commits; } catch { /* the dialog still describes the reset */ }
    const status = ctx.getStatus();
    const dirty = status ? new Set([...status.staged, ...status.unstaged].map((f) => f.path)).size : 0;
    const what = { soft: 'The changes of the removed commits stay staged.', mixed: 'The changes of the removed commits stay in the working tree, unstaged.', hard: 'The index and the working tree are made equal to this commit.' }[mode];
    if (mode === 'hard' || lost) {
      const ok = await confirmDialog({
        title: `Reset ${branch} to ${row.o.slice(0, 7)} (${mode})?`,
        body: [
          h('p', {}, what),
          lost ? h('p', {}, h('b', {}, `${plural(lost, 'commit')} will no longer be on ${branch}.`), ' They stay reachable through the reflog, and you can undo the reset from the notification.') : null,
          mode === 'hard' && dirty ? h('p', {}, h('b', {}, `Uncommitted changes in ${plural(dirty, 'file')} will be overwritten.`), ' They are saved in a safety snapshot first.') : null,
        ],
        confirm: `Reset (${mode})`, danger: mode === 'hard',
      });
      if (!ok) return;
    }
    const res = await run({ op: 'reset', to: row.o, mode });
    if (!res) return;
    toast(`Reset ${branch} to ${row.o.slice(0, 7)}`, 'success', {
      label: 'Undo',
      run: async () => {
        const back = await run({ op: 'reset', to: res.before, mode: 'hard' });
        if (!back) return;
        if (res.snapshot) await api(`/repos/${id}/snapshots/restore`, { body: { name: res.snapshot.name } }).catch(() => {});
        toast('Undone', 'success');
      },
    });
  }

  /** Extra items for the context menu of a commit. */
  function commitItems(row) {
    const branch = cur();
    const isHead = (row.r || []).some((l) => l.current);
    return [
      '-',
      { icon: 'download', label: 'Cherry-pick onto the current branch', disabled: isHead, run: () => cherryPick(row) },
      { icon: 'undo', label: 'Revert this commit', sub: 'Adds a commit that undoes it', run: () => revert(row) },
      branch ? { icon: 'edit', label: 'Rebase interactively from here…', sub: 'Reorder, squash, reword or drop the commits after this one', disabled: isHead, run: () => interactiveRebase(row) } : null,
      '-',
      { title: `Reset ${branch || 'HEAD'} to this commit` },
      { icon: 'left', label: 'Soft', sub: 'Keep the changes staged', disabled: isHead, run: () => reset(row, 'soft') },
      { icon: 'left', label: 'Mixed', sub: 'Keep the changes in the working tree', disabled: isHead, run: () => reset(row, 'mixed') },
      { icon: 'left', label: 'Hard…', sub: 'Discard the changes', danger: true, run: () => reset(row, 'hard') },
    ];
  }

  // ---------------------------------------------------------------- interactive rebase
  async function interactiveRebase(row) {
    let plan;
    try { plan = await api(`/repos/${id}/rebase-plan?${qs({ base: row.o })}`); } catch (e) { showError(e); return; }
    if (!plan.length) { toast('There are no commits after this one on the current branch'); return; }
    if (plan.some((c) => c.merge)) {
      toast('The range contains merge commits. An interactive rebase would flatten them, so it is not offered here.', 'error');
      return;
    }
    const steps = plan.map((c) => ({ ...c, action: 'pick', newMessage: c.message }));
    const list = h('div', { class: 'rb-list' });
    const note = h('div', { class: 'form-status' });
    const start = h('button', { class: 'btn primary' }, 'Start rebase');
    const published = plan.filter((c) => c.published).length;
    const move = (i, d) => { const j = i + d; if (j < 0 || j >= steps.length) return; [steps[i], steps[j]] = [steps[j], steps[i]]; draw(); };
    const validate = () => {
      const first = steps.find((s) => s.action !== 'drop');
      let problem = null;
      if (!first) problem = 'Every commit is dropped. To remove them all, reset the branch instead.';
      else if (first.action === 'squash' || first.action === 'fixup') problem = 'The first commit cannot be melded: there is no commit before it.';
      else if (steps.some((s) => s.action === 'reword' && !s.newMessage.trim())) problem = 'Enter a message for each reworded commit.';
      note.className = `form-status${problem ? ' error' : ''}`;
      note.textContent = problem || '';
      start.disabled = !!problem;
    };
    function draw() {
      fill(list, steps.map((s, i) => {
        const select = h('select', { class: 'input sm rb-action', onchange: (e) => { s.action = e.target.value; draw(); } },
          ['pick', 'reword', 'squash', 'fixup', 'drop'].map((a) => h('option', { value: a, selected: a === s.action }, a)));
        const row = h('div', { class: `rb-row ${s.action}` },
          h('div', { class: 'rb-main' },
            h('span', { class: 'rb-move' },
              h('button', { class: 'icon-btn', title: 'Move up (earlier)', disabled: i === 0, onclick: () => move(i, -1) }, icon('up', 13)),
              h('button', { class: 'icon-btn', title: 'Move down (later)', disabled: i === steps.length - 1, onclick: () => move(i, 1) }, icon('down', 13))),
            select,
            h('span', { class: 'mono dim small' }, s.oid.slice(0, 7)),
            h('span', { class: 'rb-summary', title: s.message }, s.action === 'reword' ? s.newMessage.split('\n')[0] || s.summary : s.summary),
            s.published ? h('span', { class: 'badge warn', title: 'This commit is already on the upstream branch' }, 'published') : null,
            h('span', { class: 'muted small' }, s.author)),
          s.action === 'reword' ? h('textarea', {
            class: 'input area rb-msg', rows: '3', value: s.newMessage, spellcheck: 'true',
            oninput: (e) => { s.newMessage = e.target.value; validate(); },
          }) : null);
        return row;
      }));
      validate();
    }
    start.addEventListener('click', async () => {
      if (published) {
        const ok = await confirmDialog({
          title: `Rewrite ${plural(published, 'published commit')}?`,
          body: [h('p', {}, `${plural(published, 'commit')} in this list ${published === 1 ? 'is' : 'are'} already on the upstream branch. The rebase replaces them with new commits; a push then needs force.`),
            h('p', { class: 'muted' }, 'You can undo the rebase from the notification.')],
          confirm: 'Rewrite anyway', danger: true,
        });
        if (!ok) return;
      }
      close();
      const res = await run({
        op: 'rebase-interactive', base: row.o,
        steps: steps.map((s) => ({ oid: s.oid, action: s.action, message: s.action === 'reword' ? s.newMessage : null })),
      });
      finish(res, 'Rebase finished');
    });
    draw();
    const close = openModal(`Rebase ${plural(plan.length, 'commit')} interactively`, h('div', { class: 'rb' },
      h('div', { class: 'muted small' }, 'The list is applied from top to bottom, oldest commit first. ',
        h('b', {}, 'squash'), ' melds a commit into the one above and keeps both messages; ', h('b', {}, 'fixup'), ' melds it and drops its message. To write one message for a melded group, set its first commit to ', h('b', {}, 'reword'), '.'),
      published ? h('div', { class: 'wip-warn' }, icon('alert', 13), `${plural(published, 'commit')} here ${published === 1 ? 'is' : 'are'} already published.`) : null,
      list, note),
    [h('button', { class: 'btn', onclick: () => close() }, 'Cancel'), start], { wide: true });
  }

  // ---------------------------------------------------------------- operation in progress
  async function continueOp() { finish(await run({ op: 'continue' }), 'Finished'); }
  async function skipOp() { finish(await run({ op: 'skip' }), 'Skipped the commit'); }
  async function abortOp() {
    const state = refs()?.state;
    const ok = await confirmDialog({
      title: `Abort the ${(STATE_NAME[state] || 'operation').toLowerCase()}?`,
      body: [h('p', {}, 'The repository returns to the state before the operation started. Conflict resolutions made so far are discarded.'),
        h('p', { class: 'muted' }, 'The changed files are saved in a safety snapshot first.')],
      confirm: 'Abort', danger: true,
    });
    if (!ok) return;
    const res = await run({ op: 'abort' });
    if (res) toast('Aborted', 'success');
  }

  /** Shows or hides the banner of an operation in progress. */
  function drawBanner() {
    const state = refs()?.state;
    const banner = ctx.banner;
    banner.hidden = !state;
    if (!state) return;
    const conflicted = ctx.getStatus()?.conflicted || [];
    const canSkip = state === 'rebase' || state === 'cherry-pick' || state === 'revert';
    fill(banner, icon('alert'),
      h('span', {}, h('b', {}, `${STATE_NAME[state] || state} in progress. `),
        conflicted.length ? `${plural(conflicted.length, 'file')} with conflicts: resolve them, then continue.` : 'All conflicts are resolved. Continue to finish.'),
      h('span', { class: 'spacer' }),
      conflicted.length ? h('button', { class: 'btn sm', onclick: () => openConflict(conflicted[0]) }, 'Resolve') : null,
      h('button', { class: 'btn sm primary', disabled: conflicted.length > 0, title: conflicted.length ? 'Resolve all conflicts first' : '', onclick: continueOp }, 'Continue'),
      canSkip ? h('button', { class: 'btn sm', title: 'Leave this commit out and go on', onclick: skipOp }, 'Skip') : null,
      h('button', { class: 'btn sm danger-ghost', onclick: abortOp }, 'Abort'));
  }

  // ---------------------------------------------------------------- conflicts
  async function resolveSide(path, side) {
    try {
      await api(`/repos/${id}/resolve`, { body: { path, choice: side } });
      toast(`${path}: took ${side === 'ours' ? 'our' : 'their'} version`, 'success');
    } catch (e) { showError(e); }
  }

  async function openConflict(path) {
    let d;
    try { d = await api(`/repos/${id}/conflict?${qs({ path })}`); } catch (e) { showError(e); return; }
    const segments = d.merged != null && !d.binary ? parseConflicts(d.merged) : [];
    const blocks = segments.filter((s) => !s.text);
    // Per block: which lines of each side go into the result, and whether the user decided.
    for (const b of blocks) { b.pickOurs = new Set(); b.pickTheirs = new Set(); b.decided = false; b.theirsFirst = false; }
    let manual = null; // the hand-edited result, or null while the blocks decide
    const body = h('div', { class: 'cf-body' });
    const save = h('button', { class: 'btn primary sm' }, icon('check', 13), 'Mark resolved');
    const info = h('span', { class: 'muted small' });

    const blockLines = (b) => {
      const ours = b.ours.filter((_, i) => b.pickOurs.has(i));
      const theirs = b.theirs.filter((_, i) => b.pickTheirs.has(i));
      return b.theirsFirst ? [...theirs, ...ours] : [...ours, ...theirs];
    };
    const result = () => segments.flatMap((s) => (s.text ? s.text : blockLines(s))).join('\n');
    const setAll = (b, ours, theirs) => {
      b.pickOurs = new Set(ours ? b.ours.map((_, i) => i) : []);
      b.pickTheirs = new Set(theirs ? b.theirs.map((_, i) => i) : []);
      b.decided = true;
      draw();
    };

    function side(b, key, label, cls) {
      const picked = key === 'ours' ? b.pickOurs : b.pickTheirs;
      return h('div', { class: `cf-side ${cls}` },
        h('div', { class: 'cf-side-head' }, h('b', {}, key === 'ours' ? 'Ours' : 'Theirs'), h('span', { class: 'muted' }, ` · ${label}`)),
        b[key].length ? b[key].map((line, i) => h('label', { class: `cf-line${picked.has(i) ? ' on' : ''}` },
          h('input', { type: 'checkbox', checked: picked.has(i), onchange: (e) => { if (e.target.checked) picked.add(i); else picked.delete(i); b.decided = true; draw(); } }),
          h('span', { class: 'cf-text' }, line || ' '))) : h('div', { class: 'cf-empty' }, '(no lines on this side)'));
    }

    function draw() {
      const left = blocks.filter((b) => !b.decided).length;
      if (manual != null) {
        const area = h('textarea', { class: 'input area cf-edit', spellcheck: 'false', value: manual, oninput: (e) => { manual = e.target.value; update(); } });
        fill(body, h('div', { class: 'cf-note' }, 'You edit the result by hand. ',
          blocks.length ? h('button', { class: 'link-btn', onclick: async () => {
            if (manual !== result() && !(await confirmDialog({ title: 'Go back to the conflict blocks?', body: [h('p', {}, 'Your manual edits of the result are replaced by the selection in the blocks.')], confirm: 'Discard my edits', danger: true }))) return;
            manual = null; draw();
          } }, 'Back to the conflict blocks') : null), area);
      } else if (!blocks.length) {
        fill(body, h('div', { class: 'diff-note' }, d.binary ? 'This file is binary or not UTF-8 text. Choose one side for the whole file.'
          : d.kind === 'deleted-by-us' ? 'We deleted this file; they changed it. Keep their file or keep it deleted.'
            : d.kind === 'deleted-by-them' ? 'They deleted this file; we changed it. Keep our file or delete it.'
              : 'The file has no conflict markers. Choose one side, or edit the result by hand.'));
      } else {
        let n = 0;
        fill(body, segments.map((s) => {
          if (s.text) return h('pre', { class: 'cf-plain' }, s.text.length > 8 ? [...s.text.slice(0, 3), `… ${s.text.length - 6} unchanged lines …`, ...s.text.slice(-3)].join('\n') : s.text.join('\n'));
          n += 1;
          const out = blockLines(s);
          return h('div', { class: `cf-block${s.decided ? ' decided' : ''}` },
            h('div', { class: 'cf-block-head' }, h('b', {}, `Conflict ${n} of ${blocks.length}`), h('span', { class: 'spacer' }),
              h('button', { class: 'btn sm', onclick: () => setAll(s, true, false) }, 'Take ours'),
              h('button', { class: 'btn sm', onclick: () => setAll(s, false, true) }, 'Take theirs'),
              h('button', { class: 'btn sm', onclick: () => setAll(s, true, true) }, 'Take both'),
              h('button', { class: 'btn sm ghost', title: 'Put their lines before ours in the result', onclick: () => { s.theirsFirst = !s.theirsFirst; draw(); } }, s.theirsFirst ? 'Theirs first' : 'Ours first')),
            h('div', { class: 'cf-sides' }, side(s, 'ours', d.oursLabel, 'ours'), side(s, 'theirs', d.theirsLabel, 'theirs')),
            h('div', { class: 'cf-result' }, h('div', { class: 'cf-side-head' }, h('b', {}, 'Result'), s.decided ? null : h('span', { class: 'muted' }, ' · choose lines above')),
              s.decided ? h('pre', { class: 'cf-plain out' }, out.length ? out.join('\n') : '(both sides removed)') : null));
        }));
        info.textContent = left ? `${plural(left, 'conflict')} left to decide` : 'All conflicts are decided';
      }
      update();
    }
    function update() {
      const left = blocks.filter((b) => !b.decided).length;
      save.disabled = manual == null && (blocks.length === 0 || left > 0);
      if (manual != null) info.textContent = /^(<<<<<<<|=======|>>>>>>>)/m.test(manual) ? 'The text still has conflict markers' : 'Edited by hand';
    }
    save.addEventListener('click', async () => {
      const content = manual != null ? manual : result();
      if (/^(<<<<<<<|>>>>>>>)/m.test(content) && !(await confirmDialog({ title: 'Save with conflict markers?', body: [h('p', {}, 'The text still contains conflict markers. The file is staged as it is.')], confirm: 'Save anyway', danger: true }))) return;
      try {
        await api(`/repos/${id}/resolve`, { body: { path, choice: 'content', content } });
        toast(`${path} resolved`, 'success');
        panel.close();
        const next = (ctx.getStatus()?.conflicted || []).find((p) => p !== path);
        if (next) openConflict(next);
      } catch (e) { showError(e); }
    });
    const wholeFile = async (which) => { await resolveSide(path, which); panel.close(); };
    const el = h('div', { class: 'diff-view' },
      h('div', { class: 'diff-head' },
        h('div', { class: 'diff-title' }, h('span', { class: 'st U' }, 'U'), h('span', { class: 'path', title: path }, path), h('span', { class: 'muted' }, d.kind.replace(/-/g, ' '))),
        info,
        h('button', { class: 'btn sm', title: `Use our version of the whole file: ${d.oursLabel}`, onclick: () => wholeFile('ours') }, d.kind === 'deleted-by-us' ? 'Keep deleted' : 'Use ours'),
        h('button', { class: 'btn sm', title: `Use their version of the whole file: ${d.theirsLabel}`, onclick: () => wholeFile('theirs') }, d.kind === 'deleted-by-them' ? 'Delete file' : 'Use theirs'),
        d.binary || d.merged == null ? null : h('button', { class: 'btn sm', title: 'Edit the resulting file as text', onclick: () => { if (manual == null) { manual = blocks.some((b) => !b.decided) ? d.merged : result(); draw(); } } }, icon('edit', 13), 'Edit by hand'),
        save,
        h('button', { class: 'icon-btn', title: 'Close (Esc)', onclick: () => panel.close() }, icon('x'))),
      body);
    const panel = { el, close: () => { el.remove(); ctx.panelClosed(panel); }, reload: () => {}, file: { path } };
    ctx.showPanel(panel);
    draw();
  }

  return { merge, rebase, commitItems, drawBanner, openConflict, resolveSide };
}
