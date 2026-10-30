import { createManagementPage } from './management.js';
import { createArchivePage } from './archive.js';
import { createBranchFileViewer } from './branch-files.js';
import { createSettingsPage } from './settings.js';
import { createRevisionViewer } from './revisions.js';
import { createWorkspaceView } from './workspace.js';
import { createRelationshipsView } from './relationships.js';
const $ = (id) => document.getElementById(id);
function clearCommandTickets() {
  for (const key of Object.keys(sessionStorage)) if (key.startsWith('agentdrive-action:')) sessionStorage.removeItem(key);
}
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.has('token')) {
  if (fragment.get('token') !== sessionStorage.getItem('agentdrive-token')) clearCommandTickets();
  sessionStorage.setItem('agentdrive-token', fragment.get('token'));
  history.replaceState(null, '', location.pathname + location.search);
}
let token = sessionStorage.getItem('agentdrive-token') || '';
let snapshot = null;
let selectedBranch = new URLSearchParams(location.search).get('branch');
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
  const {binary = false, ...fetchOptions} = options;
  const response = await fetch(path, {...fetchOptions, headers:{...options.headers, Authorization:`Bearer ${token}`}, cache:'no-store'});
  if (response.status === 401) {
    clearCommandTickets();
    sessionStorage.removeItem('agentdrive-token'); token = ''; revisionViewer.clear(); branchFileViewer.clear(); archivePage.clear(); relationshipsView.clear(); mountsPage.clear(); branchesPage.clear(); showLogin();
    throw new Error('访问令牌无效，请重新连接。');
  }
  if (!response.ok) {
    const result = await response.json().catch(() => null);
    throw Object.assign(new Error(result?.error?.message || `请求失败：${response.status}`), {status:response.status, code:result?.error?.code});
  }
  return binary ? response.arrayBuffer() : response.json();
}
function chooseBranch(id) {
  revisionViewer.clear(); selectedBranch = id;
  const url = new URL(location.href); url.searchParams.set('workspace', snapshot.status.workspace.id); url.searchParams.set('branch', id); url.searchParams.delete('path');
  history.replaceState(null, '', url.pathname+url.search); render(false);
}
function branchHistory(branch, revisions) {
  return revisions.filter(revision => revision.branch === branch.id).sort((a,b) => b.created_ns - a.created_ns || a.id.localeCompare(b.id));
}
function branchSaveStatus(branch, revisions) {
  if (!branch.local) {
    return {text:'远端已保存', className:'pill', title:'显示最近一次同步取得的版本。'};
  }
  if (branch.mutation_seq !== branch.saved_seq) {
    return {text:'有未保存修改', className:'pill paused', title:'最新修改尚未保存到本机。'};
  }
  if (!branch.dirty) {
    return {text:'已提交', className:'pill', title:'当前内容与分支的提交基准一致。'};
  }
  const saved = revisions.find(revision => revision.id === (branch.autosave_head || branch.formal_head));
  const title = !saved ? '内容已保存到本机，版本状态暂不可确认。'
    : saved.root.id === branch.working_root.id ? '修改已保存为版本，尚未正式提交。'
    : '内容已保存到本机，正在等待生成自动保存版本。';
  return {text:'已自动保存 · 有未提交修改', className:'pill autosaved', title};
}
function render(updateTree = true) {
  if (!snapshot) return;
  const focusId = document.activeElement?.id;
  const {status,revisions,summary} = snapshot;
  const branches = snapshot.branches.filter(branch => branch.state !== 'deleted');
  if (!branches.some(branch => branch.id === selectedBranch)) selectedBranch = branches.find(branch => branch.id === status.selected_branch)?.id || branches[0]?.id;
  $('workspace-name').textContent = status.workspace.name;
  $('location').textContent = `本地节点 ${short(status.location)}`;
  $('branch-count').textContent = summary.branch_count;
  $('mount-count').textContent = summary.mount_count;
  $('mount-detail').textContent = `${summary.ready_mount_count} 个就绪 · ${summary.mount_count - summary.ready_mount_count} 个未就绪`;
  $('mounted-branch-count').textContent = summary.mounted_branch_count;
  $('sync-status').textContent = !status.capabilities.remote_configured ? '未启用' : status.pending_jobs ? '同步中' : '已同步';
  $('sync-detail').textContent = !status.capabilities.remote_configured ? '内容保存在本机' : `${status.pending_jobs} 个待处理操作`;
  if (updateTree) workspaceView.render(snapshot, selectedBranch);
  else workspaceView.select(selectedBranch);
  branchFileViewer.update(status.workspace.id, status.branches);
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
  const parent = snapshot.branches.find(value => value.id === origin?.source_branch);
  $('branch-meta').append(element('span','',origin?.workspace_root ? '工作区初始分支' : origin ? `来源 ${parent?.name || short(origin.source_branch)} · ${short(branch.base_revision)}` : '来源信息尚未同步'));
  $('branch-meta').append(element('span','',branch.owner === status.location ? '本机分支' : `所属节点 ${short(branch.owner)}`));
  if (branch.state === 'stopped') $('branch-meta').append(element('span', 'pill paused', '已停止写入'));
  const saveStatus = branchSaveStatus(branch, revisions);
  $('branch-state').textContent = saveStatus.text;
  $('branch-state').className = saveStatus.className;
  $('branch-state').title = saveStatus.title;
  const entries = branchHistory(branch,revisions);
  $('history-count').textContent = entries.length;
  const list = $('revision-list'); list.replaceChildren();
  if (!entries.length) list.append(element('div','no-revisions','此分支尚未生成新版本。'));
  for (const revision of entries) {
    const row = element('button', 'revision-row');
    row.append(element('span','revision-node'));
    const copy = element('span','revision-copy'); copy.append(element('span','revision-title',revision.message || titles[revision.kind] || revision.kind));
    const sub = element('span','revision-sub'); sub.append(element('code','',short(revision.id)),element('span','','·'),element('span','',revision.actor));
    if (revision.id === branch.formal_head) sub.append(element('span','','HEAD'));
    copy.append(sub); row.append(copy);
    const time = element('span','revision-time',clock(revision.created_ns)); time.append(element('span','revision-type',labels[revision.kind] || revision.kind)); row.append(time);
    revisionViewer.bind(row, {workspace:status.workspace.id, revision,
      message:revision.message || titles[revision.kind] || revision.kind,
      sourceBranch:snapshot.branches.find(value => value.id === revision.branch)?.name || short(revision.branch),
      isHead:revision.id === branch.formal_head});
    list.append(row);
  }
  revisionViewer.update(status.workspace.id, revisions, snapshot.branches);
  if (focusId) document.getElementById(focusId)?.focus({preventScroll:true});
}
async function refresh(force=false) {
  if (!token || (loading && !force)) return;
  const request = ++generation; loading = true; $('refresh').disabled = true;
  try {
    const workspaces = await api('/api/workspaces');
    if (request !== generation) return;
    const selection = new URLSearchParams(location.search).get('workspace') || $('workspace').value || sessionStorage.getItem('agentdrive-workspace');
    $('workspace').replaceChildren();
    for (const workspace of workspaces) { const option = element('option','',workspace.name); option.value = workspace.id; $('workspace').append(option); }
    if (workspaces.some(workspace => workspace.id === selection)) $('workspace').value = selection;
    if ($('workspace').value) sessionStorage.setItem('agentdrive-workspace', $('workspace').value);
    $('empty').hidden = workspaces.length > 0; $('panels').hidden = !workspaces.length;
    if (!workspaces.length) { snapshot = null; revisionViewer.clear(); branchFileViewer.clear(); relationshipsView.clear(); mountsPage.show(null); branchesPage.show(null); archivePage.show(null); $('workspace-name').textContent='暂无工作区'; ['branch-count','mount-count','mounted-branch-count'].forEach(id => $(id).textContent='0'); $('mount-detail').textContent='暂无挂载点'; $('sync-status').textContent='—'; $('sync-detail').textContent='暂无工作区'; }
    else {
      const archive = isArchive();
      const data = await api(`/api/workspaces/${encodeURIComponent($('workspace').value)}${archive ? '/archive' : ''}`);
      if (request !== generation) return;
      if (archive) { $('workspace-name').textContent = data.workspace.name; archivePage.show(data); }
      else {
        snapshot = data; $('workspace-name').textContent = data.status.workspace.name;
        if (location.pathname === '/mounts') { mountsPage.show(data); await relationshipsView.show({...data.status, branches:data.branches}); }
        else if (location.pathname === '/branches') branchesPage.show(data);
        else render();
        if (request !== generation) return;
      }
    }
    const remoteError = !isArchive() && snapshot?.remote_error;
    $('error').hidden = !remoteError;
    if (remoteError) $('error').textContent = `远端信息刷新失败，当前显示上次同步结果：${remoteError.message}`;
    $('updated').textContent = `${new Date().toLocaleTimeString('zh-CN',{hour12:false})} 已刷新`;
    $('connection').textContent = '已连接本地 daemon';
  } catch (error) {
    if (request !== generation) return;
    $('error').textContent = error.message; $('error').hidden = false; $('login-error').textContent = error.message; $('connection').textContent = '连接需要检查';
  } finally { if (request === generation) { loading = false; $('refresh').disabled = false; } }
}
const settingsPage = createSettingsPage({api, element, $});
const revisionViewer = createRevisionViewer({api, element});
const branchFileViewer = createBranchFileViewer({api, element});
const mountsPage = createManagementPage({api, element, container:$('mounts-management'), kind:'mounts', changed:() => refresh(true), onBusy:value => relationshipsView.setBusy(value)});
const branchesPage = createManagementPage({api, element, container:$('branches-page'), kind:'branches', changed:() => refresh(true)});
const relationshipsView = createRelationshipsView({api, element, container:$('relationships-panel'), onSave:mount => mountsPage.saveMount(mount), onUnmount:mount => mountsPage.unmountMount(mount)});
const archivePage = createArchivePage({api, element, container:$('archive-page')});
function showBranch(branch) {
  if (branch.local) { branchFileViewer.show(snapshot.status.workspace.id, branch); return; }
  const revision = snapshot.revisions.find(value => value.id === (branch.autosave_head || branch.formal_head));
  if (revision) showRevision(revision, `branch-${branch.id}`);
}
function showRevision(revision, triggerId) {
  const branch = snapshot.branches.find(value => value.id === revision.branch);
  revisionViewer.show({workspace:snapshot.status.workspace.id, revision,
    message:revision.message || titles[revision.kind] || revision.kind,
    sourceBranch:branch?.name || short(revision.branch), isHead:revision.id === branch?.formal_head,
    triggerId});
}
const workspaceView = createWorkspaceView({element, selectBranch:chooseBranch, showBranchFiles:showBranch, showRevision});
function isArchive() { return location.pathname === '/archive' || location.pathname === '/'; }
function isSettings() { return location.pathname === '/settings'; }
async function refreshPage() {
  if (!token) return;
  if (isSettings()) await settingsPage.load();
  else await refresh();
}
function showPage() {
  generation++; loading = false; $('refresh').disabled = false;
  revisionViewer.clear(); branchFileViewer.clear(); archivePage.clear(); relationshipsView.clear(); mountsPage.clear(); branchesPage.clear();
  selectedBranch = new URLSearchParams(location.search).get('branch');
  const settings = isSettings(), archive = isArchive(), mounts = location.pathname === '/mounts', branches = location.pathname === '/branches';
  $('archive-page').hidden = !archive;
  $('history-page').hidden = settings || archive || mounts || branches;
  $('mounts-page').hidden = !mounts; $('branches-page').hidden = !branches;
  $('settings-page').hidden = !settings;
  $('history-breadcrumb').hidden = settings;
  $('settings-breadcrumb').hidden = !settings;
  for (const [id, active] of [['archive-nav', archive], ['history-nav', !settings && !archive && !mounts && !branches], ['mounts-nav', mounts], ['branches-nav', branches], ['settings-nav', settings]]) {
    $(id).classList.toggle('nav-active', active);
    if (active) $(id).setAttribute('aria-current', 'page');
    else $(id).removeAttribute('aria-current');
  }
  $('view-mode').textContent = settings ? '全局配置' : mounts || branches ? '工作区管理' : '只读浏览';
  $('workspace').closest('.workspace-picker').hidden = settings;
  document.title = settings ? 'AgentDrive · 设置' : archive ? 'AgentDrive · 档案室' : mounts ? 'AgentDrive · 挂载点管理' : branches ? 'AgentDrive · 分支管理' : 'AgentDrive · 分支与版本';
  refreshPage();
}
for (const id of ['archive-nav', 'history-nav', 'mounts-nav', 'branches-nav', 'settings-nav']) $(id).addEventListener('click', event => {
  if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault(); const url = new URL($(id).href); const current = new URLSearchParams(location.search);
  for (const key of ['workspace','branch']) if (current.has(key)) url.searchParams.set(key,current.get(key));
  history.pushState(null, '', url.pathname+url.search); showPage();
});
window.addEventListener('popstate', showPage);
$('login-form').addEventListener('submit', event => { event.preventDefault(); clearCommandTickets(); token=$('token').value.trim(); sessionStorage.setItem('agentdrive-token',token); $('token').value=''; settingsPage.clear(); $('login').hidden=true; $('app').hidden=false; showPage(); });
$('logout').addEventListener('click', () => { generation++; clearCommandTickets(); token=''; loading=false; sessionStorage.removeItem('agentdrive-token'); snapshot=null; settingsPage.clear(); revisionViewer.clear(); branchFileViewer.clear(); archivePage.clear(); relationshipsView.clear(); mountsPage.clear(); branchesPage.clear(); showLogin(); });
$('workspace').addEventListener('change', () => {
  const url = new URL(location.href); url.searchParams.set('workspace',$('workspace').value); url.searchParams.delete('branch'); url.searchParams.delete('path'); history.replaceState(null,'',url.pathname+url.search);
  selectedBranch=null; snapshot=null; revisionViewer.clear(); branchFileViewer.clear(); archivePage.clear(); relationshipsView.clear(); mountsPage.clear(); branchesPage.clear(); refresh(true);
});
$('refresh').addEventListener('click', async () => { await refreshPage(); if (isArchive()) archivePage.refresh(); });
if (location.pathname === '/') history.replaceState(null,'','/archive'+location.search);
showPage();
if (token) { $('login').hidden=true; $('app').hidden=false; } else showLogin();
setInterval(() => { if (!document.hidden) refreshPage(); },15000);
