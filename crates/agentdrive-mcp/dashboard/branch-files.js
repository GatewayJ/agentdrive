import { createFileBrowser } from './file-browser.js';

export function createBranchFileViewer({api, element}) {
  const dialog = element('dialog', 'revision-dialog branch-files-dialog');
  dialog.id = 'branch-files-dialog'; dialog.setAttribute('aria-labelledby', 'branch-files-title');
  const heading = element('div', 'revision-dialog-heading');
  const title = element('h2', '', '分支文件'); title.id = 'branch-files-title';
  const subtitle = element('p', 'revision-dialog-subtitle');
  const copy = element('div'); copy.append(title, subtitle);
  const close = element('button', 'revision-dialog-close', '×'); close.type = 'button';
  close.autofocus = true; close.setAttribute('aria-label', '关闭分支文件'); heading.append(copy, close);
  const container = element('div'); dialog.append(heading, container); document.body.append(dialog);
  const browser = createFileBrowser({api, element, container});
  let context = null;
  function clear(restoreFocus = false) {
    const trigger = context && document.getElementById(`branch-${context.branch.id}`);
    context = null; browser.clear();
    if (dialog.open) dialog.close();
    if (restoreFocus) trigger?.focus();
  }
  function show(workspace, branch) {
    clear(); context = {workspace, branch}; subtitle.textContent = `${branch.name} · 当前文件`;
    dialog.showModal(); dialog.scrollTop = 0; browser.show(workspace, branch);
  }
  function update(workspace, branches) {
    if (!context) return;
    const branch = branches.find(value => value.id === context.branch.id && value.state !== 'deleted');
    if (workspace !== context.workspace || !branch) { clear(); return; }
    subtitle.textContent = `${branch.name} · 当前文件`; browser.update(workspace, branches);
  }
  close.onclick = () => clear(true);
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
