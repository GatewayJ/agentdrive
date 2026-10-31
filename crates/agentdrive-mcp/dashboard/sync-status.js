export function workspaceSyncStatus(status) {
  const state = status.synchronization?.state || (status.capabilities.remote_configured ? 'unconfirmed' : 'disabled');
  const labels = {disabled:'未启用', unsaved:'有未保存修改', pending:'同步中', unconfirmed:'等待远端确认', confirmed:'已同步'};
  return {text:labels[state] || labels.unconfirmed, detail:state === 'disabled' ? '内容保存在本机' : state === 'confirmed' ? '本机写入分支的当前内容已获远端确认' : `${status.pending_jobs} 个待处理操作`};
}

export function branchSyncStatus(status, branch) {
  const state = status.synchronization?.branches.find(state => state.branch === branch.id);
  const saved = state?.local_saved ?? branch.mutation_seq === branch.saved_seq;
  const confirmed = !!state?.remote_confirmed;
  return {saved, confirmed, remote:!status.capabilities.remote_configured ? '未启用远程同步' : confirmed ? '远端已保存' : !saved ? '等待本机保存' : status.pending_jobs ? '同步处理中' : '等待远端确认'};
}
