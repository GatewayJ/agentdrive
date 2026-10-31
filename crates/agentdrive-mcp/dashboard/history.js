const labels = {commit:'Commit', autosave:'自动保存', checkpoint:'检查点', candidate:'合并候选'};
const title = revision => revision.message || labels[revision.kind] || '保存工作内容';
const endpoint = (workspace, branch) => `/api/workspaces/${encodeURIComponent(workspace)}/branches/${encodeURIComponent(branch)}/history`;

export function createHistoryList({api, element, list, count, viewer}) {
  const notice = element('button', 'secondary', '有新版本，查看更新'); notice.type = 'button'; notice.hidden = true;
  const feedback = element('p', 'history-hint'); feedback.setAttribute('role', 'status');
  const more = element('button', 'secondary', '加载更多版本'); more.type = 'button'; more.hidden = true;
  list.before(notice); list.after(feedback, more);
  let context = null, entries = [], next = null, total = 0, generation = 0, controller = null, busy = false, version = null;
  const signature = branch => JSON.stringify([branch.formal_head, branch.autosave_head]);
  function clear() {
    generation++; controller?.abort(); context = null; entries = []; next = null; version = null; busy = false;
    list.replaceChildren(); count.textContent = '0'; feedback.textContent = ''; notice.hidden = true; more.hidden = true;
  }
  function render() {
    if (!context) return;
    const {snapshot, branch} = context;
    const rows = new Map([...list.children].map(row => [row.dataset.revision, row]));
    if (entries.length) list.querySelector('.no-revisions')?.remove();
    for (const revision of entries) {
      const isHead = revision.id === branch.formal_head;
      let row = rows.get(revision.id);
      if (!row) { row = element('button', 'revision-row'); row.dataset.revision = revision.id; list.append(row); }
      const stamp = JSON.stringify([revision, isHead]);
      if (row.dataset.version === stamp) continue;
      row.dataset.version = stamp; row.replaceChildren(element('span', 'revision-node'));
      const copy = element('span', 'revision-copy'); copy.append(element('span', 'revision-title', title(revision)));
      const sub = element('span', 'revision-sub'); sub.append(element('code', '', revision.id.slice(0,8)), element('span', '', '·'), element('span', '', revision.actor));
      if (isHead) sub.append(element('span', '', 'HEAD'));
      copy.append(sub); row.append(copy);
      const time = element('span', 'revision-time', new Date(revision.created_ns / 1e6).toLocaleString('zh-CN', {month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}));
      time.append(element('span', 'revision-type', labels[revision.kind] || revision.kind)); row.append(time);
      viewer.bind(row, {workspace:snapshot.status.workspace.id, revision, message:title(revision), sourceBranch:branch.name, isHead});
    }
    if (!entries.length && !busy && !list.children.length) list.append(element('div', 'no-revisions', '此分支尚未生成新版本。'));
    count.textContent = total;
    more.hidden = !next; more.disabled = busy;
    viewer.update(snapshot.status.workspace.id, [...snapshot.revisions, ...entries], snapshot.branches);
  }
  async function load(append = false) {
    if (!context || busy) return;
    const request = ++generation, active = context, observedVersion = signature(active.branch);
    controller?.abort(); controller = new AbortController(); busy = true; more.disabled = true; notice.disabled = true;
    const params = new URLSearchParams({limit:'50'});
    if (append && next) params.set('cursor', next);
    feedback.textContent = '正在读取版本…';
    try {
      const page = await api(`${endpoint(active.snapshot.status.workspace.id, active.branch.id)}?${params}`, {signal:controller.signal});
      if (request !== generation) return;
      if (!append) { entries = []; list.replaceChildren(); version = observedVersion; }
      const known = new Set(entries.map(entry => entry.id));
      entries.push(...page.entries.filter(entry => !known.has(entry.id)));
      next = page.next; total = page.total;
      notice.hidden = signature(context.branch) === version;
      notice.textContent = '有新版本，查看更新';
      feedback.textContent = next ? `已显示 ${entries.length} / ${total} 个版本` : '';
    } catch (error) {
      if (request !== generation || error.name === 'AbortError') return;
      feedback.textContent = `版本读取失败：${error.message}`;
      notice.hidden = false; notice.textContent = '重新加载版本';
    } finally {
      if (request === generation) { busy = false; notice.disabled = false; render(); }
    }
  }
  async function show(snapshot, branch) {
    if (!branch) { clear(); return; }
    const changed = context?.snapshot.status.workspace.id !== snapshot.status.workspace.id || context?.branch.id !== branch.id;
    if (changed) clear();
    context = {snapshot, branch};
    if (changed) await load();
    else { if (version && signature(branch) !== version) notice.hidden = false; render(); }
  }
  more.onclick = () => load(true); notice.onclick = () => load();
  return {show, clear};
}

export function createRevisionSource({api, element, select, form, changed}) {
  const group = element('div', 'revision-source-search');
  const label = element('label', 'management-field', '查找保存版本');
  const search = element('input'); search.type = 'search'; search.placeholder = '版本 ID 或说明'; search.maxLength = 256; search.setAttribute('aria-label', '查找保存版本');
  label.append(search);
  const more = element('button', 'secondary', '加载更多来源版本'); more.type = 'button'; more.hidden = true;
  const feedback = element('p', 'management-hint'); feedback.setAttribute('role', 'status');
  group.append(label, more, feedback); select.closest('label').after(group);
  let context = null, entries = [], next = null, generation = 0, controller = null, timer = null, busy = false, version = null;
  function clear() {
    generation++; controller?.abort(); clearTimeout(timer); context = null; entries = []; next = null; busy = false; version = null;
    search.value = ''; select.replaceChildren(); delete select.dataset.options; feedback.textContent = ''; more.hidden = true;
  }
  function render() {
    if (!context) return;
    const selected = select.value;
    const branch = context.branch;
    const revisions = new Map(context.seeds.map(revision => [revision.id, revision]));
    for (const revision of entries) revisions.set(revision.id, revision);
    // A selected immutable revision stays selected when another page is loaded.
    const options = [...revisions.values()].map(revision => [revision.id, `${title(revision)} · ${revision.id.slice(0,8)}`]);
    if (branch.local) options.unshift(['working', '当前工作内容（包含尚未提交的修改）']);
    const stamp = JSON.stringify(options);
    if (select.dataset.options !== stamp) {
      select.replaceChildren(...options.map(([id, text]) => new Option(text, id))); select.dataset.options = stamp;
      const fallback = branch.local ? 'working' : branch.autosave_head || branch.formal_head;
      select.value = options.some(([id]) => id === selected) ? selected : options.some(([id]) => id === fallback) ? fallback : options[0]?.[0] || '';
    }
    more.hidden = !next; more.disabled = busy; changed();
  }
  async function load(append = false) {
    if (!context) return;
    const request = ++generation, active = context;
    controller?.abort(); controller = new AbortController(); busy = true; more.disabled = true;
    const params = new URLSearchParams({limit:'50'});
    if (search.value.trim()) params.set('search', search.value.trim());
    if (append && next) params.set('cursor', next);
    feedback.textContent = '正在读取来源版本…';
    try {
      const page = await api(`${endpoint(active.workspace, active.branch.id)}?${params}`, {signal:controller.signal});
      if (request !== generation) return;
      const selected = entries.find(revision => revision.id === select.value);
      if (!append) entries = selected ? [selected] : [];
      const known = new Set(entries.map(entry => entry.id)); entries.push(...page.entries.filter(entry => !known.has(entry.id)));
      next = page.next; feedback.textContent = page.total ? `找到 ${page.total} 个保存版本` : '没有匹配的保存版本。';
    } catch (error) {
      if (request !== generation || error.name === 'AbortError') return;
      feedback.textContent = `来源版本读取失败：${error.message}`;
    } finally {
      if (request === generation) { busy = false; render(); }
    }
  }
  function show(workspace, branch, revisions) {
    if (!branch) { clear(); changed(); return; }
    const different = context?.workspace !== workspace || context?.branch.id !== branch.id;
    const selected = !different && [...context.seeds, ...entries].find(revision => revision.id === select.value);
    const stamp = JSON.stringify([branch.formal_head, branch.autosave_head]);
    if (different) clear();
    const seeds = revisions.filter(revision => revision.id === branch.formal_head || revision.id === branch.autosave_head);
    if (selected && !seeds.some(revision => revision.id === selected.id)) seeds.push(selected);
    context = {workspace, branch, seeds};
    render();
    if (different || stamp !== version) { version = stamp; load(); }
  }
  search.oninput = () => { clearTimeout(timer); timer = setTimeout(() => load(), 200); };
  more.onclick = () => load(true);
  form.addEventListener('reset', clear);
  return {show, clear};
}
