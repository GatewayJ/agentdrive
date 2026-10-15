import { createSettingsPage } from './settings.js';
const $ = (id) => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.has('token')) {
  sessionStorage.setItem('agentfs-token', fragment.get('token'));
  history.replaceState(null, '', location.pathname + location.search);
}
let token = sessionStorage.getItem('agentfs-token') || '';
let snapshot = null;
let selectedBranch = null;
let selectedRevision = null;
let loading = false;
let generation = 0;
const labels = {commit:'Commit', autosave:'自动保存', checkpoint:'检查点', candidate:'合并候选'};
const titles = {commit:'保存工作内容', autosave:'自动保存工作进度', checkpoint:'保存会话状态', candidate:'准备合并内容'};
function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function short(id) { return id ? id.slice(0, 8) : '—'; }
function date(ns) { return new Date(ns / 1e6); }
function clock(ns) { return date(ns).toLocaleString('zh-CN', {month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}); }
function showLogin() { $('login').hidden = false; $('app').hidden = true; }
async function api(path, options = {}) {
  const response = await fetch(path, {...options, headers:{...options.headers, Authorization:`Bearer ${token}`}, cache:'no-store'});
  if (response.status === 401) {
    sessionStorage.removeItem('agentfs-token'); token = ''; showLogin();
    throw new Error('访问令牌无效，请重新连接。');
  }
  if (!response.ok) {
    const result = await response.json().catch(() => null);
    throw Object.assign(new Error(result?.error?.message || `请求失败：${response.status}`), {status:response.status});
  }
  return response.json();
}
function chooseBranch(id) { selectedBranch = id; selectedRevision = null; render(); }
function renderTree(branches, current) {
  const container = $('branch-tree'); container.replaceChildren();
  const byId = new Map(branches.map(branch => [branch.id, branch]));
  const children = new Map();
  for (const branch of branches) {
    const parent = byId.has(branch.source_branch) && branch.source_branch !== branch.id ? branch.source_branch : null;
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push(branch);
  }
  const seen = new Set();
  function group(items, root) {
    const list = element('ul', `tree-group${root ? ' root' : ''}`); list.setAttribute('role', 'group');
    for (const branch of items.sort((a,b) => (a.name === 'main' ? -1 : b.name === 'main' ? 1 : a.name.localeCompare(b.name)))) {
      if (seen.has(branch.id)) continue;
      seen.add(branch.id);
      const item = element('li','tree-item'); item.setAttribute('role','none');
      const button = element('button', `branch-button${branch.id === selectedBranch ? ' selected' : ''}`);
      button.type = 'button'; button.setAttribute('role','treeitem'); button.setAttribute('aria-selected', String(branch.id === selectedBranch)); button.title = branch.name;
      button.append(element('span','branch-symbol','⑂'));
      const copy = element('span','branch-copy'); copy.append(element('span','branch-name',branch.name), element('span','branch-sub',short(branch.id))); button.append(copy);
      if (branch.id === current) button.append(element('span','current-mark','当前'));
      button.addEventListener('click', () => chooseBranch(branch.id)); item.append(button);
      if (children.has(branch.id)) item.append(group(children.get(branch.id), false));
      list.append(item);
    }
    return list;
  }
  container.append(group(children.get(null) || [], true));
  const remaining = branches.filter(branch => !seen.has(branch.id));
  if (remaining.length) container.append(group(remaining, true));
}
function branchHistory(branch, revisions) {
  const byId = new Map(revisions.map(revision => [revision.id, revision]));
  const visited = new Set(); const pending = [branch.formal_head, branch.autosave_head];
  while (pending.length) {
    const id = pending.pop(); if (!id || visited.has(id)) continue;
    const revision = byId.get(id); if (!revision) continue;
    visited.add(id); pending.push(...revision.parents);
  }
  return revisions.filter(revision => visited.has(revision.id)).sort((a,b) => b.created_ns - a.created_ns || a.id.localeCompare(b.id));
}
function detail(revision, branch) {
  const panel = $('revision-detail'); panel.replaceChildren(); panel.hidden = !revision;
  if (!revision) return;
  const heading = element('div','detail-heading','版本信息'); heading.append(element('span','',short(revision.id))); panel.append(heading);
  const grid = element('dl','detail-grid');
  const fields = [
    ['版本 ID', revision.id], ['说明', revision.message || titles[revision.kind] || revision.kind],
    ['创建者',revision.actor], ['保存时间',date(revision.created_ns).toLocaleString('zh-CN',{hour12:false})],
    ['父版本',revision.parents.length ? revision.parents.map(short).join(' / ') : '初始版本'],
    ['内容对象',revision.root.id], ['来源分支',snapshot.status.branches.find(value => value.id === revision.branch)?.name || short(revision.branch)],
    ['保存状态', '本地已保存'],
  ];
  if (revision.id === branch.formal_head) fields.push(['分支位置','当前 commit']);
  for (const [label,value] of fields) { grid.append(element('dt','',label),element('dd','',value)); }
  panel.append(grid);
}
function render() {
  if (!snapshot) return;
  const {status,revisions} = snapshot;
  const branches = status.branches.filter(branch => branch.state !== 'deleted');
  if (!branches.some(branch => branch.id === selectedBranch)) selectedBranch = branches.find(branch => branch.id === status.selected_branch)?.id || branches[0]?.id;
  $('workspace-name').textContent = status.workspace.name;
  $('location').textContent = `本地节点 ${short(status.location)}`;
  $('branch-count').textContent = branches.length;
  $('revision-count').textContent = revisions.filter(revision => revision.kind !== 'candidate').length;
  $('commit-count').textContent = `${revisions.filter(revision => revision.kind === 'commit').length} 个 commit`;
  $('current-branch').textContent = branches.find(branch => branch.id === status.selected_branch)?.name || '尚未选择';
  $('sync-status').textContent = !status.capabilities.remote_configured ? '本地保存' : status.pending_jobs ? '同步中' : '已同步';
  $('sync-detail').textContent = !status.capabilities.remote_configured ? '未配置远程存储' : `${status.pending_jobs} 个待处理操作`;
  renderTree(branches, status.selected_branch);
  const branch = branches.find(branch => branch.id === selectedBranch);
  if (!branch) return;
  $('history-title').textContent = branch.name;
  $('branch-meta').replaceChildren(element('code','',short(branch.id)));
  const parent = branches.find(value => value.id === branch.source_branch);
  $('branch-meta').append(element('span','',parent ? `来源 ${parent.name} · ${short(branch.base_revision)}` : '工作区初始分支'));
  $('branch-state').textContent = branch.dirty ? '存在工作修改' : branch.state === 'stopped' ? '已停止写入' : '本地已保存';
  $('branch-state').className = `pill${branch.dirty || branch.state === 'stopped' ? ' paused' : ''}`;
  const entries = branchHistory(branch,revisions);
  $('history-count').textContent = entries.length;
  if (!entries.some(revision => revision.id === selectedRevision)) selectedRevision = entries[0]?.id;
  const list = $('revision-list'); list.replaceChildren();
  if (!entries.length) list.append(element('div','no-revisions','暂无保存的版本。'));
  for (const revision of entries) {
    const row = element('button',`revision-row${revision.id === selectedRevision ? ' selected' : ''}`); row.type = 'button';
    row.setAttribute('aria-pressed',String(revision.id === selectedRevision));
    row.append(element('span','revision-node'));
    const copy = element('span','revision-copy'); copy.append(element('span','revision-title',revision.message || titles[revision.kind] || revision.kind));
    const sub = element('span','revision-sub'); sub.append(element('code','',short(revision.id)),element('span','','·'),element('span','',revision.actor));
    if (revision.id === branch.formal_head) sub.append(element('span','','HEAD'));
    copy.append(sub); row.append(copy);
    const time = element('span','revision-time',clock(revision.created_ns)); time.append(element('span','revision-type',labels[revision.kind] || revision.kind)); row.append(time);
    row.addEventListener('click', () => { selectedRevision = revision.id; render(); }); list.append(row);
  }
  detail(entries.find(revision => revision.id === selectedRevision),branch);
}
async function refresh(force=false) {
  if (!token || (loading && !force)) return;
  const request = ++generation; loading = true; $('refresh').disabled = true;
  try {
    const workspaces = await api('/api/workspaces');
    if (request !== generation) return;
    const selection = $('workspace').value;
    $('workspace').replaceChildren();
    for (const workspace of workspaces) { const option = element('option','',workspace.name); option.value = workspace.id; $('workspace').append(option); }
    if (workspaces.some(workspace => workspace.id === selection)) $('workspace').value = selection;
    $('empty').hidden = workspaces.length > 0; $('panels').hidden = !workspaces.length;
    if (!workspaces.length) { snapshot = null; $('workspace-name').textContent='暂无工作区'; ['branch-count','revision-count'].forEach(id => $(id).textContent='0'); }
    else {
      const data = await api(`/api/workspaces/${encodeURIComponent($('workspace').value)}`);
      if (request !== generation) return;
      snapshot = data; render();
    }
    $('error').hidden = true; $('updated').textContent = `${new Date().toLocaleTimeString('zh-CN',{hour12:false})} 已刷新`;
    $('connection').textContent = '已连接本地 daemon';
  } catch (error) {
    if (request !== generation) return;
    $('error').textContent = error.message; $('error').hidden = false; $('login-error').textContent = error.message; $('connection').textContent = '连接需要检查';
  } finally { if (request === generation) { loading = false; $('refresh').disabled = false; } }
}
const settingsPage = createSettingsPage({api, element, $});
function isSettings() { return location.pathname === '/settings'; }
async function refreshPage() {
  if (!token) return;
  if (isSettings()) await settingsPage.load();
  else await refresh();
}
function showPage() {
  const settings = isSettings();
  $('history-page').hidden = settings;
  $('settings-page').hidden = !settings;
  $('history-breadcrumb').hidden = settings;
  $('settings-breadcrumb').hidden = !settings;
  $('history-nav').classList.toggle('nav-active', !settings);
  $('settings-nav').classList.toggle('nav-active', settings);
  for (const [id, active] of [['history-nav', !settings], ['settings-nav', settings]]) {
    if (active) $(id).setAttribute('aria-current', 'page');
    else $(id).removeAttribute('aria-current');
  }
  $('view-mode').textContent = settings ? '全局配置' : '只读浏览';
  $('workspace').closest('.workspace-picker').hidden = settings;
  document.title = settings ? 'AgentFS · 设置' : 'AgentFS · 分支与版本';
  refreshPage();
}
for (const id of ['history-nav', 'settings-nav']) $(id).addEventListener('click', event => {
  if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault(); history.pushState(null, '', $(id).getAttribute('href')); showPage();
});
window.addEventListener('popstate', showPage);
$('login-form').addEventListener('submit', event => { event.preventDefault(); token=$('token').value.trim(); sessionStorage.setItem('agentfs-token',token); $('token').value=''; settingsPage.clear(); $('login').hidden=true; $('app').hidden=false; showPage(); });
$('logout').addEventListener('click', () => { generation++; token=''; loading=false; sessionStorage.removeItem('agentfs-token'); snapshot=null; settingsPage.clear(); showLogin(); });
$('workspace').addEventListener('change', () => { selectedBranch=null; selectedRevision=null; refresh(true); });
$('refresh').addEventListener('click', refreshPage);
showPage();
if (token) { $('login').hidden=true; $('app').hidden=false; } else showLogin();
setInterval(() => { if (!document.hidden) refreshPage(); },15000);
