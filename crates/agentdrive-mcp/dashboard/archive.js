import { createFileBrowser } from './file-browser.js';
import { branchSyncStatus } from './sync-status.js';

export function createArchivePage({api, element, container}) {
  const heading = element('div', 'page-heading archive-heading');
  const introduction = element('div');
  introduction.append(element('div','eyebrow','PROJECT FILES'), element('h1','','档案室'), element('p','','每个分支的最新成果，都在这里。查看资料，接着工作。'));
  const scope = element('span','archive-scope','当前项目 · 本机内容'); heading.append(introduction,scope);
  const empty = element('div','empty-state'); empty.hidden = true;
  empty.append(element('h2','','还没有可以浏览的项目'),element('p','','让 Agent 通过 MCP 创建或同步工作区，项目文件会显示在这里。'));
  const workspace = element('div','archive-workspace');
  const branchesPanel = element('section','archive-branches');
  const branchHeading = element('div','archive-list-heading');
  const branchCount = element('span','count-badge'); branchHeading.append(element('h2','','工作分支'),branchCount);
  const search = element('input','archive-branch-search'); search.type = 'search'; search.placeholder = '查找分支'; search.setAttribute('aria-label','查找分支');
  const branches = element('div','archive-branch-list'); branches.setAttribute('aria-label','选择要浏览的分支');
  const note = element('p','archive-branch-note','选择分支查看文件。Agent 正在使用的目录保持原有分支。');
  branchesPanel.append(branchHeading,search,branches,note);
  const content = element('section','archive-content');
  const branchHeader = element('div','archive-branch-header');
  const branchTitle = element('h2'); const branchOrigin = element('p'); const copy = element('div'); copy.append(branchTitle,branchOrigin);
  const history = element('a','archive-history-link','查看版本历史 ↗'); branchHeader.append(copy,history);
  const states = element('div','archive-states');
  const files = element('div','archive-files');
  content.append(branchHeader,states,files); workspace.append(branchesPanel,content);
  const footer = element('p','archive-footer','文件预览包含尚未提交的修改。异地接手前，请由 Agent 确认远端保存，并在目标机器拉取最新内容。');
  container.append(heading,empty,workspace,footer);
  let snapshot = null, selected = null, project = null, currentPath = '/';
  const browser = createFileBrowser({api, element, container:files, onNavigate:path => {
    currentPath = path;
    if (!selected || location.pathname !== '/archive') return;
    const url = new URL(location.href); url.searchParams.set('workspace',project); url.searchParams.set('branch',selected); url.searchParams.set('path',path);
    globalThis.history.replaceState(null,'',url.pathname+url.search);
  }});
  function status(branch) {
    return branchSyncStatus(snapshot, branch);
  }
  function renderBranches() {
    const focused = branches.contains(document.activeElement) ? document.activeElement.dataset.branch : null;
    const active = snapshot.branches.filter(branch => branch.state !== 'deleted');
    branchCount.textContent = active.length; branches.replaceChildren();
    const visible = active.filter(branch => branch.name.toLocaleLowerCase().includes(search.value.toLocaleLowerCase()))
      .sort((a,b) => Number(b.name === 'main')-Number(a.name === 'main') || a.name.localeCompare(b.name));
    for (const branch of visible) {
      const button = element('button',`archive-branch${branch.id === selected ? ' active' : ''}`); button.type = 'button'; button.dataset.branch = branch.id;
      button.setAttribute('aria-pressed',String(branch.id === selected));
      const line = element('span','archive-branch-name'); line.append(element('span','archive-branch-symbol','⑂'),element('span','',branch.name));
      const mounts = snapshot.mounts.filter(mount => mount.branch === branch.id && mount.state === 'ready').length;
      const meta = element('span','archive-branch-meta');
      meta.append(element('span',status(branch).saved ? '' : 'archive-warning',status(branch).saved ? branch.dirty ? '已保存 · 未提交' : '已提交' : '有未保存修改'));
      if (mounts) meta.append(element('span','archive-mount',`${mounts} 个目录使用中`));
      button.append(line,meta); button.onclick = () => select(branch.id,'/'); branches.append(button);
    }
    if (focused) [...branches.children].find(button => button.dataset.branch === focused)?.focus({preventScroll:true});
    if (!visible.length) branches.append(element('p','panel-empty','没有匹配的分支。'));
  }
  function renderContext(branch) {
    branchTitle.textContent = branch.name;
    const source = snapshot.branches.find(value => value.id === branch.source_branch);
    branchOrigin.textContent = source ? `来自 ${source.name} · 当前工作内容` : '项目主线 · 当前工作内容';
    history.href = '/dashboard?'+new URLSearchParams({workspace:project,branch:branch.id});
    const state = status(branch);
    const mounted = snapshot.mounts.filter(mount => mount.branch === branch.id && mount.state === 'ready');
    const card = (label,value,description,warning=false) => {
      const block = element('div',`archive-state${warning ? ' attention' : ''}`);
      block.append(element('span','archive-state-label',label),element('strong','',value),element('span','archive-state-description',description));
      return block;
    };
    states.replaceChildren(
      card('本地进度',state.saved ? '本机已保存' : '有未保存修改',!state.saved ? '正在等待本机保存' : branch.dirty ? '工作内容尚未正式提交' : '工作内容已提交',!state.saved),
      card('远程保存',state.remote,state.confirmed ? '与本机内容一致 · 上次确认' : snapshot.capabilities.remote_configured ? '交接前确认远端保存结果' : '可在设置中连接 S3 或 OpenCSG',snapshot.capabilities.remote_configured && !state.confirmed),
      card('工作目录',mounted.length ? `${mounted.length} 个目录使用中` : '当前未挂载',branch.owner === snapshot.location ? '由本机管理此分支' : '本机读取副本 · 同步后更新')
    );
    if (mounted.length) states.lastChild.title = mounted.map(mount => mount.path).join('\n');
  }
  function select(id,path='/') {
    const branch = snapshot.branches.find(value => value.id === id && value.state !== 'deleted');
    if (!branch) return;
    selected = id; renderBranches(); renderContext(branch); browser.show(project,branch,path);
  }
  function show(status) {
    const previousProject = project;
    snapshot = status; project = status?.workspace.id || null;
    const active = status?.branches.filter(branch => branch.state !== 'deleted') || [];
    empty.hidden = active.length > 0; workspace.hidden = !active.length; footer.hidden = !active.length;
    if (!active.length) { selected = null; browser.clear(); return; }
    if (previousProject !== project) { selected = null; search.value = ''; }
    const query = new URLSearchParams(location.search);
    const requested = query.get('workspace') === project ? query.get('branch') : null;
    const id = active.find(branch => branch.id === requested)?.id || active.find(branch => branch.id === selected)?.id || active.find(branch => branch.id === status.selected_branch)?.id || active[0].id;
    const path = query.get('workspace') === project && requested === id ? query.get('path') || '/' : '/';
    if (selected !== id || path !== currentPath) select(id,path);
    else { renderBranches(); renderContext(active.find(branch => branch.id === id)); browser.update(project,active); }
  }
  search.oninput = renderBranches;
  function clear() { snapshot = null; selected = null; project = null; browser.clear(); branches.replaceChildren(); states.replaceChildren(); branchTitle.textContent = ''; branchOrigin.textContent = ''; workspace.hidden = true; empty.hidden = true; footer.hidden = true; }
  return {show,clear,refresh:browser.refresh};
}
