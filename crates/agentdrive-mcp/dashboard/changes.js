export function createRevisionChanges({api, element, container}) {
  const labels = {added:'新增', deleted:'删除', modified:'修改', metadata:'属性', renamed:'重命名'};
  const messages = {
    unchanged:'文件内容相同，可查看上方的路径和属性变化。',
    binary:'二进制文件，显示文件大小和内容标识。',
    too_large:'文件超过文本预览上限（256 KiB 或 10,000 行）。',
    directory:'目录变化。',
  };
  let key = null, controller = null, fileController = null;
  function clear() {
    key = null; controller?.abort(); fileController?.abort();
    container.replaceChildren(); container.hidden = true;
  }
  function show(workspace, revision) {
    if (!revision) { clear(); return; }
    const nextKey = `${workspace}/${revision.id}`;
    if (key === nextKey) return;
    clear(); key = nextKey; container.hidden = false;
    const heading = element('div', 'changes-heading');
    heading.append(element('h3', '', '修改内容'));
    const label = element('label', 'compare-label', '比较基准 ');
    const select = element('select', 'compare-parent');
    const parents = revision.parents.length ? revision.parents : [null];
    parents.forEach((id, index) => {
      const option = element('option', '', id ? `父版本 ${index + 1} · ${id.slice(0, 8)}` : '空目录（初始版本）');
      option.value = id || ''; select.append(option);
    });
    label.append(select); heading.append(label); container.append(heading);
    const body = element('div', 'changes-body'); container.append(body);
    const baseUrl = `/api/workspaces/${encodeURIComponent(workspace)}/revisions/${encodeURIComponent(revision.id)}`;

    function loadComparison() {
      controller?.abort(); fileController?.abort(); controller = new AbortController();
      const current = controller;
      const before = select.value;
      body.replaceChildren();
      const status = element('p', 'changes-status', '正在读取修改文件…'); status.setAttribute('role', 'status');
      const layout = element('div', 'changes-layout');
      const sidebar = element('div', 'changed-files');
      const list = element('div', 'changed-file-list'); list.setAttribute('aria-label', '修改文件');
      const more = element('button', 'secondary', '加载更多'); more.type = 'button'; more.hidden = true;
      sidebar.append(list, more);
      const preview = element('div', 'diff-preview'); preview.setAttribute('aria-live', 'polite');
      preview.append(element('p', 'changes-status', '选择文件查看修改内容。'));
      layout.append(sidebar, preview); body.append(status, layout);
      let cursor = null, count = 0, selected = null, loading = false;
      function query(values = {}) {
        const params = new URLSearchParams(values); if (before) params.set('before', before); return params;
      }
      function failure(target, error, retry) {
        if (error.name === 'AbortError' || current.signal.aborted) return;
        target.replaceChildren(element('p', 'changes-error', error.message));
        const button = element('button', 'secondary', '重新加载'); button.type = 'button'; button.onclick = retry; target.append(button);
      }
      async function view(entry, button) {
        selected?.classList.remove('active'); selected?.setAttribute('aria-pressed', 'false');
        selected = button; button.classList.add('active'); button.setAttribute('aria-pressed', 'true');
        fileController?.abort(); fileController = new AbortController(); const fileRequest = fileController;
        preview.replaceChildren(element('p', 'changes-status', '正在读取文本差异…'));
        try {
          const params = query({path:entry.path}); if (entry.previous_path) params.set('previous_path', entry.previous_path);
          const diff = await api(`${baseUrl}/diff?${params}`, {signal:fileRequest.signal});
          if (current.signal.aborted || fileRequest.signal.aborted) return;
          preview.replaceChildren(element('h4', 'diff-path', entry.previous_path ? `${entry.previous_path} → ${entry.path}` : entry.path));
          const metadata = element('dl', 'diff-metadata');
          function describe(inode) {
            if (!inode) return '不存在';
            return `${inode.kind === 'directory' ? '目录' : '文件'} · ${inode.size} 字节 · 权限 ${inode.mode.toString(8)} · uid/gid ${inode.uid}/${inode.gid}${inode.content ? ` · ${inode.content.id.slice(0, 12)}` : ''}`;
          }
          metadata.append(element('dt', '', '修改前'), element('dd', '', describe(diff.before)), element('dt', '', '修改后'), element('dd', '', describe(diff.after)));
          preview.append(metadata);
          if (diff.status !== 'text') { preview.append(element('p', 'changes-status', messages[diff.status] || '暂无文本差异。')); return; }
          const lines = element('div', 'diff-lines');
          for (const hunk of diff.hunks) {
            const first = hunk[0];
            lines.append(element('div', 'diff-hunk', `@@ 修改前 ${first?.before_line ?? '—'} · 修改后 ${first?.after_line ?? '—'} @@`));
            for (const line of hunk) {
              const row = element('div', `diff-line ${line.kind}`);
              row.append(element('span', 'line-number', line.before_line ?? ''), element('span', 'line-number', line.after_line ?? ''),
                element('span', 'line-sign', line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '), element('code', 'line-text', line.text));
              lines.append(row);
              if (!line.newline) lines.append(element('div', 'missing-newline', '文件末尾没有换行符'));
            }
          }
          preview.append(lines);
        } catch (error) { if (!fileRequest.signal.aborted) failure(preview, error, () => view(entry, button)); }
      }
      async function page() {
        if (loading) return;
        loading = true; more.disabled = true;
        try {
          const params = query({limit:'100'}); if (cursor) params.set('cursor', cursor);
          const data = await api(`${baseUrl}/changes?${params}`, {signal:current.signal});
          if (current.signal.aborted) return;
          for (const entry of data.entries) {
            const button = element('button', 'changed-file'); button.type = 'button'; button.setAttribute('aria-pressed', 'false');
            button.append(element('span', `change-kind ${entry.kind}`, labels[entry.kind]), element('span', 'changed-path', entry.path));
            if (entry.previous_path) button.title = `${entry.previous_path} → ${entry.path}`;
            button.onclick = () => view(entry, button); list.append(button);
          }
          count += data.entries.length; cursor = data.next; more.hidden = !cursor;
          status.replaceChildren(); status.textContent = data.total ? `共 ${data.total} 项修改，已显示 ${count} 项` : '此版本没有文件内容或属性修改。';
        } catch (error) { failure(status, error, page); }
        finally { loading = false; if (!current.signal.aborted) more.disabled = false; }
      }
      more.onclick = page; page();
    }
    select.onchange = loadComparison; loadComparison();
  }
  return {show, clear};
}
