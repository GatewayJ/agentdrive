const {test, before, after} = require('node:test');
const assert = require('node:assert/strict');
const {createServer} = require('node:http');
const {readFile} = require('node:fs/promises');
const path = require('node:path');
const {chromium} = require('playwright');

let server, browser, endpoint;
before(async () => {
  const directory = path.resolve(__dirname, '../../crates/agentdrive-mcp/dashboard');
  server = createServer(async (request, response) => {
    const pathname = new URL(request.url, 'http://localhost').pathname;
    const name = ['/', '/dashboard', '/archive', '/settings', '/mounts', '/branches'].includes(pathname) ? 'index.html' : pathname.slice(1);
    if (!/^[a-z.-]+$/.test(name)) { response.writeHead(404).end(); return; }
    try {
      const body = await readFile(path.join(directory, name));
      const types = {'.html':'text/html', '.js':'text/javascript', '.css':'text/css', '.svg':'image/svg+xml'};
      response.writeHead(200, {'Content-Type':types[path.extname(name)] || 'text/plain'}).end(body);
    } catch { response.writeHead(404).end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  endpoint = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({headless:true});
});
after(async () => {
  await browser?.close();
  if (server) await new Promise(resolve => server.close(resolve));
});

function revision(id, branch, parents, kind, created) {
  return {id, branch, parents, kind, created_ns:created, workspace:'workspace', actor:'owner', root:{id:'tree-'+id}, message:id};
}
function fixture() {
  const revisions = [
    revision('initial', 'main', [], 'commit', 1),
    revision('parent-commit', 'main', ['initial'], 'commit', 2),
    revision('child-autosave', 'feature', ['parent-commit'], 'autosave', 3),
    revision('merge-candidate', null, ['parent-commit', 'child-autosave'], 'candidate', 4),
  ];
  const main = {id:'main', name:'main', local:true, owner:'local', formal_head:'parent-commit', autosave_head:null, base_revision:'initial',
    source_branch:null, working_root:{id:'tree-parent-commit'}, dirty:false, mutation_seq:1, saved_seq:1, state:'writable', authority_epoch:1, generation:1};
  const child = {...main, id:'feature', name:'feature', source_branch:'main', base_revision:'parent-commit',
    autosave_head:'child-autosave', working_root:{id:'tree-child-autosave'}, dirty:true};
  const empty = {...main, id:'empty', name:'empty', source_branch:'main', base_revision:'parent-commit'};
  const branches = [main, child, empty];
  return {
    branches,
    status:{workspace:{id:'workspace', name:'Review workspace'}, location:'local', selected_branch:'feature',
      branches, mounts:[], capabilities:{remote_configured:false}, pending_jobs:0},
    revisions,
    origins:[main, child, empty].map(branch => ({branch:branch.id, source_branch:branch.source_branch,
      revision:branch.base_revision, revision_available:true, workspace_root:branch.id === 'main'})),
    summary:{branch_count:3, mount_count:0, ready_mount_count:0, mounted_branch_count:0},
  };
}
function relationshipFixture(data) {
  return {
    workspace:'workspace', observing_location:'local', selected_branch:data.status.selected_branch,
    remote_mounts_known:false, remote_selections_known:false, total:data.status.branches.length, next:null,
    entries:data.status.branches.flatMap(branch => {
      const mounts = data.status.mounts.filter(mount => mount.branch === branch.id && mount.state !== 'released');
      return (mounts.length ? mounts : [null]).map(mount => ({
      branch:{id:branch.id, name:branch.name, state:branch.state,
        ownership:{location:branch.owner || 'local', source:'local_authority', authority_epoch:1, generation:1, storage_version:null, observed_ns:null},
        origin:{workspace_root:!branch.source_branch, source_branch:branch.source_branch, source_name:branch.source_branch, base_revision:branch.base_revision, captured_working_root:null, status:'available'}},
      mount, mount_location:mount ? 'local' : null,
    })); }),
  };
}
async function open(t, data = fixture(), target = '/dashboard') {
  const page = await browser.newPage({viewport:{width:1440, height:1000}});
  const errors = [], requests = [], commands = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(async () => { await page.close(); assert.deepEqual(errors, []); });
  await page.addInitScript(() => sessionStorage.setItem('agentdrive-token', 'test-token'));
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    requests.push(url);
    if (url.pathname.startsWith('/api/commands/')) {
      const tool = url.pathname.split('/').at(-1), input = route.request().postDataJSON();
      commands.push({tool, input});
      if (data.command) return data.command(route, tool, input);
      return route.fulfill({status:403, json:{error:{message:'Command is unavailable'}}});
    } else if (url.pathname === '/api/workspaces') {
      await route.fulfill({json:[{id:'workspace', name:'Review workspace'}]});
    } else if (url.pathname === '/api/workspaces/workspace/archive') {
      await route.fulfill({json:data.status});
    } else if (url.pathname === '/api/workspaces/workspace') {
      const ids = new Set(data.branches.flatMap(branch => [branch.formal_head, branch.autosave_head, branch.base_revision]));
      await route.fulfill({json:{...data,revisions:data.revisions.filter(revision => ids.has(revision.id))}});
    } else if (url.pathname.endsWith('/history')) {
      const branch = url.pathname.split('/').at(-2), search = (url.searchParams.get('search') || '').toLowerCase();
      const entries = data.revisions.filter(revision => revision.branch === branch && (!search || (revision.id+' '+revision.message).toLowerCase().includes(search)))
        .sort((a,b) => b.created_ns-a.created_ns || a.id.localeCompare(b.id));
      const cursor = Number(url.searchParams.get('cursor') || 0), limit = Number(url.searchParams.get('limit') || 50);
      await route.fulfill({json:{entries:entries.slice(cursor,cursor+limit),total:entries.length,next:cursor+limit<entries.length ? String(cursor+limit) : null}});
    } else if (url.pathname === '/api/workspaces/workspace/relationships') {
      const result = typeof data.relationships === 'function' ? await data.relationships(url) : data.relationships || relationshipFixture(data);
      await route.fulfill(result.error ? {status:409, json:result} : {json:result});
    } else if (url.pathname.endsWith('/files')) {
      const branch = url.pathname.split('/').at(-2);
      const version = `${branch}:1:1:1`;
      const entries = url.searchParams.get('path') === '/nested' ? [] : [
        {path:'/nested', inode:{kind:'directory', mode:493, size:0}, dirty:false},
        {path:'/note.txt', inode:{kind:'file', mode:420, size:18}, dirty:true},
        {path:'/binary', inode:{kind:'file', mode:420, size:3}, dirty:false},
      ];
      await route.fulfill({json:{version, entries, next:null}});
    } else if (url.pathname.endsWith('/file')) {
      await route.fulfill({contentType:'application/octet-stream', body:url.searchParams.get('path') === '/binary' ? Buffer.from([0,255,1]) : Buffer.from('hello <script>world')});
    } else if (url.pathname.endsWith('/changes')) {
      await route.fulfill({json:{total:2, next:null, entries:[{path:'/first.txt', kind:'modified'}, {path:'/second.txt', kind:'added'}]}});
    } else if (url.pathname.endsWith('/diff')) {
      const inode = {kind:'file', size:5, mode:420, uid:1000, gid:1000, content:{id:'content'}};
      await route.fulfill({json:{status:'text', before:inode, after:inode,
        hunks:[[{kind:'added', before_line:null, after_line:1, text:'hello', newline:true}]]}});
    } else await route.fulfill({status:404, json:{error:{message:'Unknown test endpoint'}}});
  });
  await page.goto(endpoint+target);
  await page.locator(target.startsWith('/archive') ? '.archive-branch[data-branch="feature"]' : target.startsWith('/mounts') ? '#mount-create-form' : target.startsWith('/branches') ? '#branch-create-form' : '#branch-feature').waitFor();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  return {page, data, requests, commands, branch:data.status.branches[1]};
}
async function refresh(page) {
  const response = page.waitForResponse(url => url.url().endsWith('/api/workspaces/workspace'));
  await page.evaluate(() => document.querySelector('#refresh').click());
  await response;
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
}
async function openRevision(page, id = 'child-autosave') {
  await page.locator('#revision-'+id).dblclick();
  await page.locator('.diff-path').waitFor();
}
function differenceReads(requests) { return requests.filter(url => /\/(changes|diff)$/.test(url.pathname)); }

test('settings configure OpenCSG credentials, preserve blank tokens and isolate storage forms', async t => {
  const page = await browser.newPage({viewport:{width:1440,height:1100}});
  const errors = [], updates = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(async () => { await page.close(); assert.deepEqual(errors, []); });
  await page.addInitScript(() => sessionStorage.setItem('agentdrive-token', 'test-token'));
  let settings = {revision:'initial-settings', config_path:'/private/agentdrive.json', reload_error:null,
    runtime:{remote:null, cache:{max_bytes:2147483648}, allowed_origins:[]},
    startup:{listen:'127.0.0.1:7421',data_dir:'/private/data',mount_roots:[],directory_roots:[],credentials:[],administrators:['owner']}};
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname === '/api/workspaces') return route.fulfill({json:[]});
    if (url.pathname !== '/api/settings') return route.fulfill({status:404,json:{error:{message:'Unknown endpoint'}}});
    if (route.request().method() === 'PUT') {
      const request = route.request().postDataJSON(); updates.push(request);
      settings = {...settings,revision:'settings-'+updates.length,runtime:structuredClone(request.runtime)};
      if (settings.runtime.remote?.backend === 'opencsg') settings.runtime.remote.token = '';
    }
    return route.fulfill({json:settings});
  });
  await page.goto(endpoint+'/settings');
  await page.locator('#settings-form').waitFor();
  await page.locator('#storage-mode').selectOption('opencsg');
  assert.equal(await page.locator('#s3-fields').isVisible(), false);
  assert.equal(await page.locator('#s3-bucket').isDisabled(), true);
  await page.locator('#opencsg-endpoint').fill('https://opencsg-stg.com');
  await page.locator('#opencsg-repository').fill('owner/storage');
  await page.locator('#opencsg-token').fill('new-private-token');
  assert.equal(await page.locator('#opencsg-token').getAttribute('type'), 'password');
  async function save() {
    const response = page.waitForResponse(r => r.url().endsWith('/api/settings') && r.request().method() === 'PUT');
    await page.locator('#settings-save').click(); await response;
    await page.waitForFunction(() => document.querySelector('#settings-save').disabled);
  }
  await save();
  assert.equal(updates[0].runtime.remote.backend, 'opencsg');
  assert.equal(updates[0].runtime.remote.token, 'new-private-token');
  assert.equal(updates[0].runtime.remote.endpoint, 'https://opencsg-stg.com');
  assert.equal(await page.locator('#opencsg-token').inputValue(), '');
  await page.locator('#cache-max').fill('3'); await save();
  assert.equal(updates[1].runtime.remote.token, '');
  assert.equal(updates[1].runtime.cache.max_bytes, 3221225472);
  await page.locator('#opencsg-endpoint').fill('https://another.example.com');
  assert.equal(await page.locator('#opencsg-token').evaluate(input => input.required), true);
  await page.locator('#opencsg-token').fill('replacement-token'); await save();
  assert.equal(updates[2].runtime.remote.token, 'replacement-token');
  await page.locator('#storage-mode').selectOption('local'); await save();
  assert.equal(updates[3].runtime.remote.enabled, false);
  assert.equal(await page.locator('#cache-panel').isVisible(), false);
  await page.locator('#storage-mode').selectOption('s3');
  await page.locator('#s3-bucket').fill('bucket'); await save();
  assert.equal(updates[4].runtime.remote.backend, 's3');
  assert.equal('token' in updates[4].runtime.remote, false);
  assert.equal(await page.locator('#opencsg-token').isDisabled(), true);
});

test('history contains only revisions authored on the selected branch, including an empty child', async t => {
  const {page} = await open(t);
  assert.deepEqual(await page.locator('.revision-title').allTextContents(), ['child-autosave']);
  assert.equal(await page.locator('#history-count').textContent(), '1');
  await page.locator('#branch-empty').click();
  await page.locator('.no-revisions').waitFor();
  assert.equal(await page.locator('#history-count').textContent(), '0');
  assert.equal(await page.locator('.no-revisions').textContent(), '此分支尚未生成新版本。');
  await page.locator('#branch-main').click();
  await page.locator('#revision-parent-commit').waitFor();
  assert.deepEqual(await page.locator('.revision-title').allTextContents(), ['parent-commit', 'initial']);
});

test('save state follows durability before dirty state and retains the stopped indicator', async t => {
  const {page, branch} = await open(t);
  const status = page.locator('#branch-state');
  assert.equal(await status.textContent(), '已自动保存 · 有未提交修改');
  branch.mutation_seq++;
  await refresh(page);
  assert.equal(await status.textContent(), '有未保存修改');
  branch.dirty = false;
  await refresh(page);
  assert.equal(await status.textContent(), '有未保存修改');
  branch.saved_seq = branch.mutation_seq;
  branch.dirty = true; branch.working_root = {id:'new-checkpoint'};
  await refresh(page);
  assert.equal(await status.textContent(), '已自动保存 · 有未提交修改');
  assert((await status.getAttribute('title')).includes('等待生成自动保存版本'));
  branch.dirty = false; branch.working_root = {id:'tree-parent-commit'};
  branch.state = 'stopped';
  await refresh(page);
  assert.equal(await status.textContent(), '已提交');
  assert((await page.locator('#branch-meta').textContent()).includes('已停止写入'));
});

test('double click opens combined details, uses the parent and selects the first changed file', async t => {
  const {page, requests} = await open(t);
  assert.equal(await page.locator('.revision-more,.revision-menu').count(), 0);
  await page.locator('#revision-child-autosave').click();
  assert.equal(await page.locator('#revision-dialog').evaluate(el => el.open), false);
  assert.equal(differenceReads(requests).length, 0);
  await openRevision(page);
  assert.deepEqual(await page.locator('.revision-section-title').allTextContents(), ['版本信息','修改内容']);
  assert.equal(await page.locator('.diff-path').textContent(), '/first.txt');
  assert(differenceReads(requests).every(url => url.searchParams.get('before') === 'parent-commit'));
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'revision-child-autosave');
  await page.locator('#revision-child-autosave').press('Enter');
  await page.locator('.diff-path').waitFor();
  await page.getByRole('button', {name:'关闭弹框'}).click();
  await page.locator('#revision-child-autosave').press('Space');
  await page.locator('.diff-path').waitFor();
  const bounds = await page.locator('#revision-dialog').boundingBox();
  await page.mouse.click(bounds.x - 4, bounds.y + 5);
  assert.equal(await page.locator('#revision-dialog').evaluate(el => el.open), false);
});

test('open revision metadata follows a new commit without reloading the selected difference', async t => {
  const data = fixture(), branch = data.status.branches[1];
  branch.formal_head = 'child-autosave'; branch.autosave_head = null; branch.dirty = false;
  data.revisions[2].kind = 'commit';
  const {page, requests} = await open(t, data);
  await openRevision(page);
  assert((await page.locator('.revision-detail').textContent()).includes('当前正式提交'));
  await page.locator('.changed-file').filter({hasText:'/second.txt'}).click();
  await page.waitForFunction(() => document.querySelector('.diff-path')?.textContent === '/second.txt');
  const count = differenceReads(requests).length;
  data.revisions.push(revision('next-commit', 'feature', ['child-autosave'], 'commit', 5));
  branch.formal_head = 'next-commit'; branch.working_root = {id:'tree-next-commit'};
  await refresh(page);
  assert.equal(await page.locator('#revision-dialog').evaluate(el => el.open), true);
  assert(!(await page.locator('.revision-detail').textContent()).includes('当前正式提交'));
  assert.equal(await page.locator('.diff-path').textContent(), '/second.txt');
  assert.equal(differenceReads(requests).length, count);
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'revision-child-autosave');
});

test('source revisions remain accessible with the child selected and dialogs fit narrow screens', async t => {
  const {page, requests} = await open(t);
  await page.locator('#origin-main-parent-commit').click();
  await page.locator('.diff-path').waitFor();
  assert((await page.locator('.revision-detail').textContent()).includes('parent-commit'));
  assert(differenceReads(requests).every(url => url.searchParams.get('before') === 'initial'));
  await refresh(page);
  assert.equal(await page.locator('#revision-dialog').evaluate(el => el.open), true);
  await page.setViewportSize({width:390, height:844});
  const layout = await page.locator('#revision-dialog').evaluate(el => ({width:el.clientWidth, scrollWidth:el.scrollWidth,
    left:el.getBoundingClientRect().left, right:el.getBoundingClientRect().right}));
  assert(layout.scrollWidth <= layout.width && layout.left >= 0 && layout.right <= 390);
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'origin-main-parent-commit');
});


test('double clicking an unselected branch opens current files and preserves single click history', async t => {
  const {page, requests} = await open(t);
  await page.locator('#branch-main').click();
  assert.equal(await page.locator('#branch-files-dialog').evaluate(el => el.open), false);
  assert.equal(requests.filter(url => url.pathname.endsWith('/files')).length, 0);
  await page.locator('#branch-feature').dblclick({delay:80});
  await page.locator('.branch-file-content').waitFor();
  assert.equal(await page.locator('#history-title').textContent(), 'feature');
  assert.equal(await page.locator('.branch-file-path').textContent(), '/note.txt');
  assert.equal(await page.locator('.branch-file-content').textContent(), 'hello <script>world');
  assert((await page.locator('.branch-file-preview:visible').textContent()).includes('当前工作内容'));
  assert.equal(await page.locator('.branch-file-content script').count(), 0);
  await page.locator('.branch-file[data-path="/binary"]').click();
  await page.waitForFunction(() => document.querySelector('.branch-file-content')?.textContent.includes('00 ff 01'));
  await page.locator('.branch-file[data-path="/nested"]').click();
  await page.getByText('此目录为空。', {exact:true}).waitFor();
  assert.equal(await page.locator('#branch-files-dialog .branch-directory').textContent(), '/nested');
  await page.getByRole('button', {name:'根目录', exact:true}).click();
  await page.locator('.branch-file-content').waitFor();
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'branch-feature');
  await page.locator('#branch-main').press('Enter');
  await page.locator('.branch-file-content').waitFor();
  assert((await page.locator('#branch-files-dialog .revision-dialog-subtitle').textContent()).startsWith('main'));
  await page.setViewportSize({width:390,height:844});
  const bounds = await page.locator('#branch-files-dialog').evaluate(el => ({width:el.clientWidth,scroll:el.scrollWidth,right:el.getBoundingClientRect().right}));
  assert(bounds.scroll <= bounds.width && bounds.right <= 390);
});

test('file browser reports changes, refreshes current files and cancels a delayed response when closed', async t => {
  const {page, branch} = await open(t);
  await page.locator('#branch-feature').dblclick();
  await page.locator('.branch-file-content').waitFor();
  branch.mutation_seq++;
  await refresh(page);
  await page.getByText('分支内容已更新，可点击“刷新文件”读取最新内容。', {exact:true}).waitFor();
  await page.route('**/branches/feature/file?**', route => route.fulfill({status:409,json:{error:{message:'changed'}}}));
  await page.locator('.branch-file[data-path="/binary"]').click();
  await page.getByText('分支内容已更新，请点击“刷新文件”重新读取。', {exact:true}).waitFor();
  await page.unroute('**/branches/feature/file?**');
  await page.getByRole('button', {name:'刷新文件',exact:true}).first().click();
  await page.locator('.branch-file-content').waitFor();
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  await page.route('**/branches/feature/file?**', async route => { await pending; await route.fulfill({body:'delayed'}).catch(() => {}); });
  await page.locator('.branch-file[data-path="/binary"]').click();
  await page.getByText('正在读取文件…',{exact:true}).waitFor();
  await page.keyboard.press('Escape'); release();
  assert.equal(await page.locator('#branch-files-dialog').evaluate(el => el.open), false);
  assert.equal(await page.locator('.branch-file-content').count(), 0);
});

test('file pagination keeps UTF-8 character boundaries and directory pagination keeps the same version', async t => {
  const {page, requests} = await open(t);
  const content = Buffer.from('a'.repeat(65535) + '中文结束');
  await page.route('**/branches/main/files?**', route => {
    const url = new URL(route.request().url());
    const next = !url.searchParams.has('after');
    return route.fulfill({json:{version:'main:1:1:1', entries:[{path:next ? '/large.txt' : '/empty.txt', inode:{kind:'file',mode:420,size:next ? content.length : 0}}], next:next ? '/large.txt' : null}});
  });
  await page.route('**/branches/main/file?**', route => {
    const url = new URL(route.request().url());
    const offset = Number(url.searchParams.get('offset')), size = Number(url.searchParams.get('size'));
    return route.fulfill({body:content.subarray(offset, offset+size)});
  });
  await page.locator('#branch-main').dblclick({delay:80});
  await page.locator('.branch-file-content').waitFor();
  assert.equal((await page.locator('.branch-file-content').textContent()).length,65535);
  await page.getByRole('button',{name:'下一段',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('.branch-file-content')?.textContent === '中文结束');
  await page.getByRole('button',{name:'上一段',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('.branch-file-content')?.textContent.length === 65535);
  const nextPage = page.waitForRequest(request => request.url().includes('after='));
  await page.getByRole('button',{name:'加载更多文件',exact:true}).click();
  const url = new URL((await nextPage).url());
  assert.equal(url.searchParams.get('version'),'main:1:1:1');
  await page.locator('.branch-file[data-path="/empty.txt"]').waitFor();
  await page.getByRole('button',{name:'下一段',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('.branch-file-content')?.textContent === '中文结束');
  assert.equal(await page.locator('#branch-files-dialog .branch-file.active').getAttribute('data-path'),'/large.txt');
});


test('archive opens branch contents directly, preserves the mounted selection, and skips revision history requests', async t => {
  const {page, requests, data} = await open(t, fixture(), '/archive');
  await page.locator('#archive-page .branch-file-content').waitFor();
  assert.equal(await page.locator('#archive-nav').getAttribute('aria-current'),'page');
  assert.equal(await page.locator('#history-page').isVisible(),false);
  assert.equal(await page.locator('#archive-page .branch-file-path').textContent(),'/note.txt');
  assert.equal(requests.filter(url => url.pathname === '/api/workspaces/workspace').length,0);
  await page.locator('.archive-branch[data-branch="main"]').click();
  await page.locator('#archive-page .branch-file-content').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('branch'),'main');
  assert.equal(data.status.selected_branch,'feature');
  assert(requests.every(url => !url.pathname.includes('/mcp')));
  assert.equal(await page.locator('.archive-branch-header h2').textContent(),'main');
  await page.locator('.archive-branch[data-branch="feature"]').click();
  await page.locator('.archive-history-link').click();
  await page.waitForFunction(() => document.querySelector('#history-title')?.textContent === 'feature');
  assert.equal(await page.locator('#history-title').textContent(),'feature');
  await page.locator('#archive-nav').click();
  await page.locator('#archive-page .branch-file-content').waitFor();
  assert.equal(await page.locator('.archive-branch-header h2').textContent(),'feature');
  await page.goBack();
  await page.waitForFunction(() => document.querySelector('#history-title')?.textContent === 'feature');
  assert.equal(await page.locator('#history-title').textContent(),'feature');
});

test('archive renders safe Markdown, filters files, exposes hidden files and keeps directory links across reload', async t => {
  const {page} = await open(t, fixture(), '/archive');
  await page.route('**/branches/feature/files?**', route => {
    const url = new URL(route.request().url()), nested = url.searchParams.get('path') === '/nested';
    const names = nested ? ['/nested/README.md'] : ['/README.md','/.env','/nested'];
    return route.fulfill({json:{version:'feature:1:1:1',next:null,entries:names.map(path => ({path,inode:{kind:path === '/nested' ? 'directory' : 'file',size:200,mode:420}}))}});
  });
  await page.route('**/branches/feature/file?**',route => route.fulfill({body:'# Research handoff\n\n**Ready** for the next Agent.\n\n| Task | Status |\n| --- | --- |\n| Review | Done |\n\n<script>alert(1)</script>\n'}));
  await page.locator('#archive-page').getByRole('button',{name:'刷新文件',exact:true}).click();
  await page.locator('.document-preview h1').waitFor();
  assert.equal(await page.locator('.document-preview h1').textContent(),'Research handoff');
  assert.equal(await page.locator('.document-preview script').count(),0);
  assert.equal(await page.locator('.document-table td').count(),2);
  await page.getByRole('button',{name:'源码',exact:true}).click();
  assert.equal(await page.locator('#archive-page .branch-file-content').isVisible(),true);
  assert.equal(await page.locator('.branch-file[data-path="/.env"]').count(),0);
  await page.locator('#archive-page').getByLabel('显示隐藏文件').check();
  await page.locator('.branch-file[data-path="/.env"]').waitFor();
  await page.locator('#archive-page .file-filter').fill('readme');
  assert.equal(await page.locator('#archive-page .branch-file').count(),1);
  await page.locator('#archive-page .file-filter').fill('');
  await page.locator('.branch-file[data-path="/nested"]').click();
  await page.locator('.branch-file[data-path="/nested/README.md"]').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('path'),'/nested');
  await page.reload();
  await page.locator('.branch-file[data-path="/nested/README.md"]').waitFor();
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.locator('#history-nav').isVisible(),true);
  const widths = await page.evaluate(() => [document.documentElement.scrollWidth,innerWidth]);
  assert(widths[0] <= widths[1]);
});

test('archive renders saved and remote confirmation states supplied by the daemon', async t => {
  const data = fixture(), branch = data.status.branches[1];
  branch.owner = 'local'; data.status.capabilities.remote_configured = true;
  data.status.synchronization = {state:'confirmed',branches:[{branch:branch.id,local_saved:true,remote_confirmed:true}]};
  data.status.remote_branches = [{branch:branch.id,owner:'local',authority_epoch:1,generation:1,durable_seq:1,working_root:branch.working_root}];
  const {page} = await open(t,data,'/archive');
  const remote = page.locator('.archive-state').nth(1).locator('strong');
  assert.equal(await remote.textContent(),'远端已保存');
  branch.mutation_seq++;
  data.status.synchronization = {state:'unsaved',branches:[{branch:branch.id,local_saved:false,remote_confirmed:false}]};
  await page.locator('#refresh').click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await remote.textContent(),'等待本机保存');
  assert.equal(await page.locator('.archive-state').first().locator('strong').textContent(),'有未保存修改');
  data.status.pending_jobs = 1;
  data.status.synchronization = {state:'pending',branches:[{branch:branch.id,local_saved:true,remote_confirmed:false}]};
  await page.locator('#refresh').click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await remote.textContent(),'同步处理中');
});

test('archive clears stale content when switching projects and shows empty and failed states', async t => {
  const {page} = await open(t, fixture(), '/archive');
  await page.locator('#archive-page .branch-file-content').waitFor();
  await page.route('**/api/workspaces',route => route.fulfill({json:[{id:'workspace',name:'Review workspace'},{id:'second',name:'Second project'}]}));
  await page.route('**/api/workspaces/second/archive',route => route.fulfill({json:{workspace:{id:'second',name:'Second project'},branches:[],mounts:[],remote_branches:[],capabilities:{remote_configured:false}}}));
  await page.locator('#refresh').click();
  await page.waitForFunction(()=>document.querySelector('#workspace').options.length === 2 && !document.querySelector('#refresh').disabled);
  await page.locator('#workspace').selectOption('second');
  await page.getByText('还没有可以浏览的项目',{exact:true}).waitFor();
  assert.equal(await page.locator('#archive-page .branch-file-content').count(),0);
  await page.route('**/api/workspaces/second/archive',route => route.fulfill({status:503,json:{error:{message:'Storage unavailable'}}}));
  await page.locator('#refresh').click();
  await page.locator('#error').filter({hasText:'Storage unavailable'}).waitFor();
  assert.equal(await page.locator('#error').isVisible(),true);
});

test('refresh keeps a selected file from a later directory page and its source view', async t => {
  const {page} = await open(t, fixture(), '/archive');
  let reportDirectory = false, reads = 0;
  await page.route('**/branches/feature/files?**', route => {
    reads++;
    const after = new URL(route.request().url()).searchParams.get('after');
    const path = after === '/report.md' ? '/tail.txt' : after ? '/report.md' : '/README.md';
    return route.fulfill({json:{version:'feature:1:1:1',next:!after ? '/README.md' : reportDirectory && after === '/README.md' ? '/report.md' : null,entries:[
      {path,inode:{kind:reportDirectory && path === '/report.md' ? 'directory' : 'file',size:6}}
    ]}});
  });
  await page.route('**/branches/feature/file?**',route=>route.fulfill({body:'# Data'}));
  const archive = page.locator('#archive-page');
  await archive.getByRole('button',{name:'刷新文件',exact:true}).click();
  await archive.getByRole('button',{name:'加载更多文件',exact:true}).click();
  await archive.locator('.branch-file[data-path="/report.md"]').click();
  await archive.getByRole('button',{name:'源码',exact:true}).click();
  await archive.getByRole('button',{name:'刷新文件',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#archive-page .branch-file-path')?.textContent==='/report.md');
  assert.equal(await archive.locator('.branch-file-content').isVisible(),true);
  assert.equal(await archive.locator('.branch-file.active').getAttribute('data-path'),'/report.md');
  reportDirectory = true;
  const before = reads;
  await archive.getByRole('button',{name:'刷新文件',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#archive-page .branch-file-path')?.textContent==='/README.md');
  assert.equal(reads-before,2);
  assert.equal(await archive.getByRole('button',{name:'加载更多文件',exact:true}).isVisible(),true);
});

test('Markdown source mode survives file paging and reading mode preserves ordered steps', async t => {
  const {page} = await open(t, fixture(), '/archive');
  const content = Buffer.from('3. Prepare\n4. Publish\n\n- Notes\n\n'+'x'.repeat(66000));
  await page.route('**/branches/feature/files?**',route=>route.fulfill({json:{version:'feature:1:1:1',next:null,entries:[{path:'/README.md',inode:{kind:'file',size:content.length}}]}}));
  await page.route('**/branches/feature/file?**',route=>{
    const query = new URL(route.request().url()).searchParams;
    const offset = Number(query.get('offset')), size = Number(query.get('size'));
    return route.fulfill({body:content.subarray(offset,offset+size)});
  });
  const archive = page.locator('#archive-page');
  await archive.getByRole('button',{name:'刷新文件',exact:true}).click();
  await archive.getByRole('button',{name:'源码',exact:true}).click();
  await archive.getByRole('button',{name:'下一段',exact:true}).click();
  await archive.getByRole('button',{name:'上一段',exact:true}).waitFor();
  assert.equal(await archive.locator('.branch-file-content').isVisible(),true);
  await archive.getByRole('button',{name:'上一段',exact:true}).click();
  await archive.getByRole('button',{name:'阅读',exact:true}).click();
  assert.equal(await archive.locator('.document-preview ol').getAttribute('start'),'3');
  assert.deepEqual(await archive.locator('.document-preview ol li').allTextContents(),['Prepare','Publish']);
  assert.equal(await archive.locator('.document-preview ul li').textContent(),'Notes');
});

test('breadcrumbs navigate ancestor paths with Unicode and reserved characters and survive reload', async t => {
  const {page, requests} = await open(t, fixture(), '/archive');
  const paths = ['/', '/资料 #1', '/资料 #1/a & b', '/资料 #1/a & b/100%'];
  await page.route('**/branches/feature/files?**',route=>{
    const path = new URL(route.request().url()).searchParams.get('path');
    const index = paths.indexOf(path);
    return route.fulfill({json:{version:'feature:1:1:1',next:null,entries:index < paths.length-1 ? [{path:paths[index+1],inode:{kind:'directory',size:0}}] : []}});
  });
  const archive = page.locator('#archive-page');
  await archive.getByRole('button',{name:'刷新文件',exact:true}).click();
  for (const path of paths.slice(1)) await archive.locator('.branch-file').filter({hasText:path.split('/').at(-1)}).click();
  await archive.getByText('此目录为空。',{exact:true}).waitFor();
  const breadcrumbs = archive.getByRole('navigation',{name:'目录路径'});
  assert.equal(await breadcrumbs.textContent(),paths.at(-1));
  assert.equal(await breadcrumbs.locator('[aria-current]').textContent(),'100%');
  await breadcrumbs.getByRole('button',{name:'a & b',exact:true}).click();
  await archive.locator('.branch-file').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('path'),paths[2]);
  await page.reload();
  await archive.locator('.branch-file').waitFor();
  assert.equal(await breadcrumbs.textContent(),paths[2]);
  await breadcrumbs.getByRole('button',{name:'资料 #1',exact:true}).press('Enter');
  await archive.locator('.branch-file').waitFor();
  assert.equal(await page.evaluate(()=>document.activeElement.dataset.path),paths[1]);
  await breadcrumbs.getByRole('button',{name:'根目录',exact:true}).click();
  await archive.locator('.branch-file').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('path'),'/');
  assert(requests.every(url=>url.pathname.startsWith('/api/workspaces')));
  await page.setViewportSize({width:390,height:844});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
});

test('switching archive branches discards delayed directory and file responses', async t => {
  const {page, data, requests} = await open(t, fixture(), '/archive');
  const archive = page.locator('#archive-page');
  await page.route('**/branches/main/file?**',route=>route.fulfill({body:'current main bytes'}));
  const main = page.locator('.archive-branch[data-branch="main"]');
  const feature = page.locator('.archive-branch[data-branch="feature"]');
  async function checkMain() {
    await page.waitForFunction(()=>document.querySelector('#archive-page .branch-file-content')?.textContent==='current main bytes');
    assert.equal(await page.locator('.archive-branch-header h2').textContent(),'main');
    assert.equal(new URL(page.url()).searchParams.get('branch'),'main');
  }
  for (const kind of ['files','file']) {
    await main.click(); await checkMain();
    let release, started, completed;
    const blocked = new Promise(resolve=>{release=resolve;});
    const pending = new Promise(resolve=>{started=resolve;});
    const done = new Promise(resolve=>{completed=resolve;});
    const pattern = `**/branches/feature/${kind}?**`;
    await page.route(pattern,async route=>{
      started(); await blocked;
      await route.fulfill(kind === 'file' ? {body:'stale feature body'} : {json:{version:'feature:1:1:1',next:null,entries:[{path:'/stale.txt',inode:{kind:'file',size:18}}]}}).catch(()=>{});
      completed();
    });
    t.after(()=>release());
    await feature.click(); await pending;
    await main.click(); await checkMain();
    release(); await done;
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    await checkMain();
    assert.equal(await archive.locator('.branch-file[data-path="/stale.txt"]').count(),0);
    await page.unroute(pattern);
  }
  assert.equal(data.status.selected_branch,'feature');
  assert(requests.every(url=>!url.pathname.includes('/mcp')));
});

test('relationships display ownership, mount principals and exact origins and send filters', async t => {
  const data = fixture();
  const result = relationshipFixture(data);
  const entry = result.entries[1];
  data.branches[1].owner = 'remote-owner';
  entry.branch.ownership = {location:'remote-owner',source:'remote_reference',authority_epoch:3,generation:4,storage_version:'storage-v7',observed_ns:1792312200000000000};
  entry.branch.origin.captured_working_root = {id:'creation-root'};
  entry.mount = {id:'mount-1',path:'/workspace/current',principal:'reader <script>',access:'read_only',state:'ready',generation:5};
  entry.mount_location = 'local'; result.entries = [entry]; result.total = 1; data.relationships = result;
  const {page, requests} = await open(t,data,'/mounts');
  const panel = page.locator('#relationships-panel');
  assert.match(await panel.textContent(), /远端观测记录/);
  assert.match(await panel.textContent(), /reader <script>/);
  assert.match(await panel.textContent(), /远端挂载、远端默认分支及机器在线状态未知/);
  assert.equal(await panel.locator('script').count(), 0);
  await panel.getByText('记录详情', {exact:true}).click();
  assert.match(await panel.textContent(), /storage-v7/);
  await panel.getByText('创建时的内容对象', {exact:true}).click();
  assert.equal(await panel.locator('.relationship-root').textContent(),'creation-root');
  await page.locator('#relationship-branch').selectOption('feature');
  await panel.getByText('更多筛选', {exact:true}).click();
  await page.locator('#relationship-mount_state').selectOption('released');
  await page.locator('#relationship-owner_location').selectOption('remote-owner');
  await page.locator('#relationship-mount_location').selectOption('local');
  await page.locator('#relationship-mount').fill('mount-1');
  entry.mount.state = 'released';
  await panel.getByRole('button',{name:'查询',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#relationship-list .released'));
  const request = requests.filter(url => url.pathname.endsWith('/relationships')).at(-1);
  assert.equal(request.searchParams.get('branch'),'feature');
  assert.equal(request.searchParams.get('mount_state'),'released');
  assert.equal(request.searchParams.get('owner_location'),'remote-owner');
  assert.equal(request.searchParams.get('mount_location'),'local');
  assert.equal(request.searchParams.get('mount'),'mount-1');
  await page.setViewportSize({width:390,height:844});
  const widths = await page.evaluate(() => [document.documentElement.scrollWidth,innerWidth]);
  assert(widths[0] <= widths[1]);
});

test('relationships retain loaded pages and offer reload after the server rejects a changed cursor', async t => {
  const data = fixture(), result = relationshipFixture(data);
  const base = result.entries[1];
  const entry = index => ({...base,mount_location:'local',mount:{id:`mount-${index}`,path:`/mount/${index}`,principal:'owner',access:'read_only',state:'ready',generation:1}});
  let changed = false;
  data.relationships = url => url.searchParams.has('cursor')
    ? changed ? {error:{code:'TARGET_MOVED',message:'relationships changed'}} : {...result,entries:[entry(50)],total:51,next:null}
    : {...result,entries:Array.from({length:50},(_,index)=>entry(index)),total:51,next:'page-2'};
  const {page, requests} = await open(t,data,'/mounts');
  assert.equal(await page.locator('#relationship-list tr').count(),50);
  await page.locator('#relationship-more').click();
  await page.waitForFunction(() => document.querySelectorAll('#relationship-list tr').length === 51);
  assert.equal(await page.locator('#relationship-more').isVisible(),false);
  await refresh(page);
  assert.equal(await page.locator('#relationship-list tr').count(),51);
  changed = true;
  await refresh(page);
  assert.equal(await page.locator('#relationship-list tr').count(),51);
  changed = false;
  await page.getByRole('button',{name:'重新载入关系',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#relationship-status').textContent === '已显示 51 / 51 条已知关系');
  const calls = requests.filter(url => url.pathname.endsWith('/relationships'));
  assert.equal(calls.at(-2).searchParams.has('cursor'),false);
  assert.equal(calls.at(-1).searchParams.get('cursor'),'page-2');
  assert.equal(await page.locator('#relationship-list tr').count(),51);
});

test('relationships clear old rows and report query failures without hiding branch history', async t => {
  const data = fixture();
  const {page} = await open(t,data,'/mounts');
  assert.equal(await page.locator('#relationship-list tr').count(),3);
  data.relationships = {error:{code:'UNAVAILABLE',message:'relationship service unavailable'}};
  await page.locator('#relationships-panel').getByRole('button',{name:'查询',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#relationship-status').textContent.includes('查询失败'));
  assert.equal(await page.locator('#relationship-list tr').count(),0);
  assert.equal(await page.locator('#relationship-more').isVisible(),false);
  assert.equal(await page.locator('#mount-create-form').isVisible(),true);
});

test('workspace tree includes other locations and preserves exact ancestry and local mounts', async t => {
  const data = fixture();
  const main = data.branches[0], a = data.branches[1], b = data.branches[2];
  main.local = false; main.owner = 'location-a';
  a.local = false; a.owner = 'location-a'; a.name = 'feat/a';
  Object.assign(b, {name:'feat/b', owner:'location-b', source_branch:'feature', base_revision:'child-autosave'});
  data.status.location = 'location-b'; data.status.selected_branch = b.id; data.status.branches = [b];
  data.status.mounts = [{id:'mount-b', branch:b.id, path:'/project', state:'ready', access:'read_write'}];
  data.summary.mount_count = data.summary.ready_mount_count = data.summary.mounted_branch_count = 1;
  data.origins.find(origin => origin.branch === b.id).source_branch = a.id;
  data.origins.find(origin => origin.branch === b.id).revision = 'child-autosave';
  const {page, requests} = await open(t, data);
  assert.equal(await page.locator('.branch-button').count(), 3);
  assert.equal(await page.locator('#branch-count').textContent(), '3');
  assert.equal(await page.locator('#mount-count').textContent(), '1');
  assert.equal(await page.locator('#branch-tree').getByText('记录不可用', {exact:false}).count(), 0);
  assert.equal(await page.locator('#branch-empty .default-mark').last().textContent(), '本机默认');
  assert.equal(await page.locator('#branch-feature .default-mark').textContent(), '其他节点 location');
  const hierarchy = await page.locator('#branch-empty').evaluate(button => {
    const ids = [];
    for (let item = button.closest('.branch-item'); item; item = item.parentElement?.closest('.branch-item')) {
      ids.push(item.querySelector(':scope > .branch-button').id);
    }
    return ids;
  });
  assert.deepEqual(hierarchy, ['branch-empty','branch-feature','branch-main']);
  await page.locator('#branch-feature').click();
  assert.equal(await page.locator('#history-title').textContent(), 'feat/a');
  assert.equal(await page.locator('#branch-state').textContent(), '远端已保存');
  assert.match(await page.locator('#branch-meta').textContent(), /来源 main/);
  await page.locator('#branch-feature').dblclick();
  await page.locator('#revision-dialog[open]').waitFor();
  assert.equal(await page.locator('#branch-files-dialog[open]').count(), 0);
  assert.equal(requests.filter(url => url.pathname.includes('/branches/feature/') && /\/(files|file)$/.test(url.pathname)).length, 0);
  await page.locator('.revision-dialog-close').first().click();
  await page.locator('#refresh').click();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  assert.equal(await page.locator('#branch-empty .default-mark').last().textContent(), '本机默认');
  assert.equal(await page.locator('#mount-count').textContent(), '1');
  data.remote_error = {code:'IO', message:'remote offline'};
  await page.locator('#refresh').click();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  assert.match(await page.locator('#error').textContent(), /上次同步结果/);
  assert.equal(await page.locator('.branch-button').count(), 3);
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true);
});

function completedOperation(tool, input, result, mountReady = null) {
  return {id:'operation-'+input.request_id, request_id:input.request_id, name:tool, phase:'complete', local_saved:true,
    remote_confirmed:input.request?.durability === 'remote', mount_ready:mountReady, result, error:null, pending:null};
}

test('management navigation isolates relationship queries and preserves forms during refresh', async t => {
  const {page, requests, commands} = await open(t);
  assert.equal(requests.filter(url => url.pathname.endsWith('/relationships')).length, 0);
  assert.equal(await page.locator('#relationships-panel').isVisible(), false);
  await page.locator('#mounts-nav').click();
  await page.locator('#relationship-list tr').first().waitFor();
  assert.equal(await page.locator('#history-page').isVisible(), false);
  await page.locator('#mounts-path').fill('/projects/new directory');
  await refresh(page);
  assert.equal(await page.locator('#mounts-path').inputValue(), '/projects/new directory');
  await page.locator('#relationship-search').fill('feat/test');
  await page.locator('#relationship-mounted').selectOption('false');
  await page.locator('#relationships-panel').getByRole('button',{name:'查询',exact:true}).click();
  await page.waitForFunction(() => location.search.includes('rel_search=feat%2Ftest'));
  assert.equal(requests.filter(url => url.pathname.endsWith('/relationships')).at(-1).searchParams.get('search'),'feat/test');
  await page.reload();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  assert.equal(await page.locator('#relationship-search').inputValue(),'feat/test');
  assert.equal(await page.locator('#relationship-mounted').inputValue(),'false');
  await page.locator('#relationships-panel').getByRole('button',{name:'清空筛选'}).click();
  assert.equal(await page.locator('#relationship-search').inputValue(),'');
  const previous = requests.filter(url => url.pathname.endsWith('/relationships')).length;
  await page.locator('#branches-nav').click();
  await page.locator('#branches-list .management-row').first().waitFor();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  assert.equal(requests.filter(url => url.pathname.endsWith('/relationships')).length,previous);
  assert.equal(await page.locator('#relationships-panel').isVisible(),false);
  assert.equal(commands.length,0);
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),true);
});

test('branch creation preserves its source and switching is a separate explicit action', async t => {
  const data = fixture();
  data.command = async (route, tool, input) => {
    if (tool === 'branch_create') {
      const branch = {...data.branches[0],id:'created',name:input.request.name,source_branch:'main',base_revision:'parent-commit'};
      data.branches.push(branch);
      return route.fulfill({json:{...completedOperation(tool,input,{kind:'branch',branch}),phase:'local_saved',remote_confirmed:false}});
    }
    assert.equal(tool,'branch_switch'); data.status.selected_branch = input.request.branch;
    return route.fulfill({json:completedOperation(tool,input,{kind:'updated'})});
  };
  const {page,commands} = await open(t,data,'/branches');
  await page.locator('#branches-branch').selectOption('main');
  await page.locator('#branches-revision').selectOption('parent-commit');
  await page.locator('#branches-name').fill('feat/next-task');
  await page.locator('#branch-create-form').getByRole('button',{name:'创建分支',exact:true}).click();
  await page.locator('#branches-list [data-branch="created"]').waitFor();
  await page.getByText('本机操作已完成',{exact:true}).waitFor();
  assert.equal(await page.locator('#branches-name').isDisabled(),false);
  assert.deepEqual(commands[0].input.request,{workspace:'workspace',name:'feat/next-task',source:'parent-commit'});
  assert.equal(data.status.selected_branch,'feature');
  await page.locator('#switch-created').click();
  await page.waitForFunction(() => document.querySelector('#branches-list [data-branch="created"]').textContent.includes('本机默认分支'));
  assert.equal(commands[1].tool,'branch_switch');
  assert.notEqual(commands[0].input.request_id,commands[1].input.request_id);
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),true);
});

test('remote branch creation uses a saved revision and does not expose a writable remote mount', async t => {
  const data = fixture();
  const remote = {...data.branches[0],id:'remote',name:'feat/remote',owner:'another-location',local:false,formal_head:'remote-revision'};
  data.branches = [...data.branches,remote]; data.revisions.push(revision('remote-revision','remote',[],'commit',8));
  data.command = (route,tool,input) => route.fulfill({json:completedOperation(tool,input,{kind:'updated'})});
  const {page,commands} = await open(t,data,'/branches');
  await page.locator('#branches-list [data-branch="remote"]').getByRole('button',{name:'从此创建'}).click();
  assert.equal(await page.locator('#branches-revision').inputValue(),'remote-revision');
  assert.equal(await page.locator('#branches-revision option[value="working"]').count(),0);
  await page.locator('#branches-name').fill('feat/local-copy');
  await page.locator('#branch-create-form').getByRole('button',{name:'创建分支',exact:true}).click();
  await page.getByText('操作已完成',{exact:true}).waitFor();
  assert.equal(commands[0].input.request.source,'remote-revision');
  await page.locator('#mounts-nav').click();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  assert.equal(await page.locator('#mounts-branch option[value="remote"]').count(),0);
});

test('mount page creates, saves and unmounts a directory with the observed generation', async t => {
  const data = fixture(); data.status.capabilities.remote_configured = true;
  data.command = async (route, tool, input) => {
    if (tool === 'mount_prepare') {
      const binding = {id:'new-mount',generation:7,state:'ready',principal:'owner',...input.request};
      data.status.mounts.push(binding);
      return route.fulfill({json:completedOperation(tool,input,{kind:'mount',binding},true)});
    }
    if (tool === 'mount_unmount') data.status.mounts = [];
    return route.fulfill({json:completedOperation(tool,input,{kind:'updated'})});
  };
  const {page,commands} = await open(t,data,'/mounts');
  await page.locator('#mounts-path').fill('/projects/example');
  await page.locator('#mount-create-form').getByRole('button',{name:'创建挂载点'}).click();
  await page.locator('#relationship-list [data-mount="new-mount"]').waitFor();
  await page.getByText('操作已完成',{exact:true}).waitFor();
  assert.equal(commands[0].input.request.path,'/projects/example');
  await page.locator('#relationship-list').getByRole('button',{name:'保存版本'}).click();
  await page.getByText('远端已确认',{exact:true}).waitFor();
  assert.deepEqual(commands[1].input.request,{mount:'new-mount',expected_binding_generation:7,durability:'remote'});
  await page.locator('#unmount-new-mount').click();
  await page.waitForFunction(() => !document.querySelector('#relationship-list [data-mount="new-mount"]'));
  assert.deepEqual(commands[2].input.request,{mount:'new-mount',generation:7});
});

test('unknown command outcomes survive reload and retry the original request', async t => {
  const data = fixture(); let attempts = 0;
  data.command = async (route, tool, input) => {
    attempts++;
    if (attempts === 1) return route.abort('failed');
    return route.fulfill({json:completedOperation(tool,input,{kind:'updated'})});
  };
  const {page,commands} = await open(t,data,'/branches');
  await page.locator('#branches-name').fill('feat/retry');
  await page.locator('#branch-create-form').getByRole('button',{name:'创建分支',exact:true}).click();
  await page.getByRole('button',{name:'重试请求'}).waitFor();
  await page.waitForFunction(() => document.querySelector('#branches-page .command-error').textContent.length > 0);
  await page.reload();
  await page.getByRole('button',{name:'重试请求'}).click();
  await page.getByText('操作已完成',{exact:true}).waitFor();
  assert.deepEqual(commands[0],commands[1]);
  assert.equal(await page.evaluate(() => Object.keys(sessionStorage).filter(key => key.startsWith('agentdrive-action:')).length),1);
  await page.locator('#logout').click();
  assert.equal(await page.evaluate(() => Object.keys(sessionStorage).filter(key => key.startsWith('agentdrive-action:')).length),0);
});

test('busy branch switches report pending state and retain their request identity', async t => {
  const data = fixture(); let attempts = 0;
  data.command = (route,tool,input) => {
    const result = completedOperation(tool,input,{kind:'updated'},true);
    if (!attempts++) Object.assign(result,{phase:'waiting_for_quiesce',local_saved:false,mount_ready:false,error:{code:'MOUNT_BUSY',message:'Directory is in use',retryable:true},pending:{old:{generation:4}}});
    else data.status.selected_branch = input.request.branch;
    return route.fulfill({json:result});
  };
  const {page,commands} = await open(t,data,'/branches');
  await page.locator('#switch-main').click();
  await page.getByText('需要释放目录占用',{exact:true}).waitFor();
  assert.equal(await page.getByText('操作已完成',{exact:true}).count(),0);
  await page.getByRole('button',{name:'重试请求'}).click();
  await page.getByText('操作已完成',{exact:true}).waitFor();
  assert.deepEqual(commands[0],commands[1]);
});

test('history loads bounded pages and preserves loaded rows while new versions arrive', async t => {
  const data = fixture(), branch = data.branches[1];
  data.revisions = [...data.revisions.filter(revision => revision.branch !== branch.id),
    ...Array.from({length:1000},(_,index) => revision(`history-${index}`,branch.id,[],'commit',100+index))];
  branch.formal_head = 'history-999'; branch.autosave_head = null;
  const {page, requests} = await open(t,data);
  assert.equal(await page.locator('.revision-row').count(),50);
  assert.equal(await page.locator('#history-count').textContent(),'1000');
  await page.getByRole('button',{name:'加载更多版本',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.revision-row').length===100);
  await page.evaluate(()=>globalThis.retainedHistoryRow=document.querySelector('.revision-row'));
  const queries = () => requests.filter(url=>url.pathname.endsWith('/history'));
  const before = queries().length;
  await refresh(page);
  assert.equal(queries().length,before);
  assert.equal(await page.locator('.revision-row').count(),100);
  assert.equal(await page.evaluate(()=>globalThis.retainedHistoryRow===document.querySelector('.revision-row')),true);
  data.revisions.push(revision('new-head',branch.id,[],'commit',2000)); branch.formal_head='new-head';
  await refresh(page);
  assert.equal(await page.locator('.revision-row').count(),100);
  await page.getByRole('button',{name:'有新版本，查看更新',exact:true}).click();
  await page.locator('#revision-new-head').waitFor();
  assert.equal(await page.locator('.revision-row').count(),50);
  assert.equal(await page.locator('#history-count').textContent(),'1001');
  assert(queries().every(url=>url.searchParams.get('limit')==='50'));
});

test('management uses bounded history searches and preserves selected source revisions', async t => {
  const data = fixture(), branch = data.branches[1];
  data.revisions.push(...Array.from({length:1000},(_,index)=>revision(`source-${index}`,branch.id,[],'commit',100+index)));
  branch.formal_head='source-999'; branch.autosave_head=null;
  const {page,requests} = await open(t,data,'/mounts');
  assert.equal(requests.filter(url=>url.pathname.endsWith('/history')).length,0);
  await page.locator('#branches-nav').click();
  await page.locator('#branches-revision option[value="source-950"]').waitFor({state:'attached'});
  assert((await page.locator('#branches-revision option').count())<=52);
  await page.getByLabel('查找保存版本',{exact:true}).fill('source-5');
  await page.locator('#branches-revision option[value="source-559"]').waitFor({state:'attached'});
  await page.locator('#branches-revision').selectOption('source-559');
  await refresh(page);
  assert.equal(await page.locator('#branches-revision').inputValue(),'source-559');
  assert(requests.some(url=>url.pathname.endsWith('/history') && url.searchParams.get('search')==='source-5'));
});

test('source selection survives switching branches with the same inherited revision', async t => {
  const data = fixture(), sibling = {...data.branches[2],id:'sibling',name:'sibling'};
  data.branches.push(sibling);
  const {page} = await open(t,data,'/branches');
  await page.locator('#branches-branch').selectOption('empty');
  await page.locator('#branches-revision').selectOption('parent-commit');
  await page.locator('#branches-branch').selectOption('sibling');
  await page.locator('#branches-revision option[value="parent-commit"]').waitFor({state:'attached'});
  assert.equal(await page.locator('#branches-revision').inputValue(),'working');
  assert.equal(await page.getByRole('button',{name:'创建分支',exact:true}).isEnabled(),true);
});

test('a selected inherited revision stays selected when the branch head advances', async t => {
  const data = fixture(), branch = data.branches[2];
  branch.local=false; branch.owner='remote';
  const {page} = await open(t,data,'/branches');
  await page.locator('#branches-branch').selectOption('empty');
  assert.equal(await page.locator('#branches-revision').inputValue(),'parent-commit');
  await page.locator('#branches-name').fill('feat/continue');
  data.revisions.push(revision('child-commit','empty',['parent-commit'],'commit',10));
  branch.formal_head='child-commit';
  await refresh(page);
  await page.locator('#branches-revision option[value="child-commit"]').waitFor({state:'attached'});
  assert.equal(await page.locator('#branches-revision').inputValue(),'parent-commit');
  assert.equal(await page.locator('#branches-name').inputValue(),'feat/continue');
});

test('read-only mounts of a transferred branch pin its current published revision', async t => {
  const data = fixture(), branch = data.branches[1];
  branch.local=false; branch.local_available=true; branch.owner='other-location';
  branch.autosave_head=null; branch.formal_head='remote-current';
  data.command=(route,tool,input)=>route.fulfill({json:completedOperation(tool,input,{kind:'updated'},true)});
  const {page,commands} = await open(t,data,'/mounts');
  await page.locator('#mounts-access').selectOption('read_only');
  await page.locator('#mounts-branch').selectOption('feature');
  await page.locator('#mounts-path').fill('/workspace/reference');
  await page.getByRole('button',{name:'创建挂载点',exact:true}).click();
  await page.getByText('操作已完成',{exact:true}).waitFor();
  assert.equal(commands[0].input.request.revision,'remote-current');
  assert.equal(commands[0].input.request.branch,'feature');
});

test('changing the source branch discards an older history response', async t => {
  const {page} = await open(t,fixture(),'/branches');
  let started, release, done;
  const pending = new Promise(resolve=>{started=resolve;});
  const blocked = new Promise(resolve=>{release=resolve;});
  const completed = new Promise(resolve=>{done=resolve;});
  t.after(()=>release());
  await page.route('**/branches/main/history?**',async route=>{
    started(); await blocked;
    await route.fulfill({json:{entries:[revision('stale-source','main',[],'commit',100)],next:null,total:1}}).catch(()=>{});
    done();
  });
  await page.locator('#branches-branch').selectOption('main'); await pending;
  await page.locator('#branches-branch').selectOption('empty');
  release(); await completed;
  assert.equal(await page.locator('#branches-branch').inputValue(),'empty');
  assert.equal(await page.locator('#branches-revision option[value="stale-source"]').count(),0);
  assert.equal(await page.locator('#branches-revision').inputValue(),'working');
});

test('history and archive use daemon synchronization state even when no jobs are queued', async t => {
  const data = fixture(); data.status.capabilities.remote_configured=true;
  data.status.synchronization={state:'unsaved',branches:[{branch:'feature',local_saved:false,remote_confirmed:false}]};
  const {page} = await open(t,data);
  assert.equal(await page.locator('#sync-status').textContent(),'有未保存修改');
  data.status.synchronization.state='unconfirmed'; await refresh(page);
  assert.equal(await page.locator('#sync-status').textContent(),'等待远端确认');
  data.status.synchronization.state='confirmed'; await refresh(page);
  assert.equal(await page.locator('#sync-status').textContent(),'已同步');
});
