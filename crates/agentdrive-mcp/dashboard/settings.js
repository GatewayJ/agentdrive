export function createSettingsPage({api, element, $}) {
  let current = null;
  let dirty = false;
  let busy = false;
  let generation = 0;
  const gib = 1024 * 1024 * 1024;
  function message(id, text) { $(id).textContent = text || ''; $(id).hidden = !text; }
  function state() {
    $('settings-save').disabled = busy || !dirty;
    $('settings-reload').disabled = busy;
    $('settings-fields').disabled = busy;
    $('settings-state').textContent = busy ? '正在处理，存储切换期间需要传输历史内容…' : dirty ? '有尚未保存的修改' : '当前配置已生效';
    $('settings-reload').textContent = dirty ? '重新载入并放弃修改' : '重新载入';
  }
  function changed() { dirty = true; storageFields(); message('settings-notice', ''); state(); }
  function render(data) {
    current = data; dirty = false;
    $('settings-form').hidden = false; $('startup-panel').hidden = false;
    $('cache-max').value = data.runtime.cache.max_bytes / gib;
    $('allowed-origins').value = data.runtime.allowed_origins.join('\n');
    const remote = data.runtime.remote;
    const backend = remote?.backend || 's3';
    $('storage-mode').value = remote?.enabled !== false && remote ? backend : 'local';
    const s3 = backend === 's3' ? remote : null;
    const opencsg = backend === 'opencsg' ? remote : null;
    $('s3-endpoint').value = s3?.endpoint || '';
    $('s3-bucket').value = s3?.bucket || '';
    $('s3-region').value = s3?.region || 'us-east-1';
    $('s3-prefix').value = s3?.prefix || 'agentdrive/v1';
    $('s3-http').checked = s3?.allow_http || false;
    $('s3-auth').value = s3?.credentials ? 'saved' : 'environment';
    $('s3-access-key').value = s3?.credentials?.access_key_id || '';
    $('s3-secret-key').value = ''; $('s3-session-token').value = '';
    $('opencsg-endpoint').value = opencsg?.endpoint || '';
    $('opencsg-repository').value = opencsg?.repository || '';
    $('opencsg-branch').value = opencsg?.branch || 'main';
    $('opencsg-prefix').value = opencsg?.prefix || 'agentdrive/v1';
    $('opencsg-http').checked = opencsg?.allow_http || false;
    $('opencsg-token').value = '';
    storageFields();
    const values = $('startup-values'); values.replaceChildren();
    const startup = data.startup;
    const fields = [
      ['配置文件', data.config_path], ['监听地址', startup.listen], ['存储目录', startup.data_dir],
      ['挂载目录', startup.mount_roots.join('\n') || '未配置'], ['导入导出目录', (startup.directory_roots || []).join('\n') || '未配置'],
      ['认证身份', startup.credentials.map(value => value.principal).join('、')],
      ['设置管理员', (startup.administrators || ['owner']).join('、') || '未配置'],
    ];
    for (const [label, value] of fields) values.append(element('dt', '', label), element('dd', '', value));
    $('settings-version').textContent = `配置版本 ${data.revision.slice(0, 12)}`;
    message('reload-error', data.reload_error ? `配置文件提示：${data.reload_error}` : '');
    state();
  }
  async function load(discard = false) {
    if (busy) return;
    busy = true; state();
    const request = ++generation;
    try {
      const data = await api('/api/settings');
      if (request !== generation) return;
      if (!dirty || discard) { render(data); message('settings-notice', ''); }
      else {
        message('reload-error', data.reload_error ? `配置文件提示：${data.reload_error}` : '');
        if (current?.revision !== data.revision) message('settings-notice', '配置已被其他操作更新。当前编辑内容已保留，请重新载入后修改。');
      }
      message('settings-error', '');
      $('updated').textContent = `${new Date().toLocaleTimeString('zh-CN', {hour12:false})} 已刷新`;
      $('connection').textContent = '已连接本地 daemon';
    } catch (error) {
      if (request !== generation) return;
      message('settings-error', error.status === 403 ? '当前身份没有设置管理权限。管理员由配置文件中的 administrators 指定，默认是 owner。' : error.message);
      $('updated').textContent = error.status === 403 ? '需要管理员权限' : '刷新失败';
      if (error.status === 403 || error.status === 401) { current = null; dirty = false; $('settings-form').hidden = true; $('startup-panel').hidden = true; }
    } finally { if (request === generation) { busy = false; state(); } }
  }
  function runtime() {
    const mode = $('storage-mode').value;
    const enabled = mode !== 'local';
    const maxBytes = enabled ? Math.round(Number($('cache-max').value) * gib) : current.runtime.cache.max_bytes;
    if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) throw new Error('本地缓存容量必须大于零，且不能超过浏览器支持的整数范围。');
    const cache = {max_bytes:maxBytes};
    let remote = current.runtime.remote ? {...current.runtime.remote, enabled:false} : null;
    if (mode === 's3') {
      const bucket = $('s3-bucket').value.trim();
      if (!bucket) throw new Error('请填写 S3 Bucket。');
      remote = {
      backend:'s3', enabled:true, bucket, endpoint:$('s3-endpoint').value.trim() || null,
      region:$('s3-region').value.trim(), prefix:$('s3-prefix').value.trim(), allow_http:$('s3-http').checked,
      credentials:$('s3-auth').value === 'saved' ? {
        access_key_id:$('s3-access-key').value.trim(), secret_access_key:$('s3-secret-key').value,
        session_token:$('s3-session-token').value || null,
      } : null,
      };
    } else if (mode === 'opencsg') {
      remote = {backend:'opencsg', enabled:true, endpoint:$('opencsg-endpoint').value.trim(),
        repository:$('opencsg-repository').value.trim(), branch:$('opencsg-branch').value.trim(),
        prefix:$('opencsg-prefix').value.trim(), token:$('opencsg-token').value, allow_http:$('opencsg-http').checked};
      if (!remote.endpoint || !remote.repository) throw new Error('请填写 OpenCSG 域名和仓库。');
    }
    return {cache, remote, allowed_origins:[...new Set($('allowed-origins').value.split('\n').map(value => value.trim()).filter(Boolean))]};
  }
  function storageFields() {
    const mode = $('storage-mode').value;
    const enabled = mode !== 'local';
    $('cache-panel').hidden = !enabled;
    $('cache-max').disabled = !enabled;
    $('cache-max').required = enabled;
    $('s3-fields').hidden = mode !== 's3';
    $('s3-fields').disabled = mode !== 's3';
    $('opencsg-fields').hidden = mode !== 'opencsg';
    $('opencsg-fields').disabled = mode !== 'opencsg';
    $('s3-bucket').required = mode === 's3';
    const saved = mode === 's3' && $('s3-auth').value === 'saved';
    for (const id of ['s3-access-key', 's3-secret-key', 's3-session-token']) $(id).disabled = !saved;
    $('s3-access-key').required = saved;
    for (const name of ['endpoint','repository','branch','prefix']) $('opencsg-'+name).required = mode === 'opencsg';
    const previous = current?.runtime.remote;
    const retained = previous?.backend === 'opencsg' && previous.endpoint === $('opencsg-endpoint').value.trim()
      && previous.repository === $('opencsg-repository').value.trim();
    $('opencsg-token').required = mode === 'opencsg' && !retained;
    $('opencsg-token').placeholder = retained ? '留空保留已保存 token' : '请输入仓库访问 token';
  }
  $('storage-mode').addEventListener('change', storageFields);
  $('s3-auth').addEventListener('change', storageFields);
  $('settings-form').addEventListener('input', changed);
  $('settings-reload').addEventListener('click', () => load(true));
  $('settings-form').addEventListener('submit', async event => {
    event.preventDefault();
    if (busy || !current) return;
    const request = ++generation;
    try {
      const update = {revision:current.revision, runtime:runtime()};
      busy = true; state(); message('settings-error', '');
      const data = await api('/api/settings', {method:'PUT', headers:{'Content-Type':'application/json'}, body:JSON.stringify(update)});
      if (request !== generation) return;
      render(data); message('settings-notice', '配置已保存并生效。');
    } catch (error) {
      if (request === generation) message('settings-error', error.message);
    } finally { if (request === generation) { busy = false; state(); } }
  });
  return {
    load,
    clear() { generation++; current = null; dirty = false; busy = false; $('settings-form').reset(); $('settings-form').hidden = true; $('startup-panel').hidden = true; ['settings-error','settings-notice','reload-error'].forEach(id => message(id, '')); state(); },
  };
}
