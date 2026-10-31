export function createWorkspaceView({element, selectBranch, showRevision, showBranchFiles}) {
  const short = id => id?.slice(0, 8) || '—';
  const kinds = {commit:'Commit', autosave:'自动保存', checkpoint:'检查点', candidate:'合并候选'};

  function render(snapshot, selectedBranch) {
    const {status, revisions, origins} = snapshot;
    const branches = new Map(snapshot.branches.map(branch => [branch.id, branch]));
    const revisionById = new Map(revisions.map(revision => [revision.id, revision]));
    const originById = new Map(origins.map(origin => [origin.branch, origin]));
    const mounted = new Map();
    for (const mount of status.mounts) {
      if (mount.state === 'ready') mounted.set(mount.branch, (mounted.get(mount.branch) || 0) + 1);
    }
    const visible = new Set();
    for (const branch of snapshot.branches.filter(branch => branch.state !== 'deleted')) {
      let id = branch.id;
      while (id && !visible.has(id) && branches.has(id)) {
        visible.add(id); id = originById.get(id)?.source_branch;
      }
    }
    const children = new Map();
    for (const id of visible) {
      const source = originById.get(id)?.source_branch;
      const parent = visible.has(source) && source !== id ? source : null;
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(branches.get(id));
    }
    const tree = document.getElementById('branch-tree'); tree.replaceChildren();
    const seen = new Set();
    function sourceNode(origin, id) {
      const revision = revisionById.get(origin.revision);
      const node = element('div', 'origin-node');
      const dot = element('span', 'origin-dot'); dot.setAttribute('aria-hidden', 'true');
      const copy = element('div', 'origin-copy');
      const button = element('button', 'origin-revision', `${kinds[revision?.kind] || '版本'} ${short(origin.revision)}`);
      button.id = id; button.type = 'button'; button.title = origin.revision;
      button.disabled = !origin.revision_available || !revision;
      button.onclick = () => showRevision(revision, id);
      copy.append(button, element('span', 'origin-message', revision ? revision.message || '从此版本创建分支' : '来源版本记录不可用'));
      node.append(dot, copy); return node;
    }
    function group(items) {
      const list = element('ul', 'branch-group');
      for (const branch of [...items].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id))) {
        if (seen.has(branch.id)) continue;
        seen.add(branch.id);
        const item = element('li', 'branch-item');
        const origin = originById.get(branch.id);
        if (origin && !visible.has(origin.source_branch)) {
          const label = origin.workspace_root ? '工作区初始版本' : origin.source_branch ? `来源分支 ${short(origin.source_branch)} · 记录不可用` : '来源版本';
          item.append(element('div', 'origin-caption', label), sourceNode(origin, `origin-root-${branch.id}`));
        }
        const button = element('button', `branch-button${branch.id === selectedBranch ? ' selected' : ''}`);
        button.id = `branch-${branch.id}`; button.type = 'button'; button.setAttribute('aria-pressed', String(branch.id === selectedBranch));
        button.disabled = branch.state === 'deleted'; button.title = `${branch.name} · ${branch.local ? '双击预览当前文件' : '双击查看最近保存的版本'}`;
        button.setAttribute('aria-haspopup', 'dialog'); button.setAttribute('aria-controls', branch.local ? 'branch-files-dialog' : 'revision-dialog');
        const symbol = element('span', 'branch-symbol', '⑂'); symbol.setAttribute('aria-hidden', 'true');
        const copy = element('span', 'branch-copy');
        copy.append(element('span', 'branch-name', branch.name));
        const badges = element('span', 'branch-badges');
        const location = element('span', 'default-mark', branch.owner === status.location ? '本机' : `其他节点 ${short(branch.owner)}`);
        location.title = `location：${branch.owner}`; badges.append(location);
        if (branch.id === status.selected_branch) {
          const badge = element('span', 'default-mark', '本机默认'); badge.title = '未指定分支时的本机选择'; badges.append(badge);
        }
        if (mounted.has(branch.id)) badges.append(element('span', 'mount-mark', `已挂载 ${mounted.get(branch.id)}`));
        if (branch.state === 'deleted') badges.append(element('span', 'branch-unavailable', '已删除'));
        button.append(symbol, copy, badges); button.onclick = event => { selectBranch(branch.id); if (event.detail === 0) showBranchFiles(branch); };
        button.ondblclick = () => showBranchFiles(branch); item.append(button);
        const forks = new Map();
        for (const child of children.get(branch.id) || []) {
          const base = originById.get(child.id).revision;
          if (!forks.has(base)) forks.set(base, []);
          forks.get(base).push(child);
        }
        if (forks.size) {
          const timeline = element('ul', 'origin-timeline');
          const ordered = [...forks].sort(([a], [b]) => (revisionById.get(a)?.created_ns || 0) - (revisionById.get(b)?.created_ns || 0) || a.localeCompare(b));
          for (const [base, descendants] of ordered) {
            const fork = element('li', 'origin-fork');
            fork.append(sourceNode(originById.get(descendants[0].id), `origin-${branch.id}-${base}`), group(descendants));
            timeline.append(fork);
          }
          item.append(timeline);
        }
        list.append(item);
      }
      return list;
    }
    tree.append(group(children.get(null) || []));
    const remaining = [...visible].filter(id => !seen.has(id)).map(id => branches.get(id));
    if (remaining.length) tree.append(group(remaining));
    if (!visible.size) tree.append(element('p', 'panel-empty', '暂无工作分支。'));

  }
  function select(id) {
    for (const button of document.querySelectorAll('.branch-button')) {
      const active = button.id === `branch-${id}`;
      button.classList.toggle('selected', active); button.setAttribute('aria-pressed', String(active));
    }
  }
  return {render, select};
}
