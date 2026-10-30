export function createRelationshipsView({api, element, container}) {
  const short = value => value?.slice(0, 8) || '—';
  const states = {preparing:'准备中', ready:'已挂载', paused:'已暂停', released:'已释放', recovery_required:'待恢复'};
  const heading = element('div', 'panel-heading');
  const title = element('h2', '', '分支与挂载关系');
  const count = element('span', 'count-badge', '0'); count.id = 'relationship-count'; title.append(count);
  const selection = element('p'); selection.id = 'relationship-selection';
  const copy = element('div'); copy.append(title, selection); heading.append(copy);
  const form = element('form', 'relationship-filters');
  const fields = {};
  function field(name, label, options) {
    const wrapper = element('label', '', label);
    const input = element(options ? 'select' : 'input');
    input.id = `relationship-${name}`;
    if (options) for (const [value, text] of options) {
      const option = element('option', '', text); option.value = value; input.append(option);
    }
    else { input.type = 'text'; input.placeholder = '全部'; input.spellcheck = false; input.autocomplete = 'off'; }
    fields[name] = input; wrapper.append(input); form.append(wrapper); return input;
  }
  field('branch', '分支', [['', '全部分支']]);
  field('mount_state', '挂载状态', [['', '当前挂载'], ...Object.entries(states)]);
  field('owner_location', '写入所属 location ID');
  field('mount_location', '挂载 location ID');
  field('mount', '挂载 ID');
  const apply = element('button', 'secondary', '查询'); apply.type = 'submit'; form.append(apply);
  const note = element('p', 'relationship-note', '挂载目录和默认分支仅记录本机信息。远端挂载、远端默认分支及机器在线状态未知。');
  const status = element('p', 'relationship-status'); status.id = 'relationship-status'; status.setAttribute('role', 'status');
  const wrap = element('div', 'mount-table-wrap');
  const table = element('table', 'relationship-table'); table.id = 'relationship-table';
  const header = element('tr');
  for (const text of ['分支', '写入所属 location', '挂载目录', '使用者与状态', '创建来源']) {
    const cell = element('th', '', text); cell.scope = 'col'; header.append(cell);
  }
  const head = element('thead'); head.append(header);
  const body = element('tbody'); body.id = 'relationship-list'; table.append(head, body); wrap.append(table);
  const more = element('button', 'secondary relationship-more', '继续加载'); more.type = 'button'; more.hidden = true;
  more.id = 'relationship-more';
  container.replaceChildren(heading, form, note, status, wrap, more);
  let workspace = null, current = null, next = null, generation = 0, controller = null;

  function clear() {
    generation++; controller?.abort(); controller = null;
    workspace = null; current = null; next = null; body.replaceChildren(); count.textContent = '0';
    more.hidden = true; status.textContent = ''; selection.textContent = '';
    for (const input of Object.values(fields)) input.value = '';
  }
  function value(parent, text, className = '') {
    const node = element('small', className, text); parent.append(node); return node;
  }
  function identifier(parent, id, prefix = '') {
    if (!id) return;
    const node = element('code', 'relationship-id', `${prefix}${short(id)}`); node.title = id; parent.append(node);
  }
  function row(entry, location) {
    const {branch, mount} = entry, owner = branch.ownership, origin = branch.origin;
    const row = element('tr'); row.dataset.branch = branch.id;
    if (mount) row.dataset.mount = mount.id;
    const name = element('td'); name.append(element('strong', '', branch.name || short(branch.id)));
    identifier(name, branch.id);
    if (current.selected_branch === branch.id) value(name, '本机默认分支');
    if (branch.state === 'stopped') value(name, '已停止写入');
    if (branch.state === 'deleted') value(name, '已删除');
    const ownership = element('td');
    ownership.append(element('span', '', owner.location === location ? '本机' : '其他 location'));
    identifier(ownership, owner.location);
    value(ownership, owner.source === 'local_authority' ? '本机管理写入' : owner.source === 'remote_reference' ? '远端观测记录' : '本机保留记录');
    const ownershipDetails = element('details'); ownershipDetails.append(element('summary', '', '记录详情'));
    value(ownershipDetails, `authority_epoch: ${owner.authority_epoch} · generation: ${owner.generation}`);
    if (owner.source === 'remote_reference') {
      value(ownershipDetails, owner.observed_ns == null ? '观测时间未知' : `观测于 ${new Date(owner.observed_ns / 1e6).toLocaleString('zh-CN', {hour12:false})}`);
      value(ownershipDetails, `存储版本：${owner.storage_version ?? '未知'}`);
    }
    ownership.append(ownershipDetails);
    const directory = element('td');
    const state = element('td');
    if (mount) {
      directory.append(element('code', 'mount-path', mount.path)); identifier(directory, mount.id, 'ID ');
      value(directory, `挂载于 ${entry.mount_location === location ? '本机' : entry.mount_location}`);
      value(directory, `generation: ${mount.generation} · ${mount.access === 'read_only' ? '只读' : '读写'}`);
      state.append(element('span', '', mount.principal), element('span', `mount-state ${mount.state}`, states[mount.state] || mount.state));
    } else {
      directory.append(element('span', '', '本机暂无挂载'));
      state.textContent = owner.location === location ? '—' : '远端挂载未知';
    }
    const source = element('td');
    if (origin.status === 'restricted') source.textContent = '来源信息无权限';
    else {
      source.append(element('span', '', origin.source_branch ? origin.source_name || short(origin.source_branch) : origin.workspace_root ? '工作区初始版本' : origin.base_revision ? '来源版本' : '来源记录不可用'));
      if (origin.source_branch) identifier(source, origin.source_branch, '分支 ');
      identifier(source, origin.base_revision, '版本 ');
      if (origin.status === 'unavailable' && origin.base_revision) value(source, '来源记录不完整');
      if (origin.captured_working_root) {
        const details = element('details'); details.append(element('summary', '', '创建时的内容对象'));
        details.append(element('code', 'relationship-root', origin.captured_working_root.id)); source.append(details);
      }
    }
    row.append(name, ownership, directory, state, source); return row;
  }
  async function load(append = false) {
    if (!workspace) return;
    const request = ++generation; controller?.abort(); controller = new AbortController();
    const params = new URLSearchParams({limit:'50'});
    for (const [name, input] of Object.entries(fields)) if (input.value.trim()) params.set(name, input.value.trim());
    if (append && next) params.set('cursor', next);
    if (!append) { next = null; body.replaceChildren(); count.textContent = '0'; more.hidden = true; }
    status.textContent = '正在读取关系…'; more.disabled = true;
    try {
      const result = await api(`/api/workspaces/${encodeURIComponent(workspace)}/relationships?${params}`, {signal:controller.signal});
      if (request !== generation) return;
      current = result; next = result.next;
      const selected = result.entries.find(entry => entry.branch.id === result.selected_branch)?.branch.name;
      selection.textContent = `本机 location ${result.observing_location} · 默认分支 ${selected || result.selected_branch || '未选择'}`;
      for (const entry of result.entries) body.append(row(entry, result.observing_location));
      count.textContent = result.total;
      status.textContent = result.total ? `已显示 ${body.children.length} / ${result.total} 条已知关系` : '没有符合条件的已知关系。';
      more.hidden = !next;
    } catch (error) {
      if (request !== generation || error.name === 'AbortError') return;
      if (append && error.code === 'TARGET_MOVED') {
        await load();
        return;
      }
      next = null; more.hidden = true;
      status.textContent = `关系查询失败：${error.message}`;
    } finally { if (request === generation) more.disabled = false; }
  }
  async function show(snapshot) {
    if (!snapshot) { clear(); return; }
    if (workspace !== snapshot.workspace.id) clear();
    workspace = snapshot.workspace.id;
    const selected = fields.branch.value;
    const options = new Map(snapshot.branches.map(branch => [branch.id, branch.name]));
    for (const branch of snapshot.remote_branches || []) if (!options.has(branch.branch)) options.set(branch.branch, branch.branch);
    fields.branch.replaceChildren();
    for (const [id, name] of [['', '全部分支'], ...options]) {
      const option = element('option', '', name); option.value = id; fields.branch.append(option);
    }
    fields.branch.value = options.has(selected) ? selected : '';
    await load();
  }
  form.onsubmit = event => { event.preventDefault(); load(); };
  more.onclick = () => load(true);
  return {show, clear};
}
