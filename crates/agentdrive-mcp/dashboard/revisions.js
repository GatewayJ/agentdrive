import { createRevisionChanges } from './changes.js';

export function createRevisionViewer({api, element}) {
  const menu = element('div', 'revision-menu');
  menu.id = 'revision-menu'; menu.hidden = true;
  menu.setAttribute('role', 'menu'); menu.setAttribute('aria-label', '版本操作');
  const dialog = element('dialog', 'revision-dialog');
  dialog.setAttribute('aria-labelledby', 'revision-dialog-title');
  const heading = element('div', 'revision-dialog-heading');
  const title = element('h2'); title.id = 'revision-dialog-title';
  const subtitle = element('p', 'revision-dialog-subtitle');
  const copy = element('div'); copy.append(title, subtitle);
  const close = element('button', 'revision-dialog-close', '×');
  close.type = 'button'; close.autofocus = true; close.setAttribute('aria-label', '关闭弹框');
  heading.append(copy, close);
  const detail = element('div', 'revision-detail'); detail.hidden = true;
  const changes = element('section', 'revision-changes'); changes.hidden = true;
  dialog.append(heading, detail, changes); document.body.append(menu, dialog);
  const revisionChanges = createRevisionChanges({api, element, container:changes});
  let menuContext = null, dialogContext = null;
  const triggerId = context => `revision-more-${context.revision.id}`;
  const trigger = context => context && document.getElementById(triggerId(context));

  function closeMenu(restoreFocus = false) {
    const button = trigger(menuContext);
    menu.hidden = true; menuContext = null;
    button?.setAttribute('aria-expanded', 'false');
    if (restoreFocus) button?.focus();
  }
  function closeDialog(restoreFocus = true) {
    const button = trigger(dialogContext);
    dialogContext = null; revisionChanges.clear(); detail.replaceChildren();
    if (dialog.open) dialog.close();
    if (restoreFocus) button?.focus();
  }
  function clear() { closeMenu(); closeDialog(false); }
  function positionMenu() {
    const button = trigger(menuContext);
    if (!button) { closeMenu(); return; }
    const rect = button.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(rect.right - menu.offsetWidth, innerWidth - menu.offsetWidth - 8))}px`;
    const top = rect.bottom + 6;
    menu.style.top = `${Math.max(8, top + menu.offsetHeight <= innerHeight - 8 ? top : rect.top - menu.offsetHeight - 6)}px`;
  }
  function showDialog(kind) {
    const context = menuContext; closeMenu();
    if (!context) return;
    closeDialog(false); dialogContext = context;
    const {workspace, revision, message, sourceBranch, isHead} = context;
    title.textContent = kind === 'info' ? '版本信息' : '修改内容';
    subtitle.textContent = `${revision.id.slice(0, 8)} · ${message}`;
    dialog.classList.toggle('revision-dialog-changes', kind === 'changes');
    detail.hidden = kind !== 'info';
    if (kind === 'info') {
      const grid = element('dl', 'detail-grid');
      const fields = [
        ['版本 ID', revision.id], ['说明', message], ['创建者', revision.actor],
        ['保存时间', new Date(revision.created_ns / 1e6).toLocaleString('zh-CN', {hour12:false})],
        ['父版本', revision.parents.length ? revision.parents.join(' / ') : '初始版本'],
        ['内容对象', revision.root.id], ['来源分支', sourceBranch], ['保存状态', '本地已保存'],
      ];
      if (isHead) fields.push(['分支位置', '当前 commit']);
      for (const [label, value] of fields) grid.append(element('dt', '', label), element('dd', '', value));
      detail.append(grid);
    } else revisionChanges.show(workspace, revision);
    dialog.showModal();
  }
  for (const [kind, label] of [['info', '版本信息'], ['changes', '修改内容']]) {
    const item = element('button', 'revision-menu-item', label);
    item.type = 'button'; item.tabIndex = -1; item.setAttribute('role', 'menuitem');
    item.onclick = () => showDialog(kind); menu.append(item);
  }
  function openMenu(context, index = 0) {
    closeMenu(); menuContext = context; menu.hidden = false;
    trigger(context)?.setAttribute('aria-expanded', 'true');
    positionMenu(); menu.children[index].focus();
  }
  function button(context) {
    const more = element('button', 'revision-more', '⋯');
    more.id = triggerId(context); more.type = 'button'; more.title = '更多';
    more.setAttribute('aria-label', `版本 ${context.revision.id.slice(0, 8)} 的更多操作`);
    more.setAttribute('aria-haspopup', 'menu'); more.setAttribute('aria-expanded', 'false');
    more.setAttribute('aria-controls', menu.id);
    more.onclick = () => menuContext?.revision.id === context.revision.id ? closeMenu(true) : openMenu(context);
    more.onkeydown = event => {
      if (!['ArrowDown', 'ArrowUp'].includes(event.key)) return;
      event.preventDefault(); openMenu(context, event.key === 'ArrowUp' ? 1 : 0);
    };
    return more;
  }
  function update(workspace, revisions) {
    const exists = context => context?.workspace === workspace && revisions.some(revision => revision.id === context.revision.id);
    if (dialogContext && !exists(dialogContext)) closeDialog(false);
    if (menuContext) {
      if (!exists(menuContext)) closeMenu();
      else { trigger(menuContext)?.setAttribute('aria-expanded', 'true'); positionMenu(); }
    }
  }
  menu.onkeydown = event => {
    const items = [...menu.children], current = items.indexOf(document.activeElement);
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      const index = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (current + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length;
      items[index].focus();
    } else if (event.key === 'Escape') { event.preventDefault(); closeMenu(true); }
    else if (event.key === 'Tab') closeMenu(true);
  };
  document.addEventListener('pointerdown', event => {
    if (menuContext && !menu.contains(event.target) && event.target !== trigger(menuContext)) closeMenu();
  });
  document.addEventListener('focusin', event => {
    if (menuContext && !menu.contains(event.target) && event.target !== trigger(menuContext)) closeMenu();
  });
  window.addEventListener('resize', () => { if (menuContext) positionMenu(); });
  document.addEventListener('scroll', event => { if (menuContext && !menu.contains(event.target)) closeMenu(true); }, true);
  close.onclick = () => closeDialog();
  dialog.addEventListener('cancel', event => { event.preventDefault(); closeDialog(); });
  let backdropPressed = false;
  const outsideDialog = event => {
    const rect = dialog.getBoundingClientRect();
    return event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom);
  };
  dialog.addEventListener('pointerdown', event => { backdropPressed = outsideDialog(event); });
  dialog.addEventListener('click', event => { if (backdropPressed && outsideDialog(event)) closeDialog(); backdropPressed = false; });
  return {button, update, clear};
}
