export function createSettingsPage({api, element, $}) {
  let current = null;
  let dirty = false;
  let busy = false;
  let generation = 0;
  const mib = 1024 * 1024;
  function message(id, text) { $(id).textContent = text || ''; $(id).hidden = !text; }
  function state() {
    $('settings-save').disabled = busy || !dirty;
    $('settings-reload').disabled = busy;
    $('settings-fields').disabled = busy;
    $('settings-state').textContent = busy ? '正在处理…' : dirty ? '有尚未保存的修改' : '当前配置已生效';
    $('settings-reload').textContent = dirty ? '重新载入并放弃修改' : '重新载入';
  }
  function changed() { dirty = true; message('settings-notice', ''); state(); }
  function inputField(card, key, label, value, type = 'text') {
    const wrapper = element('label', '', label);
    const input = element(type === 'textarea' ? 'textarea' : 'input');
    input.dataset.field = key;
    if (type !== 'textarea') input.type = type;
    if (type === 'number') { input.min = '1'; input.step = '1'; }
    input.required = !['arguments'].includes(key);
    input.value = value;
    if (type === 'textarea') input.rows = 3;
    wrapper.append(input); card.append(wrapper);
    return input;
  }
  function addValidation(name = '', config = {image:'', program:'', arguments:[], timeout_seconds:60, max_output_bytes:65536}) {
    const card = element('div', 'validation-card');
    const heading = element('div', 'validation-card-heading');
    heading.append(element('strong', '', name || '新验证任务'));
    const remove = element('button', 'text-button', '移除任务'); remove.type = 'button';
    remove.addEventListener('click', () => { card.remove(); $('validation-empty').hidden = $('validation-list').children.length > 0; changed(); });
    heading.append(remove); card.append(heading);
    const fields = element('div', 'validation-fields');
    const nameInput = inputField(fields, 'name', '任务名称', name); nameInput.maxLength = 128;
    inputField(fields, 'image', '容器镜像', config.image);
    inputField(fields, 'program', '容器内程序', config.program);
    inputField(fields, 'timeout_seconds', '超时时间（秒）', config.timeout_seconds, 'number').max = '3600';
    inputField(fields, 'max_output_bytes', '输出上限（字节）', config.max_output_bytes, 'number').max = '2097152';
    inputField(fields, 'arguments', '程序参数，每行一个', config.arguments.join('\n'), 'textarea');
    card.append(fields); $('validation-list').append(card); $('validation-empty').hidden = true;
    return nameInput;
  }
  function render(data) {
    current = data; dirty = false;
    $('settings-form').hidden = false; $('startup-panel').hidden = false;
    $('cache-max').value = data.runtime.cache.max_bytes / mib;
    $('cache-target').value = data.runtime.cache.target_bytes / mib;
    $('cache-free').value = data.runtime.cache.min_free_bytes / mib;
    $('allowed-origins').value = data.runtime.allowed_origins.join('\n');
    $('validation-list').replaceChildren(); $('validation-empty').hidden = false;
    for (const [name, config] of Object.entries(data.runtime.validation)) addValidation(name, config);
    const values = $('startup-values'); values.replaceChildren();
    const startup = data.startup;
    const fields = [
      ['配置文件', data.config_path], ['监听地址', startup.listen], ['存储目录', startup.data_dir],
      ['挂载目录', startup.mount_roots.join('\n') || '未配置'], ['导入导出目录', (startup.directory_roots || []).join('\n') || '未配置'],
      ['认证身份', startup.credentials.map(value => value.principal).join('、')],
      ['设置管理员', (startup.administrators || ['owner']).join('、') || '未配置'], ['Docker 程序', startup.docker || 'docker'],
      ['远程存储', startup.remote ? `${startup.remote.endpoint || 'AWS S3'}\n${startup.remote.bucket} / ${startup.remote.prefix}` : '本地存储'],
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
    function bytes(id) {
      const value = Number($(id).value) * mib;
      if (!Number.isSafeInteger(value) || value < 0) throw new Error('容量必须是有效的字节整数。');
      return value;
    }
    const cache = {max_bytes:bytes('cache-max'), target_bytes:bytes('cache-target'), min_free_bytes:bytes('cache-free')};
    if (cache.target_bytes >= cache.max_bytes) throw new Error('清理目标必须小于容量上限。');
    const validation = Object.create(null);
    for (const card of $('validation-list').children) {
      const fields = Object.fromEntries([...card.querySelectorAll('[data-field]')].map(input => [input.dataset.field, input.value]));
      const name = fields.name.trim();
      if (!name || Object.hasOwn(validation, name)) throw new Error('验证任务名称不能为空或重复。');
      validation[name] = {image:fields.image.trim(), program:fields.program.trim(), arguments:fields.arguments === '' ? [] : fields.arguments.split('\n'), timeout_seconds:Number(fields.timeout_seconds), max_output_bytes:Number(fields.max_output_bytes)};
    }
    return {cache, validation, allowed_origins:[...new Set($('allowed-origins').value.split('\n').map(value => value.trim()).filter(Boolean))]};
  }
  $('settings-form').addEventListener('input', changed);
  $('add-validation').addEventListener('click', () => { addValidation().focus(); changed(); });
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
      if (request === generation) message('settings-error', error.status === 409 ? '配置已经更新，保存未覆盖现有内容。请重新载入后修改。' : error.message);
    } finally { if (request === generation) { busy = false; state(); } }
  });
  return {
    load,
    clear() { generation++; current = null; dirty = false; busy = false; $('settings-form').reset(); $('settings-form').hidden = true; $('startup-panel').hidden = true; ['settings-error','settings-notice','reload-error'].forEach(id => message(id, '')); state(); },
  };
}
