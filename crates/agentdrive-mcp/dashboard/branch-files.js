export function createBranchFileViewer({api, element}) {
  const dialog = element('dialog', 'revision-dialog branch-files-dialog');
  dialog.id = 'branch-files-dialog'; dialog.setAttribute('aria-labelledby', 'branch-files-title');
  const heading = element('div', 'revision-dialog-heading');
  const title = element('h2', '', '分支文件'); title.id = 'branch-files-title';
  const subtitle = element('p', 'revision-dialog-subtitle');
  const copy = element('div'); copy.append(title, subtitle);
  const close = element('button', 'revision-dialog-close', '×'); close.type = 'button';
  close.autofocus = true; close.setAttribute('aria-label', '关闭分支文件'); heading.append(copy, close);
  const body = element('section', 'branch-files-body');
  const toolbar = element('div', 'branch-files-toolbar');
  const root = element('button', 'secondary', '根目录'); root.type = 'button';
  const up = element('button', 'secondary', '上级目录'); up.type = 'button';
  const refresh = element('button', 'secondary', '刷新文件'); refresh.type = 'button';
  const directory = element('code', 'branch-directory');
  toolbar.append(root, up, directory, refresh);
  const status = element('p', 'changes-status'); status.setAttribute('role', 'status');
  const layout = element('div', 'changes-layout');
  const sidebar = element('div', 'changed-files');
  const list = element('div', 'branch-file-list'); list.setAttribute('aria-label', '分支文件列表');
  const more = element('button', 'secondary', '加载更多文件'); more.type = 'button'; more.hidden = true;
  sidebar.append(list, more);
  const preview = element('div', 'diff-preview branch-file-preview'); preview.setAttribute('aria-live', 'polite');
  layout.append(sidebar, preview); body.append(toolbar, status, layout); dialog.append(heading, body); document.body.append(dialog);
  let context = null, path = '/', version = null, cursor = null, request = null, fileRequest = null, selected = null, changed = false;
  function clear(restoreFocus = false) {
    const trigger = context && document.getElementById(`branch-${context.branch.id}`);
    context = null; request?.abort(); fileRequest?.abort();
    list.replaceChildren(); preview.replaceChildren();
    if (dialog.open) dialog.close();
    if (restoreFocus) trigger?.focus();
  }
  function base() { return `/api/workspaces/${encodeURIComponent(context.workspace)}/branches/${encodeURIComponent(context.branch.id)}`; }
  function failure(target, error, retry) {
    if (error.name === 'AbortError') return;
    const message = error.status === 409 ? '分支内容已更新，请点击“刷新文件”重新读取。' : error.message;
    target.replaceChildren(element('p', 'changes-error', message));
    const button = element('button', 'secondary', error.status === 409 ? '刷新文件' : '重新加载'); button.type = 'button';
    button.onclick = error.status === 409 ? () => navigate(path, true) : retry; target.append(button);
  }
  function hex(bytes, offset) {
    const lines = [];
    for (let i = 0; i < bytes.length; i += 16) {
      const chunk = bytes.subarray(i, i + 16);
      lines.push((offset+i).toString(16).padStart(8, '0') + '  ' + Array.from(chunk, value => value.toString(16).padStart(2, '0')).join(' '));
    }
    return lines.join('\n');
  }
  async function view(entry, button, offsets = [0]) {
    selected?.classList.remove('active'); selected?.setAttribute('aria-pressed', 'false');
    selected = button; button.classList.add('active'); button.setAttribute('aria-pressed', 'true');
    fileRequest?.abort(); fileRequest = new AbortController(); const current = fileRequest;
    const offset = offsets.at(-1);
    preview.replaceChildren(element('p', 'changes-status', '正在读取文件…'));
    try {
      const query = new URLSearchParams({path:entry.path, version, offset, size:65536});
      const bytes = new Uint8Array(await api(`${base()}/file?${query}`, {signal:current.signal, binary:true}));
      if (current.signal.aborted) return;
      const inode = entry.inode;
      const final = offset + bytes.length >= inode.size;
      let text = null, consumed = bytes.length;
      if (!bytes.includes(0)) {
        for (let trim = 0; trim <= (final ? 0 : 3) && trim < bytes.length + 1; trim++) {
          try { text = new TextDecoder('utf-8', {fatal:true}).decode(bytes.subarray(0, bytes.length-trim)); consumed -= trim; break; }
          catch { /* Try a complete UTF-8 character boundary at the end of this chunk. */ }
        }
      }
      preview.replaceChildren(element('h3', 'branch-file-path', entry.path),
        element('p', 'diff-metadata', `${inode.size} 字节 · 权限 ${inode.mode.toString(8)} · ${entry.dirty ? '当前工作内容' : '已保存内容'}`));
      const range = inode.size === 0 ? '空文件（0 字节）' : `字节 ${offset + 1}–${offset + consumed} / ${inode.size}`;
      preview.append(element('p', 'changes-status', `${range}${text === null ? ' · 二进制或非 UTF-8 内容，显示十六进制' : ' · UTF-8 文本'}`));
      preview.append(element('pre', 'branch-file-content', text === null ? hex(bytes, offset) : text));
      const navigation = element('div', 'branch-file-navigation');
      if (offsets.length > 1) {
        const previous = element('button', 'secondary', '上一段'); previous.type = 'button'; previous.onclick = () => view(entry, button, offsets.slice(0,-1)); navigation.append(previous);
      }
      if (consumed > 0 && offset + consumed < inode.size) {
        const next = element('button', 'secondary', '下一段'); next.type = 'button'; next.onclick = () => view(entry, button, [...offsets, offset + consumed]); navigation.append(next);
      }
      preview.append(navigation);
    } catch (error) { if (!current.signal.aborted) failure(preview, error, () => view(entry, button, offsets)); }
  }
  async function page(append = false) {
    request?.abort(); request = new AbortController(); const current = request;
    more.disabled = true;
    try {
      const query = new URLSearchParams({path}); if (version) query.set('version', version); if (append && cursor) query.set('after', cursor);
      const data = await api(`${base()}/files?${query}`, {signal:current.signal});
      if (current.signal.aborted) return;
      version = data.version; cursor = data.next; more.hidden = !cursor;
      const files = [];
      for (const entry of data.entries) {
        const isDirectory = entry.inode.kind === 'directory';
        const button = element('button', 'changed-file branch-file'); button.type = 'button'; button.setAttribute('aria-pressed','false');
        button.dataset.path = entry.path;
        button.append(element('span', 'branch-file-kind', isDirectory ? '目录' : '文件'), element('span', 'changed-path', entry.path.split('/').at(-1)));
        button.onclick = isDirectory ? () => navigate(entry.path) : () => view(entry, button);
        list.append(button); if (!isDirectory) files.push({entry,button});
      }
      status.textContent = changed ? '分支内容已更新，可点击“刷新文件”读取最新内容。' : `已显示 ${list.children.length} 项${cursor ? '，还有更多文件' : ''} · 当前工作目录`;
      if (!selected && files.length) {
        const first = files.find(({entry}) => !entry.path.split('/').at(-1).startsWith('._')) || files[0];
        view(first.entry, first.button);
      } else if (!selected) preview.replaceChildren(element('p', 'changes-status', list.children.length ? '选择目录浏览其中的文件。' : '此目录为空。'));
    } catch (error) { if (!current.signal.aborted) failure(status, error, () => page(append)); }
    finally { if (!current.signal.aborted) more.disabled = false; }
  }
  function navigate(destination, latest = false) {
    request?.abort(); fileRequest?.abort(); path = destination; cursor = null; selected = null;
    if (latest) { version = null; changed = false; }
    directory.textContent = path; up.disabled = path === '/';
    list.replaceChildren(); preview.replaceChildren(); more.hidden = true; status.textContent = '正在读取目录…'; page();
  }
  function show(workspace, branch) {
    clear(); context = {workspace, branch}; subtitle.textContent = `${branch.name} · 当前文件`;
    dialog.showModal(); dialog.scrollTop = 0; navigate('/', true);
  }
  function update(workspace, branches) {
    if (!context) return;
    const branch = branches.find(value => value.id === context.branch.id && value.state !== 'deleted');
    if (workspace !== context.workspace || !branch) { clear(); return; }
    subtitle.textContent = `${branch.name} · 当前文件`;
    const expected = `${branch.id}:${branch.authority_epoch}:${branch.generation}:${branch.mutation_seq}`;
    if (version && expected !== version) { changed = true; status.textContent = '分支内容已更新，可点击“刷新文件”读取最新内容。'; }
  }
  root.onclick = () => navigate('/'); up.onclick = () => navigate(path.slice(0,path.lastIndexOf('/')) || '/');
  refresh.onclick = () => navigate(path, true); more.onclick = () => page(true); close.onclick = () => clear(true);
  dialog.addEventListener('cancel', event => { event.preventDefault(); clear(true); });
  let outside = false;
  const isOutside = event => {
    const rect = dialog.getBoundingClientRect();
    return event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom);
  };
  dialog.addEventListener('pointerdown', event => { outside = isOutside(event); });
  dialog.addEventListener('click', event => { if (outside && isOutside(event)) clear(true); outside = false; });
  return {show, update, clear};
}
