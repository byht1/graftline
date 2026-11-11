// Graftline — repository actions: toolbar buttons, context menus, dialogs, network jobs
// and dragging one branch onto another.

import { h, fill, icon, api, toast, copyText, contextMenu, openMenu, openModal, menuItems, confirmDialog, formModal, showError, fmtNum } from '/ui.js';

const plural = (n, word) => `${fmtNum(n)} ${word}${n === 1 ? '' : 's'}`;

/**
 * `ctx`: { repoId, getRefs(), hasGit(), selectOid(oid), history: { merge, rebase, … } (set later) }.
 */
export function createActions(ctx) {
  const id = ctx.repoId;
  const refs = () => ctx.getRefs();
  const current = () => refs()?.branches.find((b) => b.current) || null;
  const remoteOf = (upstream) => {
    // The longest remote name that is a prefix wins: "origin/team" before "origin".
    const names = (refs()?.remotes || []).map((r) => r.name).sort((a, b) => b.length - a.length);
    const remote = names.find((n) => upstream.startsWith(`${n}/`));
    return remote ? { remote, branch: upstream.slice(remote.length + 1) } : null;
  };

  /** Runs a local operation. Returns the result, or null after showing the error. */
  async function op(payload) {
    try {
      return await api(`/repos/${id}/op`, { body: payload });
    } catch (e) {
      showError(e);
      return null;
    }
  }

  // ---------------------------------------------------------------- network jobs
  const running = new Map();
  const waiters = new Map();
  const statusEl = h('div', { class: 'job-status', hidden: true });

  function drawJobs() {
    const job = [...running.values()].pop();
    statusEl.hidden = !job;
    if (!job) return;
    fill(statusEl, h('span', { class: 'spinner' }),
      h('span', { class: 'job-text', title: `${job.command}\n${job.line}` }, job.percent != null ? `${job.title} ${job.percent}%` : job.title, job.line ? h('span', { class: 'dim' }, ` · ${job.line}`) : null),
      h('button', { class: 'icon-btn', title: 'Cancel', onclick: () => api(`/jobs/${job.id}`, { method: 'DELETE' }).catch(() => {}) }, icon('x', 13)),
      h('div', { class: 'job-bar' }, h('div', { style: { width: `${job.percent ?? 0}%` }, class: job.percent == null ? 'indeterminate' : '' })));
  }

  /** Called for every `job` event of this repository. */
  function onJob(job) {
    if (job.state === 'running') {
      running.set(job.id, job);
    } else {
      running.delete(job.id);
      const done = waiters.get(job.id);
      waiters.delete(job.id);
      if (job.state === 'done') toast(`${job.title}: done`, 'success');
      else if (job.state === 'cancelled') toast(`${job.title}: cancelled`);
      else showError({ command: job.command, output: job.output || job.line, message: job.output || job.line }, `${job.title} failed`);
      done?.(job.state === 'done');
    }
    drawJobs();
    drawToolbar();
  }

  /** Starts a network job. Resolves to true when it succeeds. */
  async function net(payload) {
    if (!ctx.hasGit()) { toast('Git was not found on PATH. Install Git to use the network.', 'error'); return false; }
    try {
      const job = await api(`/repos/${id}/job`, { body: payload });
      running.set(job.id, job);
      drawJobs();
      drawToolbar();
      return await new Promise((resolve) => waiters.set(job.id, resolve));
    } catch (e) {
      showError(e);
      return false;
    }
  }

  const fetchAll = (prune = false) => net({ job: 'fetch', prune });
  const pull = (mode = null) => net({ job: 'pull', mode });

  async function addRemote() {
    return formModal({
      title: 'Add a remote', confirm: 'Add',
      fields: [{ key: 'name', label: 'Name', value: refs()?.remotes.length ? '' : 'origin' }, { key: 'url', label: 'URL', placeholder: 'https://example.com/team/project.git or git@example.com:team/project.git' }],
      submit: (v) => api(`/repos/${id}/op`, { body: { op: 'remote-add', name: v.name.trim(), url: v.url.trim() } }),
    });
  }

  async function push({ branch = null, force = false } = {}) {
    const b = branch ? refs()?.branches.find((x) => x.name === branch) : current();
    if (!b) { toast('Check out a branch to push it', 'error'); return; }
    const remotes = refs()?.remotes || [];
    if (!remotes.length) { if (await addRemote()) toast('Remote added. Push again.'); return; }
    const up = b.upstream && !b.gone ? remoteOf(b.upstream) : null;
    let target = up ? { remote: up.remote, remoteName: up.branch, setUpstream: false } : null;
    if (!target) {
      const v = await formModal({
        title: `Push ${b.name}`, confirm: 'Push',
        intro: h('div', { class: 'muted small' }, 'This branch has no upstream yet.'),
        fields: [
          { key: 'remote', label: 'Remote', type: 'select', options: remotes.map((r) => r.name), value: remotes.find((r) => r.name === 'origin')?.name || remotes[0].name },
          { key: 'name', label: 'Branch name on the remote', value: b.name },
          { key: 'track', label: 'Set as upstream of this branch', type: 'checkbox', value: true },
        ],
      });
      if (!v) return;
      target = { remote: v.remote, remoteName: v.name.trim() || b.name, setUpstream: v.track };
    }
    if (force) {
      const ok = await confirmDialog({
        title: `Force push ${b.name}?`,
        body: [
          h('p', {}, `The branch ${target.remote}/${target.remoteName} is replaced with your local ${b.name}.`),
          b.behind ? h('p', {}, h('b', {}, `${plural(b.behind, 'commit')} on the remote that your branch does not have will be removed from it.`)) : null,
          h('p', { class: 'muted' }, 'The push uses --force-with-lease: it stops if the remote has commits that you have not fetched yet. Other people who pulled the old commits must repair their branches.'),
        ],
        confirm: 'Force push', danger: true,
      });
      if (!ok) return;
    }
    await net({ job: 'push', remote: target.remote, refname: b.name, remoteName: target.remoteName, setUpstream: target.setUpstream, force });
  }

  // ---------------------------------------------------------------- branches
  async function createBranch(at = null, atLabel = null) {
    return formModal({
      title: 'Create a branch', confirm: 'Create',
      intro: h('div', { class: 'muted small' }, atLabel ? `At ${atLabel}` : 'At the current commit (HEAD)'),
      fields: [{ key: 'name', label: 'Branch name', placeholder: 'feature/my-change' }, { key: 'checkout', label: 'Check out the new branch', type: 'checkbox', value: true }],
      submit: (v) => api(`/repos/${id}/op`, { body: { op: 'branch-create', name: v.name.trim(), at, checkout: v.checkout } }),
    });
  }

  const renameBranch = (name) => formModal({
    title: `Rename ${name}`, confirm: 'Rename',
    fields: [{ key: 'to', label: 'New name', value: name }],
    submit: (v) => api(`/repos/${id}/op`, { body: { op: 'branch-rename', name, to: v.to.trim() } }),
  });

  async function deleteBranch(name) {
    let res = await op({ op: 'branch-delete', name });
    if (!res) return;
    if (res.needsForce) {
      const list = res.commits.slice(0, 10).map((c) => `${c.oid.slice(0, 7)}  ${c.summary}`).join('\n') + (res.commits.length > 10 ? `\n… and ${res.commits.length - 10} more` : '');
      const ok = await confirmDialog({
        title: `Delete ${name}?`,
        body: [
          h('p', {}, h('b', {}, `${plural(res.commits.length, 'commit')} exist only on this branch.`), ' After the delete no branch or tag leads to them:'),
          h('pre', { class: 'confirm-list' }, list),
          h('p', { class: 'muted' }, 'Git keeps unreachable commits for a limited time (the reflog). You can undo the delete from the notification.'),
        ],
        confirm: 'Delete branch', danger: true,
      });
      if (!ok) return;
      res = await op({ op: 'branch-delete', name, force: true });
      if (!res) return;
    }
    toast(`Deleted branch ${name}`, 'success', { label: 'Undo', run: () => op({ op: 'branch-restore', name, oid: res.oid }) });
  }

  const checkout = async (target, kind) => {
    const res = await op({ op: 'checkout', target, kind });
    if (res && kind === 'commit') toast('HEAD is detached. Create a branch to keep new commits.');
    return res;
  };

  async function deleteRemoteBranch(full) {
    const r = remoteOf(full);
    if (!r) return;
    const ok = await confirmDialog({
      title: `Delete ${full} on the remote?`,
      body: [h('p', {}, `The branch ${r.branch} is deleted on ${r.remote} for everyone who uses that remote.`),
        h('p', { class: 'muted' }, 'Your local branches are not changed. To bring the branch back, push it again from a repository that still has it.')],
      confirm: 'Delete on remote', danger: true,
    });
    if (ok) await net({ job: 'push-delete', remote: r.remote, refname: r.branch });
  }

  // ---------------------------------------------------------------- tags
  const createTag = (at = null, atLabel = null) => formModal({
    title: 'Create a tag', confirm: 'Create',
    intro: h('div', { class: 'muted small' }, atLabel ? `At ${atLabel}` : 'At the current commit (HEAD)'),
    fields: [{ key: 'name', label: 'Tag name', placeholder: 'v1.0.0' }, { key: 'message', label: 'Message (optional)', type: 'area', hint: 'With a message the tag is annotated: it records who created it and when.' }],
    submit: (v) => api(`/repos/${id}/op`, { body: { op: 'tag-create', name: v.name.trim(), at, message: v.message } }),
  });

  async function deleteTag(name) {
    const res = await op({ op: 'tag-delete', name });
    if (res) toast(`Deleted tag ${name}`, 'success', res.oid ? { label: 'Undo', run: () => op({ op: 'tag-restore', name, oid: res.oid }) } : null);
  }

  async function pushTag(name, remote, remove = false) {
    if (remove) {
      const ok = await confirmDialog({ title: `Delete tag ${name} on ${remote}?`, body: [h('p', {}, `The tag is deleted on ${remote} for everyone. Your local tag is not changed.`)], confirm: 'Delete on remote', danger: true });
      if (ok) await net({ job: 'push-delete', remote, refname: `refs/tags/${name}` });
    } else await net({ job: 'push', remote, refname: `refs/tags/${name}` });
  }

  // ---------------------------------------------------------------- stashes
  async function stashSave() {
    return formModal({
      title: 'Stash changes', confirm: 'Stash',
      intro: h('div', { class: 'muted small' }, 'Saves your uncommitted changes and cleans the working tree. Apply or pop the stash to get them back.'),
      fields: [{ key: 'message', label: 'Message (optional)' }, { key: 'untracked', label: 'Include untracked files', type: 'checkbox', value: true }, { key: 'keepIndex', label: 'Keep staged changes in the working tree', type: 'checkbox', value: false }],
      submit: (v) => api(`/repos/${id}/op`, { body: { op: 'stash-save', message: v.message, untracked: v.untracked, keepIndex: v.keepIndex } }),
    });
  }
  const stashApply = (index) => op({ op: 'stash-apply', index }).then((r) => r && toast('Stash applied', 'success'));
  const stashPop = (index) => op({ op: 'stash-pop', index }).then((r) => r && toast('Stash applied and removed', 'success'));
  async function stashDrop(index, message) {
    const ok = await confirmDialog({
      title: 'Drop this stash?',
      body: [h('p', {}, 'The stashed changes are removed from the stash list:'), h('pre', { class: 'confirm-list' }, message || `stash@{${index}}`),
        h('p', { class: 'muted' }, 'You can undo the drop from the notification. After that the changes are hard to find.')],
      confirm: 'Drop stash', danger: true,
    });
    if (!ok) return;
    const res = await op({ op: 'stash-drop', index });
    if (res) toast('Stash dropped', 'success', { label: 'Undo', run: () => op({ op: 'stash-store', oid: res.oid, message: res.message }) });
  }
  const stashIndex = (name) => Number((/\{(\d+)\}/.exec(name) || [])[1] ?? 0);

  // ---------------------------------------------------------------- remotes
  const editRemote = (r) => formModal({
    title: `Remote ${r.name}`, confirm: 'Save',
    fields: [{ key: 'url', label: 'URL', value: r.url || '' }],
    submit: (v) => api(`/repos/${id}/op`, { body: { op: 'remote-set-url', name: r.name, url: v.url.trim() } }),
  });
  const renameRemote = (r) => formModal({
    title: `Rename remote ${r.name}`, confirm: 'Rename',
    fields: [{ key: 'to', label: 'New name', value: r.name }],
    submit: (v) => api(`/repos/${id}/op`, { body: { op: 'remote-rename', name: r.name, to: v.to.trim() } }),
  });
  async function removeRemote(r) {
    const ok = await confirmDialog({
      title: `Remove the remote ${r.name}?`,
      body: [h('p', {}, `The remote and its ${plural(r.branches.length, 'remote-tracking branch')} are removed from this repository. Nothing changes on the server.`),
        h('p', { class: 'muted mono small' }, r.url || '')],
      confirm: 'Remove', danger: true,
    });
    if (ok) await op({ op: 'remote-remove', name: r.name });
  }

  // ---------------------------------------------------------------- menus
  // Merge and rebase started from a menu or a drag. `ctx.history` replaces them with the
  // versions that also handle conflicts and undo.
  async function basicMerge(branch, into) {
    const res = await op({ op: 'merge', branch, into });
    if (!res) return;
    if (res.stopped) toast(`The merge stopped because of conflicts. Resolve them, then commit.`, 'error');
    else toast(/Already up to date/.test(res.output) ? `${into} already contains ${branch}` : `Merged ${branch} into ${into}`, 'success');
  }
  async function basicRebase(branch, onto) {
    const res = await op({ op: 'rebase', branch, onto });
    if (!res) return;
    if (res.stopped) toast('The rebase stopped because of conflicts.', 'error');
    else toast(`Rebased ${branch} onto ${onto}`, 'success');
  }
  const hist = () => ({ merge: basicMerge, rebase: basicRebase, ...(ctx.history || {}) });
  const cur = () => current()?.name || null;

  function branchItems(name) {
    const b = refs()?.branches.find((x) => x.name === name);
    const here = cur();
    return [
      { title: name },
      { icon: 'check', label: 'Checkout', disabled: b?.current, run: () => checkout(name, 'branch') },
      here && !b?.current ? { icon: 'merge', label: `Merge into ${here}`, run: () => hist().merge?.(name, here) } : null,
      here && !b?.current ? { icon: 'branch', label: `Rebase ${here} onto this branch`, run: () => hist().rebase?.(here, name) } : null,
      '-',
      { icon: 'push', label: 'Push', run: () => push({ branch: name }) },
      { icon: 'push', label: 'Force push…', danger: true, run: () => push({ branch: name, force: true }) },
      '-',
      { icon: 'edit', label: 'Rename…', run: () => renameBranch(name) },
      { icon: 'trash', label: 'Delete…', danger: true, disabled: b?.current, tip: b?.current ? 'Check out another branch first' : null, run: () => deleteBranch(name) },
      { icon: 'copy', label: 'Copy name', run: () => copyText(name) },
    ];
  }

  function remoteBranchItems(full) {
    const here = cur();
    return [
      { title: full },
      { icon: 'check', label: 'Checkout as a local branch', run: () => checkout(full, 'remote') },
      here ? { icon: 'merge', label: `Merge into ${here}`, run: () => hist().merge?.(full, here) } : null,
      here ? { icon: 'branch', label: `Rebase ${here} onto this branch`, run: () => hist().rebase?.(here, full) } : null,
      '-',
      { icon: 'trash', label: 'Delete on the remote…', danger: true, run: () => deleteRemoteBranch(full) },
      { icon: 'copy', label: 'Copy name', run: () => copyText(full) },
    ];
  }

  function tagItems(name) {
    const remotes = refs()?.remotes || [];
    return [
      { title: name },
      { icon: 'check', label: 'Checkout (detached HEAD)', run: () => checkout(name, 'tag') },
      ...remotes.slice(0, 3).map((r) => ({ icon: 'push', label: `Push to ${r.name}`, run: () => pushTag(name, r.name) })),
      '-',
      { icon: 'trash', label: 'Delete tag', danger: true, run: () => deleteTag(name) },
      ...remotes.slice(0, 3).map((r) => ({ icon: 'trash', label: `Delete on ${r.name}…`, danger: true, run: () => pushTag(name, r.name, true) })),
      { icon: 'copy', label: 'Copy name', run: () => copyText(name) },
    ];
  }

  function stashItems(index, message) {
    return [
      { title: `stash@{${index}}` },
      { icon: 'download', label: 'Apply', sub: 'Keep the stash', run: () => stashApply(index) },
      { icon: 'download', label: 'Pop', sub: 'Apply and remove the stash', run: () => stashPop(index) },
      '-',
      { icon: 'trash', label: 'Drop…', danger: true, run: () => stashDrop(index, message) },
    ];
  }

  function remoteItems(r) {
    return [
      { title: r.name },
      { icon: 'fetch', label: `Fetch ${r.name}`, run: () => net({ job: 'fetch', remote: r.name }) },
      { icon: 'fetch', label: 'Fetch and prune', sub: 'Remove branches deleted on the remote', run: () => net({ job: 'fetch', remote: r.name, prune: true }) },
      '-',
      { icon: 'edit', label: 'Edit URL…', run: () => editRemote(r) },
      { icon: 'edit', label: 'Rename…', run: () => renameRemote(r) },
      { icon: 'trash', label: 'Remove…', danger: true, run: () => removeRemote(r) },
      r.url ? { icon: 'copy', label: 'Copy URL', run: () => copyText(r.url) } : null,
    ];
  }

  /** Menu items for a ref label: { name, kind }. */
  function refItems(ref, row) {
    if (ref.kind === 'branch') return branchItems(ref.name);
    if (ref.kind === 'remote') return remoteBranchItems(ref.name);
    if (ref.kind === 'tag') return tagItems(ref.name);
    if (ref.kind === 'stash') return stashItems(stashIndex(ref.name), row?.s);
    return [];
  }

  /** Menu of a graph row; `ref` is the label under the pointer, if any. */
  function commitMenu(e, row, ref) {
    if (row.k === 'wip') {
      contextMenu(e, [{ icon: 'stash', label: 'Stash changes…', run: stashSave }]);
      return;
    }
    if (row.k === 'stash') {
      const label = (row.r || []).find((l) => l.kind === 'stash');
      contextMenu(e, stashItems(stashIndex(label?.name || ''), row.s));
      return;
    }
    const short = row.o.slice(0, 7);
    const items = ref ? [...refItems(ref, row), '-', { title: `Commit ${short}` }] : [{ title: `Commit ${short}` }];
    items.push(
      { icon: 'check', label: 'Checkout this commit', sub: 'Detached HEAD', run: () => checkout(row.o, 'commit') },
      { icon: 'branch', label: 'Create a branch here…', run: () => createBranch(row.o, `${short} ${row.s}`) },
      { icon: 'tag', label: 'Create a tag here…', run: () => createTag(row.o, `${short} ${row.s}`) },
      ...(hist().commitItems?.(row) || []),
      '-',
      { icon: 'copy', label: 'Copy commit id', sub: row.o.slice(0, 10), run: () => copyText(row.o, 'Commit id copied') },
      { icon: 'copy', label: 'Copy message', run: () => copyText(row.s) },
    );
    contextMenu(e, items);
  }

  // ---------------------------------------------------------------- toolbar
  const toolbar = h('div', { class: 'tb-actions' });
  function drawToolbar() {
    const b = current();
    const busy = running.size > 0;
    const btn = (ic, label, title, run, badge = null, extra = '') => h('button', { class: `btn ghost act ${extra}`, title, disabled: busy, onclick: run }, icon(ic, 15), h('span', { class: 'lbl' }, label), badge ? h('span', { class: 'act-badge' }, badge) : null);
    fill(toolbar,
      btn('fetch', 'Fetch', 'Fetch all remotes', () => fetchAll(false)),
      h('span', { class: 'act-group' },
        btn('pull', 'Pull', b?.upstream ? `Pull ${b.upstream} into ${b.name}` : 'Pull the upstream branch', () => pull(), b?.behind ? `↓${b.behind}` : null),
        h('button', { class: 'btn ghost act caret', title: 'More ways to pull and fetch', disabled: busy, onclick: (e) => openMenu(e.currentTarget, menuItems([
          { icon: 'pull', label: 'Pull', sub: 'As set in your Git configuration', run: () => pull() },
          { icon: 'pull', label: 'Pull, fast-forward only', sub: 'Stops when the branches diverged', run: () => pull('ff-only') },
          { icon: 'merge', label: 'Pull with merge', run: () => pull('merge') },
          { icon: 'branch', label: 'Pull with rebase', run: () => pull('rebase') },
          '-',
          { icon: 'fetch', label: 'Fetch all and prune', sub: 'Remove branches deleted on the remotes', run: () => fetchAll(true) },
        ])) }, icon('chevron', 12))),
      h('span', { class: 'act-group' },
        btn('push', 'Push', b ? (b.upstream ? `Push ${b.name} to ${b.upstream}` : `Push ${b.name} and choose its upstream`) : 'Check out a branch to push', () => push(), b?.ahead ? `↑${b.ahead}` : null),
        h('button', { class: 'btn ghost act caret', title: 'More ways to push', disabled: busy, onclick: (e) => openMenu(e.currentTarget, menuItems([
          { icon: 'push', label: 'Push', run: () => push() },
          { icon: 'push', label: 'Force push…', sub: 'With --force-with-lease', danger: true, run: () => push({ force: true }) },
        ])) }, icon('chevron', 12))),
      h('span', { class: 'act-sep' }),
      btn('branch', 'Branch', 'Create a branch at HEAD', () => createBranch()),
      btn('stash', 'Stash', 'Stash the uncommitted changes', stashSave),
      btn('download', 'Pop', 'Apply the newest stash and remove it', () => stashPop(0), null, refs()?.stashes.length ? '' : 'off'));
    for (const el of toolbar.querySelectorAll('.off')) el.disabled = true;
  }

  // ---------------------------------------------------------------- drag a branch onto a branch
  /**
   * Elements with data-ref and data-kind "branch" or "remote" inside `root` can be dragged
   * onto each other. The drag uses pointer events: the desktop window handles native
   * drag and drop itself (for dropped folders).
   */
  function enableBranchDrag(root) {
    const SEL = '[data-ref][data-kind="branch"], [data-ref][data-kind="remote"]';
    root.addEventListener('mousedown', (e) => {
      const src = e.button === 0 ? e.target.closest?.(SEL) : null;
      if (!src) return;
      const from = { name: src.dataset.ref, kind: src.dataset.kind };
      const x0 = e.clientX, y0 = e.clientY;
      let ghost = null, target = null;
      const move = (ev) => {
        if (!ghost) {
          if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
          ghost = h('div', { class: 'drag-ghost' }, icon('branch', 12), from.name);
          document.body.append(ghost);
          document.body.classList.add('dragging-branch');
        }
        ghost.style.left = `${ev.clientX + 12}px`;
        ghost.style.top = `${ev.clientY + 12}px`;
        const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest?.(SEL);
        const next = over && over.dataset.ref !== from.name ? over : null;
        if (next !== target) {
          target?.classList.remove('drop-target');
          target = next;
          target?.classList.add('drop-target');
        }
      };
      const up = (ev) => {
        document.removeEventListener('mousemove', move);
        document.removeEventListener('mouseup', up);
        if (!ghost) return;
        ghost.remove();
        document.body.classList.remove('dragging-branch');
        target?.classList.remove('drop-target');
        // The click that ends a drag must not select a row.
        const swallow = (c) => { c.stopPropagation(); c.preventDefault(); };
        window.addEventListener('click', swallow, { capture: true, once: true });
        setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 50);
        if (!target) return;
        const to = { name: target.dataset.ref, kind: target.dataset.kind };
        const at = { getBoundingClientRect: () => ({ left: ev.clientX, bottom: ev.clientY - 4 }) };
        openMenu(at, menuItems([
          { title: `${from.name} → ${to.name}` },
          to.kind === 'branch' ? { icon: 'merge', label: `Merge ${from.name} into ${to.name}`, sub: to.name === cur() ? null : `Checks out ${to.name} first`, run: () => hist().merge?.(from.name, to.name) } : null,
          from.kind === 'branch' ? { icon: 'branch', label: `Rebase ${from.name} onto ${to.name}`, sub: from.name === cur() ? null : `Checks out ${from.name} first`, run: () => hist().rebase?.(from.name, to.name) } : null,
        ]));
      };
      document.addEventListener('mousemove', move);
      document.addEventListener('mouseup', up);
    });
  }

  drawToolbar();
  return {
    op, net, onJob, toolbar, statusEl, drawToolbar, enableBranchDrag, commitMenu,
    branchItems, remoteBranchItems, tagItems, stashItems, remoteItems,
    createBranch, createTag, stashSave, addRemote, push, pull, fetchAll, checkout,
  };
}

/** The clone dialog. `onDone(path)` runs when the clone succeeded. `jobEvents` lets the dialog follow its job. */
export function openCloneDialog({ desktop, defaultParent, jobEvents, onDone, rememberParent }) {
  const url = h('input', { class: 'input', placeholder: 'https://example.com/team/project.git', spellcheck: 'false' });
  const parent = h('input', { class: 'input', value: defaultParent || '', placeholder: 'Folder that receives the new repository', spellcheck: 'false' });
  const name = h('input', { class: 'input', placeholder: 'Folder name (from the URL)', spellcheck: 'false' });
  const status = h('div', { class: 'form-status' });
  const bar = h('div', { class: 'job-bar', hidden: true }, h('div', {}));
  let jobId = null;
  let off = null;
  const folderName = (u) => (u.trim().replace(/[\\/]+$/, '').split(/[\\/:]/).pop() || '').replace(/\.git$/, '');
  url.addEventListener('input', () => { name.placeholder = folderName(url.value) || 'Folder name (from the URL)'; });
  const browse = desktop ? h('button', { class: 'btn', type: 'button', onclick: async () => {
    try { const r = await api('/dialog/folder', { body: {} }); if (r.path) parent.value = r.path; } catch (e) { toast(e.message, 'error'); }
  } }, icon('folder', 14), 'Browse') : null;
  const go = h('button', { class: 'btn primary' }, 'Clone');
  const cancel = h('button', { class: 'btn', onclick: () => (jobId ? api(`/jobs/${jobId}`, { method: 'DELETE' }).catch(() => {}) : close()) }, 'Cancel');
  const start = async () => {
    go.disabled = true;
    status.className = 'form-status';
    status.textContent = 'Starting…';
    try {
      const res = await api('/clone', { body: { url: url.value, parent: parent.value, name: name.value } });
      jobId = res.job.id;
      rememberParent?.(parent.value.trim());
      bar.hidden = false;
      off = jobEvents(jobId, (job) => {
        status.textContent = job.line || job.title;
        bar.firstChild.style.width = `${job.percent ?? 0}%`;
        bar.firstChild.className = job.percent == null ? 'indeterminate' : '';
        if (job.state === 'running') return;
        off?.();
        jobId = null;
        bar.hidden = true;
        if (job.state === 'done') { close(); onDone(res.path); return; }
        status.className = 'form-status error';
        status.textContent = job.state === 'cancelled' ? 'Cancelled.' : `${job.command}\n${job.output || job.line}`;
        status.style.whiteSpace = 'pre-wrap';
        go.disabled = false;
      });
    } catch (e) {
      status.className = 'form-status error';
      status.textContent = e.message;
      go.disabled = false;
    }
  };
  go.addEventListener('click', start);
  const form = h('form', { class: 'form', onsubmit: (e) => { e.preventDefault(); start(); } },
    h('label', {}, 'Repository URL', url),
    h('label', {}, 'Clone into', h('div', { class: 'commit-row' }, parent, browse)),
    h('label', {}, 'Folder name', name),
    h('div', { class: 'hint' }, 'Sign-in uses your Git setup: the credential helper and the SSH agent. Graftline does not store passwords.'),
    bar, status);
  const close = openModal('Clone a repository', form, [cancel, go], { onClose: () => { off?.(); if (jobId) api(`/jobs/${jobId}`, { method: 'DELETE' }).catch(() => {}); } });
  queueMicrotask(() => url.focus());
}
