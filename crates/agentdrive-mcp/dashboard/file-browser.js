import { renderDocument } from './file-preview.js';

export function createFileBrowser({api, element, container, onNavigate}) {
  const body = element('section', 'branch-files-body');
  const toolbar = element('div', 'branch-files-toolbar');
  const refresh = element('button', 'secondary', '刷新文件'); refresh.type = 'button';
  const directory = element('nav', 'branch-directory'); directory.setAttribute('aria-label', '目录路径');
  toolbar.append(directory, refresh);
  const status = element('p', 'changes-status'); status.setAttribute('role', 'status');
  const layout = element('div', 'changes-layout');
  const sidebar = element('div', 'changed-files');
  const list = element('div', 'branch-file-list'); list.setAttribute('aria-label', '分支文件列表');
  const more = element('button', 'secondary', '加载更多文件'); more.type = 'button'; more.hidden = true;
  const filter = element('input', 'file-filter'); filter.type = 'search'; filter.placeholder = '筛选当前目录'; filter.setAttribute('aria-label', '筛选已载入的当前目录文件');
  const hiddenLabel = element('label', 'hidden-files-toggle');
  const hiddenFiles = element('input'); hiddenFiles.type = 'checkbox'; hiddenLabel.append(hiddenFiles, document.createTextNode('显示隐藏文件'));
  sidebar.append(filter, hiddenLabel, list, more);
  const preview = element('div', 'diff-preview branch-file-preview'); preview.setAttribute('aria-live', 'polite');
  layout.append(sidebar, preview); body.append(toolbar, status, layout); container.append(body);
  let context = null, path = '/', version = null, cursor = null, request = null, fileRequest = null, selected = null, changed = false, entries = [];
  let previewPath = null, previewMode = 'reading', loadedPages = 0, restoreSelection = null;
  function clear() {
    context = null; entries = []; selected = null; request?.abort(); fileRequest?.abort();
    previewPath = null; previewMode = 'reading'; loadedPages = 0; restoreSelection = null;
    list.replaceChildren(); preview.replaceChildren(); status.replaceChildren(); directory.replaceChildren();
    more.hidden = true;
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
    if (previewPath !== entry.path) previewMode = 'reading';
    previewPath = entry.path;
    button = [...list.children].find(node => node.dataset.path === entry.path) || button;
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
        element('p', 'diff-metadata', `${inode.size.toLocaleString()} 字节 · ${entry.dirty ? '当前工作内容' : '本机已保存'}`));
      const range = inode.size === 0 ? '空文件（0 字节）' : `字节 ${offset + 1}–${offset + consumed} / ${inode.size}`;
      if (text === null || !final || offset > 0 || inode.size === 0) preview.append(element('p', 'changes-status', `${range}${text === null ? ' · 二进制或非 UTF-8 内容，显示十六进制' : ''}`));
      const source = element('pre', 'branch-file-content', text === null ? hex(bytes, offset) : text);
      if (text !== null && /\.(md|markdown)$/i.test(entry.path)) {
        const document = renderDocument(text, element);
        const switcher = element('div', 'preview-switcher');
        const reading = element('button', 'preview-mode active', '阅读'); reading.type = 'button'; reading.setAttribute('aria-pressed','true');
        const raw = element('button', 'preview-mode', '源码'); raw.type = 'button'; raw.setAttribute('aria-pressed','false');
        const toggle = value => { previewMode = value ? 'reading' : 'source'; document.hidden = !value; source.hidden = value; reading.classList.toggle('active',value); raw.classList.toggle('active',!value); reading.setAttribute('aria-pressed',String(value)); raw.setAttribute('aria-pressed',String(!value)); };
        reading.onclick = () => toggle(true); raw.onclick = () => toggle(false); switcher.append(reading, raw);
        toggle(previewMode === 'reading'); preview.append(switcher, document);
      }
      preview.append(source);
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
  function renderEntries() {
    const previousPath = selected?.dataset.path || restoreSelection?.path;
    const filterText = filter.value.toLocaleLowerCase();
    const visible = entries.filter(entry => (hiddenFiles.checked || !entry.path.split('/').at(-1).startsWith('.')) && entry.path.split('/').at(-1).toLocaleLowerCase().includes(filterText))
      .sort((a,b) => Number(b.inode.kind === 'directory') - Number(a.inode.kind === 'directory') || a.path.localeCompare(b.path));
    list.replaceChildren(); selected = null;
    const files = [];
    for (const entry of visible) {
      const isDirectory = entry.inode.kind === 'directory';
      const button = element('button', 'changed-file branch-file'); button.type = 'button'; button.setAttribute('aria-pressed','false'); button.dataset.path = entry.path;
      const icon = element('span', `file-symbol ${isDirectory ? 'folder' : 'document'}`, isDirectory ? '▰' : /\.(md|txt|markdown)$/i.test(entry.path) ? 'Aa' : /\.(csv|json)$/i.test(entry.path) ? '{}' : '≡'); icon.setAttribute('aria-hidden','true');
      button.append(icon, element('span', 'changed-path', entry.path.split('/').at(-1)));
      button.title = isDirectory ? '打开目录' : `${entry.inode.size.toLocaleString()} 字节`;
      button.onclick = isDirectory ? () => navigate(entry.path) : () => view(entry, button);
      list.append(button);
      if (!isDirectory) files.push({entry,button});
      if (!isDirectory && entry.path === previousPath) { selected = button; button.classList.add('active'); button.setAttribute('aria-pressed','true'); }
    }
    status.textContent = changed ? '分支内容已更新，可点击“刷新文件”读取最新内容。' : `已载入 ${entries.length} 项 · 显示 ${visible.length} 项${cursor ? ' · 还有更多文件' : ''}`;
    if (restoreSelection && cursor && loadedPages < restoreSelection.pages && !selected) return;
    if (selected && restoreSelection) view(files.find(({entry}) => entry.path === previousPath).entry, selected);
    else if (!selected && files.length) { const first = files.find(({entry}) => /\/readme\.md$/i.test(entry.path)) || files.find(({entry}) => /\.(md|txt|json|csv|py)$/i.test(entry.path)) || files[0]; view(first.entry, first.button); }
    else if (!selected) {
      fileRequest?.abort();
      preview.replaceChildren(element('p', 'changes-status', entries.length ? visible.length ? '选择目录浏览其中的文件。' : '没有符合筛选条件的文件，可调整筛选或显示隐藏文件。' : '此目录为空。'));
    }
  }
  async function page(append = false) {
    request?.abort(); request = new AbortController(); const current = request;
    more.disabled = true;
    try {
      for (;;) {
        const query = new URLSearchParams({path}); if (version) query.set('version', version); if (append && cursor) query.set('after', cursor);
        const data = await api(`${base()}/files?${query}`, {signal:current.signal});
        if (current.signal.aborted) return;
        version = data.version; cursor = data.next; more.hidden = !cursor;
        entries.push(...data.entries); loadedPages++;
        // Restore only previously loaded pages, keeping refresh work bounded.
        if (restoreSelection && cursor && loadedPages < restoreSelection.pages && !entries.some(entry => entry.path === restoreSelection.path)) { append = true; continue; }
        renderEntries(); restoreSelection = null; break;
      }
    } catch (error) { if (!current.signal.aborted) failure(status, error, () => page(append)); }
    finally { if (!current.signal.aborted) more.disabled = false; }
  }
  function renderDirectory() {
    const focused = directory.contains(document.activeElement) ? document.activeElement.dataset.path : null;
    directory.replaceChildren();
    const segment = (name, destination) => {
      const button = element('button', 'directory-segment', name); button.type = 'button';
      button.dataset.path = destination; button.title = destination;
      if (destination === '/') button.setAttribute('aria-label', '根目录');
      if (destination === path) button.setAttribute('aria-current', 'location');
      button.onclick = () => navigate(destination);
      directory.append(button);
      if (focused === destination) button.focus({preventScroll:true});
    };
    segment('/', '/');
    let destination = '';
    path.split('/').filter(Boolean).forEach((name, index) => {
      if (index) { const separator = element('span', 'directory-separator', '/'); separator.setAttribute('aria-hidden', 'true'); directory.append(separator); }
      destination += '/' + name; segment(name, destination);
    });
  }
  function navigate(destination, latest = false) {
    restoreSelection = latest && destination === path && previewPath ? restoreSelection || {path:previewPath, pages:loadedPages} : null;
    if (!restoreSelection) { previewPath = null; previewMode = 'reading'; filter.value = ''; }
    request?.abort(); fileRequest?.abort(); path = destination; cursor = null; selected = null; entries = []; loadedPages = 0;
    if (latest) { version = null; changed = false; }
    renderDirectory(); onNavigate?.(path);
    list.replaceChildren(); preview.replaceChildren(); more.hidden = true; status.textContent = '正在读取目录…'; page();
  }
  function show(workspace, branch, destination = '/') {
    clear(); context = {workspace, branch}; navigate(destination, true);
  }
  function update(workspace, branches) {
    if (!context) return;
    const branch = branches.find(value => value.id === context.branch.id && value.state !== 'deleted');
    if (workspace !== context.workspace || !branch) { clear(); return; }
    const expected = `${branch.id}:${branch.authority_epoch}:${branch.generation}:${branch.mutation_seq}`;
    if (version && expected !== version) { changed = true; status.textContent = '分支内容已更新，可点击“刷新文件”读取最新内容。'; }
  }
  filter.oninput = renderEntries; hiddenFiles.onchange = renderEntries;
  refresh.onclick = () => navigate(path, true); more.onclick = () => page(true);
  return {show, update, clear, refresh:() => context && navigate(path, true)};
}
