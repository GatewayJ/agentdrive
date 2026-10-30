import { createCommandPanel } from './commands.js';

export function createManagementPage({api, element, container, kind, changed, onBusy = () => {}}) {
  const mounts = kind === 'mounts';
  const heading = element('div', 'page-heading management-heading'), intro = element('div');
  intro.append(element('div', 'eyebrow', mounts ? 'LOCAL MOUNTS' : 'WORKSPACE BRANCHES'),
    element('h1', '', mounts ? '挂载点管理' : '分支管理'),
    element('p', '', mounts ? '选择分支和本机目录，管理 Agent 使用的工作目录。' : '从已有成果创建分支，选择本机继续工作的分支。'));
  const locationLabel = element('span', 'location-label'); heading.append(intro, locationLabel);
  const empty = element('div', 'empty-state'); empty.hidden = true;
  empty.append(element('h2', '', '暂无工作区'), element('p', '', '创建或同步工作区后，可以在这里管理挂载和分支。'));
  const content = element('div', 'management-content');
  const formPanel = element('section', 'panel management-create'), formHeader = element('div', 'panel-heading');
  formHeader.append(element('h2', '', mounts ? '新增挂载点' : '创建分支'));
  const form = element('form', 'management-form'); form.id = mounts ? 'mount-create-form' : 'branch-create-form';
  const inputs = {};
  function field(name, label, type = 'select') {
    const group = element('label', 'management-field', label), input = element(type === 'select' ? 'select' : 'input');
    input.id = `${kind}-${name}`; input.required = true;
    if (type !== 'select') { input.type = type; input.autocomplete = 'off'; }
    group.append(input); form.append(group); inputs[name] = input; return input;
  }
  const branchInput = field('branch', mounts ? '挂载分支' : '来源分支');
  const revisionInput = !mounts && field('revision', '来源内容');
  if (mounts) {
    field('path', '本机目录', 'text').placeholder = '/workspace/project';
    field('access', '访问方式');
    for (const [value, label] of [['read_write','读写'],['read_only','只读']]) inputs.access.append(new Option(label, value));
  } else { field('name', '分支名称', 'text').placeholder = 'feat/new-task'; inputs.name.maxLength = 256; }
  const hint = element('p', 'management-hint');
  const submit = element('button', 'primary', mounts ? '创建挂载点' : '创建分支'); submit.type = 'submit';
  form.append(hint, submit); formPanel.append(formHeader, form);
  const operationHost = element('div');
  const listing = element('section', 'panel management-list');
  listing.setAttribute('aria-label', mounts ? '本机挂载点' : '工作区分支');
  const listHeader = element('div', 'panel-heading'), count = element('span', 'count-badge');
  listHeader.append(element('h2', '', mounts ? '本机挂载点' : '工作区分支'), count);
  const rows = element('div', 'management-rows'); rows.id = `${kind}-list`; listing.append(listHeader, rows);
  content.append(operationHost, formPanel); if (!mounts) content.append(listing); container.append(heading, empty, content);
  let snapshot = null, project = null, busy = false;
  const runner = createCommandPanel({api, element, container:operationHost, scope:kind, changed,
    setBusy:value => { busy = value; updateBusy(); onBusy(value); }});
  function updateBusy() {
    for (const input of form.elements) input.disabled = busy || !snapshot;
    for (const button of rows.querySelectorAll('button')) button.disabled = busy || button.dataset.unavailable === 'true';
    if (snapshot && (!eligible().length || !mounts && !revisionInput.value)) submit.disabled = true;
  }
  function eligible() {
    if (!snapshot) return [];
    return snapshot.branches.filter(branch => branch.state !== 'deleted' &&
      (!mounts || branch.local && (inputs.access.value === 'read_only' || branch.owner === snapshot.status.location && branch.state === 'writable')));
  }
  function options(input, values, fallback) {
    const selected = input.value;
    const signature = JSON.stringify(values);
    if (input.dataset.options === signature) return;
    input.replaceChildren(...values.map(([id, label]) => new Option(label, id)));
    input.dataset.options = signature;
    input.value = values.some(([id]) => id === selected) ? selected : values.some(([id]) => id === fallback) ? fallback : values[0]?.[0] || '';
  }
  function sourceOptions() {
    if (!snapshot || mounts) return;
    const branch = snapshot.branches.find(branch => branch.id === branchInput.value);
    const revisions = snapshot.revisions.filter(revision => revision.branch === branch?.id || revision.id === branch?.formal_head || revision.id === branch?.autosave_head)
      .sort((a,b) => b.created_ns-a.created_ns);
    const values = revisions.map(revision => [revision.id, `${revision.message || {commit:'提交',autosave:'自动保存',checkpoint:'检查点'}[revision.kind] || '版本'} · ${revision.id.slice(0,8)}`]);
    if (branch?.local) values.unshift(['working', '当前工作内容（包含尚未提交的修改）']);
    options(revisionInput, values, branch?.local ? 'working' : branch?.autosave_head || branch?.formal_head);
    submit.disabled = busy || !values.length;
  }
  function action(parent, label, callback, unavailable = false) {
    const button = element('button', 'secondary', label); button.type = 'button';
    button.dataset.unavailable = String(unavailable); button.onclick = callback; parent.append(button); return button;
  }
  function link(parent, label, path, branch) {
    const anchor = element('a', 'management-link', label);
    anchor.href = `${path}?${new URLSearchParams({workspace:project,branch})}`; parent.append(anchor);
  }
  function renderRows() {
    const focusId = document.activeElement?.id;
    rows.replaceChildren();
    const values = snapshot.branches.filter(branch => branch.state !== 'deleted');
    count.textContent = values.length;
    if (!values.length) rows.append(element('p', 'panel-empty', '暂无分支。'));
    for (const item of values) {
      const row = element('article', 'management-row'); row.dataset.branch = item.id;
      const copy = element('div', 'management-row-copy');
      copy.append(element('h3', '', item.name));
      const source = snapshot.branches.find(value => value.id === item.source_branch);
      copy.append(element('p', '', `${item.owner === snapshot.status.location ? '本机管理写入' : `其他 location ${item.owner.slice(0,8)}`} · ${source ? `来自 ${source.name}` : item.source_branch ? `来自 ${item.source_branch.slice(0,8)}` : '工作区初始分支'}`));
      if (snapshot.status.selected_branch === item.id) copy.append(element('span', 'pill', '本机默认分支'));
      const identifier = element('code', 'management-id', item.id.slice(0,8)); identifier.title = item.id; copy.append(identifier);
      const buttons = element('div', 'management-actions');
      action(buttons, '从此创建', () => { branchInput.value = item.id; sourceOptions(); inputs.name.focus(); formPanel.scrollIntoView({block:'nearest',behavior:'smooth'}); });
      if (item.local && item.owner === snapshot.status.location && item.state === 'writable') {
        action(buttons, '切换使用', () => runner.run('branch_switch', {workspace:project, branch:item.id}, `切换到 ${item.name}`), snapshot.status.selected_branch === item.id).id = `switch-${item.id}`;
        link(buttons, '挂载到目录', '/mounts', item.id);
      }
      link(buttons, '版本历史', '/dashboard', item.id);
      row.append(copy, buttons); rows.append(row);
    }
    if (focusId) document.getElementById(focusId)?.focus({preventScroll:true});
  }
  function show(data) {
    const previous = project; snapshot = data; project = data?.status.workspace.id || null;
    content.hidden = !data; empty.hidden = !!data;
    if (!data) { runner.clear(); return; }
    if (previous !== project) { form.reset(); for (const input of Object.values(inputs)) delete input.dataset.options; }
    locationLabel.textContent = `本机 location ${data.status.location.slice(0,8)}`;
    const requested = new URLSearchParams(location.search).get('branch');
    options(branchInput, eligible().map(branch => [branch.id, branch.name]), requested || data.status.selected_branch);
    hint.textContent = mounts ? '目录位于当前 AgentDrive 所在机器。一个分支在本机只能有一个读写挂载点。' : '选择准确的来源内容；创建完成后，可以切换使用或挂载到本机目录。';
    sourceOptions(); if (!mounts) renderRows(); runner.show(project); updateBusy();
  }
  function clear() { snapshot = null; project = null; rows.replaceChildren(); form.reset(); runner.clear(); content.hidden = true; }
  form.onsubmit = event => {
    event.preventDefault(); if (busy || !snapshot) return;
    const textInput = mounts ? inputs.path : inputs.name;
    textInput.setCustomValidity(textInput.value.trim() ? '' : '请填写有效内容。');
    if (!form.reportValidity()) return;
    if (mounts) runner.run('mount_prepare', {workspace:project, branch:branchInput.value, path:inputs.path.value.trim(), access:inputs.access.value, durability:'local'}, '创建挂载点');
    else {
      const source = revisionInput.value === 'working' ? {source_branch:branchInput.value} : {source:revisionInput.value};
      runner.run('branch_create', {workspace:project, name:inputs.name.value.trim(), ...source}, '创建分支');
    }
  };
  branchInput.onchange = sourceOptions;
  (mounts ? inputs.path : inputs.name).oninput = event => event.target.setCustomValidity('');
  if (mounts) inputs.access.onchange = () => show(snapshot);
  const saveMount = mount => !busy && runner.run('mount_save', {mount:mount.id, expected_binding_generation:mount.generation, durability:snapshot.status.capabilities.remote_configured ? 'remote' : 'local'}, '保存挂载目录');
  const unmountMount = mount => !busy && runner.run('mount_unmount', {mount:mount.id, generation:mount.generation}, '卸载本机目录');
  return {show, clear, saveMount, unmountMount};
}
