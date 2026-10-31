export function createCommandPanel({api, element, container, scope, changed, setBusy}) {
  const panel = element('section', 'panel command-panel'); panel.hidden = true;
  panel.setAttribute('aria-label', '操作进度');
  const title = element('h2'), status = element('p', 'command-status'); status.setAttribute('role', 'status');
  const flags = element('div', 'command-flags'), error = element('p', 'command-error'); error.setAttribute('role', 'alert');
  const actions = element('div', 'management-actions');
  const retry = element('button', 'secondary', '重试请求'), check = element('button', 'secondary', '检查结果');
  const release = element('button', 'secondary', '已关闭占用，继续操作');
  const detail = element('details'), details = element('code', 'command-details'); detail.append(element('summary', '', '操作详情'), details);
  actions.append(retry, check, release); panel.append(title, status, flags, error, actions, detail); container.append(panel);
  let workspace = null, ticket = null, timer = null, generation = 0, running = false;
  const key = () => `agentdrive-action:${scope}:${workspace}`;
  const post = (tool, body) => api(`/api/commands/${tool}`, {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(body)});
  const complete = record => record && !record.error && !record.pending &&
    (record.phase === 'complete' || record.local_saved && ['local_saved','uploading'].includes(record.phase)) && record.mount_ready !== false &&
    (ticket.request.durability !== 'remote' || record.remote_confirmed);
  function persist() { if (workspace && ticket) sessionStorage.setItem(key(), JSON.stringify(ticket)); }
  function render() {
    panel.hidden = !ticket;
    const record = ticket?.record;
    const recovery = record?.recovery;
    const terminal = complete(record) || record && !recovery && ['failed','cancelled'].includes(record.phase);
    setBusy(running || !!ticket && !terminal && !ticket.rejected);
    if (!ticket) return;
    title.textContent = ticket.label;
    const labels = {reserved:'正在准备',saving:'正在保存',local_saved:'本机已保存',uploading:'正在同步',
      waiting_for_quiesce:'需要释放目录占用',committed_mount_pending:'内容已更新，挂载等待恢复',
      recovery_required:'操作需要恢复',failed:'操作失败',cancelled:'操作已取消'};
    status.textContent = complete(record) ? record.phase === 'complete' ? '操作已完成' : '本机操作已完成' : running ? '正在处理…' :
      record?.phase === 'complete' ? '等待远端保存确认' : labels[record?.phase] || '结果尚未确认，可以检查结果或重试原请求。';
    flags.replaceChildren();
    if (record) {
      flags.append(element('span', 'pill', record.local_saved ? '本机已保存' : '等待本机保存'));
      if (ticket.request.durability === 'remote') flags.append(element('span', 'pill', record.remote_confirmed ? '远端已确认' : '等待远端确认'));
      if (record.mount_ready != null) flags.append(element('span', 'pill', record.mount_ready ? '挂载已就绪' : '挂载未就绪'));
    }
    error.textContent = ticket.message || record?.error?.message || '';
    if (record?.phase === 'waiting_for_quiesce') error.textContent += ' 请关闭使用该目录的文件，让终端退出该目录后继续。';
    retry.hidden = !!record && (complete(record) || recovery?.kind === 'release_mount' ||
      recovery?.kind !== 'retry_request' && (!record.error?.retryable || terminal));
    check.hidden = !record && !!ticket.rejected;
    const legacyRelease = record && !('recovery' in record) && record.phase === 'waiting_for_quiesce' && ticket.tool !== 'branch_switch' && record.pending?.old;
    release.hidden = recovery?.kind !== 'release_mount' && !legacyRelease;
    release.textContent = record?.phase === 'waiting_for_quiesce' ? '已关闭占用，继续操作' : '恢复挂载';
    for (const button of [retry,check,release]) button.disabled = running;
    details.textContent = `request_id: ${ticket.request_id}${record ? `\noperation: ${record.id}\nphase: ${record.phase}` : ''}`;
  }
  function schedule() {
    clearTimeout(timer);
    const record = ticket?.record;
    if (record && !ticket.message && !complete(record) && !record.error && !['failed','cancelled','waiting_for_quiesce','recovery_required','committed_mount_pending'].includes(record.phase)) {
      timer = setTimeout(() => execute('check'), 2000);
    }
  }
  async function execute(mode = 'submit') {
    if (running || !ticket) return;
    const request = ++generation, active = ticket;
    running = true; active.message = ''; render();
    try {
      let record;
      if (mode === 'release') {
        active.release_request_id ||= crypto.randomUUID();
        active.release_request ||= {operation:active.record.id,
          expected_binding_generation:active.record.recovery?.expected_binding_generation ?? active.record.pending.old.generation};
        persist();
        const response = await post('mount_release', {request_id:active.release_request_id, request:active.release_request});
        if (request !== generation) return;
        record = await post('operation_get', {operation:active.record.id});
        if (response.error) active.message = response.error.message;
      } else if (mode === 'check') {
        record = await post(active.record ? 'operation_get' : 'operation_by_request', active.record ? {operation:active.record.id} : {request_id:active.request_id});
        if (!record) throw new Error('服务尚未记录此请求，可以重试原请求。');
      } else {
        record = await post(active.tool, {request_id:active.request_id, request:active.request});
      }
      if (request !== generation) return;
      active.record = record; active.rejected = false; persist();
      if (record.local_saved || complete(record)) await changed();
    } catch (failure) {
      if (request !== generation) return;
      active.message = failure.message;
      active.rejected = mode === 'submit' && failure.status >= 400 && failure.status < 500;
      persist();
    } finally {
      if (request === generation) { running = false; render(); schedule(); }
    }
  }
  function clear() { generation++; clearTimeout(timer); running = false; workspace = null; ticket = null; render(); }
  function show(id) {
    if (id === workspace) return;
    clear(); workspace = id;
    if (id) {
      try { ticket = JSON.parse(sessionStorage.getItem(key())); } catch { ticket = null; }
      render(); schedule();
      if (ticket?.record && !('recovery' in ticket.record) && ticket.record.pending) execute('check');
    }
  }
  async function run(tool, request, label) {
    if (running) return;
    ticket = {tool, request, label, request_id:crypto.randomUUID()}; persist();
    await execute();
  }
  retry.onclick = () => execute(); check.onclick = () => execute('check'); release.onclick = () => execute('release');
  return {show, clear, run};
}
