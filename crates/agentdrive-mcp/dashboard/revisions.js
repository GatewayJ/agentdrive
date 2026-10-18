import { createRevisionChanges } from './changes.js';

export function createRevisionViewer({api, element}) {
  const dialog = element('dialog', 'revision-dialog');
  dialog.id = 'revision-dialog'; dialog.setAttribute('aria-labelledby', 'revision-dialog-title');
  const heading = element('div', 'revision-dialog-heading');
  const title = element('h2', '', '版本详情'); title.id = 'revision-dialog-title';
  const subtitle = element('p', 'revision-dialog-subtitle');
  const copy = element('div'); copy.append(title, subtitle);
  const close = element('button', 'revision-dialog-close', '×');
  close.type = 'button'; close.autofocus = true; close.setAttribute('aria-label', '关闭弹框');
  heading.append(copy, close);
  const detail = element('section', 'revision-detail');
  const changesSection = element('section', 'revision-changes-section');
  const changes = element('div', 'revision-changes');
  changesSection.append(element('h3', 'revision-section-title', '修改内容'), changes);
  dialog.append(heading, detail, changesSection); document.body.append(dialog);
  const revisionChanges = createRevisionChanges({api, element, container:changes});
  let dialogContext = null;
  const triggerId = context => context.triggerId || `revision-${context.revision.id}`;
  const trigger = context => context && document.getElementById(triggerId(context));

  function closeDialog(restoreFocus = true) {
    const button = trigger(dialogContext);
    dialogContext = null; revisionChanges.clear(); detail.replaceChildren();
    if (dialog.open) dialog.close();
    if (restoreFocus) button?.focus();
  }
  function clear() { closeDialog(false); }
  function show(context) {
    closeDialog(false); dialogContext = context;
    const {workspace, revision, message, sourceBranch, isHead} = context;
    subtitle.textContent = `${revision.id.slice(0, 8)} · ${message}`;
    const grid = element('dl', 'detail-grid');
    const fields = [
      ['版本 ID', revision.id], ['说明', message], ['创建者', revision.actor],
      ['保存时间', new Date(revision.created_ns / 1e6).toLocaleString('zh-CN', {hour12:false})],
      ['父版本', revision.parents.length ? revision.parents.join(' / ') : '初始版本'],
      ['内容对象', revision.root.id], ['来源分支', sourceBranch], ['保存状态', '本地已保存'],
    ];
    if (isHead) fields.push(['分支位置', '当前 commit']);
    for (const [label, value] of fields) {
      const field = element('div', 'detail-field');
      field.append(element('dt', '', label), element('dd', '', value)); grid.append(field);
    }
    detail.append(element('h3', 'revision-section-title', '版本信息'), grid);
    revisionChanges.show(workspace, revision);
    dialog.showModal(); dialog.scrollTop = 0;
  }
  function bind(row, context) {
    row.id = triggerId(context); row.type = 'button'; row.title = '双击查看版本信息和修改内容';
    row.setAttribute('aria-label', `版本 ${context.revision.id.slice(0, 8)}：${context.message}，查看版本详情`);
    row.setAttribute('aria-haspopup', 'dialog'); row.setAttribute('aria-controls', dialog.id);
    row.ondblclick = () => show(context);
    row.onclick = event => { if (event.detail === 0) show(context); };
  }
  function update(workspace, revisions) {
    if (dialogContext && (dialogContext.workspace !== workspace || !revisions.some(revision => revision.id === dialogContext.revision.id))) closeDialog(false);
  }
  close.onclick = () => closeDialog();
  dialog.addEventListener('cancel', event => { event.preventDefault(); closeDialog(); });
  let backdropPressed = false;
  const outsideDialog = event => {
    const rect = dialog.getBoundingClientRect();
    return event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom);
  };
  dialog.addEventListener('pointerdown', event => { backdropPressed = outsideDialog(event); });
  dialog.addEventListener('click', event => { if (backdropPressed && outsideDialog(event)) closeDialog(); backdropPressed = false; });
  return {bind, show, update, clear};
}
