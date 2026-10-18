import { createSettingsPage } from './settings.js';
import { createRevisionViewer } from './revisions.js';
import { createWorkspaceView } from './workspace.js';
const $ = (id) => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.has('token')) {
  sessionStorage.setItem('agentdrive-token', fragment.get('token'));
  history.replaceState(null, '', location.pathname + location.search);
}
let token = sessionStorage.getItem('agentdrive-token') || '';
let snapshot = null;
let selectedBranch = null;
let loading = false;
let generation = 0;
const labels = {commit:'Commit', autosave:'自动保存', checkpoint:'检查点', candidate:'合并候选'};
const titles = {commit:'保存工作内容', autosave:'自动保存工作进度', checkpoint:'保存检查点', candidate:'准备合并内容'};
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
    sessionStorage.removeItem('agentdrive-token'); token = ''; revisionViewer.clear(); showLogin();
    throw new Error('访问令牌无效，请重新连接。');
  }
  if (!response.ok) {
    const result = await response.json().catch(() => null);
    throw Object.assign(new Error(result?.error?.message || `请求失败：${response.status}`), {status:response.status});
  }
  return response.json();
}
function chooseBranch(id) { revisionViewer.clear(); selectedBranch = id; render(); }
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
function render() {
  if (!snapshot) return;
  const focusId = document.activeElement?.id;
  const {status,revisions,summary} = snapshot;
  const branches = status.branches.filter(branch => branch.state !== 'deleted');
  if (!branches.some(branch => branch.id === selectedBranch)) selectedBranch = branches.find(branch => branch.id === status.selected_branch)?.id || branches[0]?.id;
  $('workspace-name').textContent = status.workspace.name;
  $('location').textContent = `本地节点 ${short(status.location)}`;
  $('branch-count').textContent = summary.branch_count;
  $('mount-count').textContent = summary.mount_count;
  $('mount-detail').textContent = `${summary.ready_mount_count} 个就绪 · ${summary.mount_count - summary.ready_mount_count} 个未就绪`;
  $('mounted-branch-count').textContent = summary.mounted_branch_count;
  $('sync-status').textContent = !status.capabilities.remote_configured ? '未启用' : status.pending_jobs ? '同步中' : '已同步';
  $('sync-detail').textContent = !status.capabilities.remote_configured ? '内容保存在本机' : `${status.pending_jobs} 个待处理操作`;
  workspaceView.render(snapshot, selectedBranch);
  const branch = branches.find(branch => branch.id === selectedBranch);
  if (!branch) {
    revisionViewer.clear();
    $('revision-list').replaceChildren(element('div', 'no-revisions', '暂无保存的版本。'));
    $('history-count').textContent = '0';
    return;
  }
  $('history-title').textContent = branch.name;
  $('branch-meta').replaceChildren(element('code','',short(branch.id)));
  const origin = snapshot.origins.find(value => value.branch === branch.id);
  const parent = status.branches.find(value => value.id === origin?.source_branch);
  $('branch-meta').append(element('span','',origin?.workspace_root ? '工作区初始分支' : `来源 ${parent?.name || short(origin?.source_branch)} · ${short(branch.base_revision)}`));
  $('branch-state').textContent = branch.dirty ? '存在工作修改' : branch.state === 'stopped' ? '已停止写入' : '本地已保存';
  $('branch-state').className = `pill${branch.dirty || branch.state === 'stopped' ? ' paused' : ''}`;
  const entries = branchHistory(branch,revisions);
  $('history-count').textContent = entries.length;
  const list = $('revision-list'); list.replaceChildren();
  if (!entries.length) list.append(element('div','no-revisions','暂无保存的版本。'));
  for (const revision of entries) {
    const row = element('div', 'revision-row');
    row.append(element('span','revision-node'));
    const copy = element('span','revision-copy'); copy.append(element('span','revision-title',revision.message || titles[revision.kind] || revision.kind));
    const sub = element('span','revision-sub'); sub.append(element('code','',short(revision.id)),element('span','','·'),element('span','',revision.actor));
    if (revision.id === branch.formal_head) sub.append(element('span','','HEAD'));
    copy.append(sub); row.append(copy);
    const time = element('span','revision-time',clock(revision.created_ns)); time.append(element('span','revision-type',labels[revision.kind] || revision.kind)); row.append(time);
    row.append(revisionViewer.button({workspace:status.workspace.id, revision,
      message:revision.message || titles[revision.kind] || revision.kind,
      sourceBranch:status.branches.find(value => value.id === revision.branch)?.name || short(revision.branch),
      isHead:revision.id === branch.formal_head}));
    list.append(row);
  }
  revisionViewer.update(status.workspace.id, revisions);
  if (focusId) document.getElementById(focusId)?.focus({preventScroll:true});
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
    if (!workspaces.length) { snapshot = null; revisionViewer.clear(); $('workspace-name').textContent='暂无工作区'; ['branch-count','mount-count','mounted-branch-count'].forEach(id => $(id).textContent='0'); $('mount-detail').textContent='暂无挂载点'; $('sync-status').textContent='—'; $('sync-detail').textContent='暂无工作区'; }
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
const revisionViewer = createRevisionViewer({api, element});
const workspaceView = createWorkspaceView({element, selectBranch:chooseBranch, showRevision:(revision, triggerId) => {
  const branch = snapshot.status.branches.find(value => value.id === revision.branch);
  revisionViewer.information({workspace:snapshot.status.workspace.id, revision,
    message:revision.message || titles[revision.kind] || revision.kind,
    sourceBranch:branch?.name || short(revision.branch), isHead:revision.id === branch?.formal_head,
    triggerId});
}});
function isSettings() { return location.pathname === '/settings'; }
async function refreshPage() {
  if (!token) return;
  if (isSettings()) await settingsPage.load();
  else await refresh();
}
function showPage() {
  revisionViewer.clear();
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
  document.title = settings ? 'AgentDrive · 设置' : 'AgentDrive · 分支与版本';
  refreshPage();
}
for (const id of ['history-nav', 'settings-nav']) $(id).addEventListener('click', event => {
  if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault(); history.pushState(null, '', $(id).getAttribute('href')); showPage();
});
window.addEventListener('popstate', showPage);
$('login-form').addEventListener('submit', event => { event.preventDefault(); token=$('token').value.trim(); sessionStorage.setItem('agentdrive-token',token); $('token').value=''; settingsPage.clear(); $('login').hidden=true; $('app').hidden=false; showPage(); });
$('logout').addEventListener('click', () => { generation++; token=''; loading=false; sessionStorage.removeItem('agentdrive-token'); snapshot=null; settingsPage.clear(); revisionViewer.clear(); showLogin(); });
$('workspace').addEventListener('change', () => { selectedBranch=null; revisionViewer.clear(); refresh(true); });
$('refresh').addEventListener('click', refreshPage);
showPage();
if (token) { $('login').hidden=true; $('app').hidden=false; } else showLogin();
setInterval(() => { if (!document.hidden) refreshPage(); },15000);
