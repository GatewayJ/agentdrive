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
    const name = ['/', '/dashboard', '/archive', '/settings'].includes(pathname) ? 'index.html' : pathname.slice(1);
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
  const main = {id:'main', name:'main', formal_head:'parent-commit', autosave_head:null, base_revision:'initial',
    source_branch:null, working_root:{id:'tree-parent-commit'}, dirty:false, mutation_seq:1, saved_seq:1, state:'writable', authority_epoch:1, generation:1};
  const child = {...main, id:'feature', name:'feature', source_branch:'main', base_revision:'parent-commit',
    autosave_head:'child-autosave', working_root:{id:'tree-child-autosave'}, dirty:true};
  const empty = {...main, id:'empty', name:'empty', source_branch:'main', base_revision:'parent-commit'};
  return {
    status:{workspace:{id:'workspace', name:'Review workspace'}, location:'local', selected_branch:'feature',
      branches:[main, child, empty], mounts:[], capabilities:{remote_configured:false}, pending_jobs:0},
    revisions,
    origins:[main, child, empty].map(branch => ({branch:branch.id, source_branch:branch.source_branch,
      revision:branch.base_revision, revision_available:true, workspace_root:branch.id === 'main'})),
    summary:{branch_count:3, mount_count:0, ready_mount_count:0, mounted_branch_count:0},
  };
}
async function open(t, data = fixture(), target = '/dashboard') {
  const page = await browser.newPage({viewport:{width:1440, height:1000}});
  const errors = [], requests = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(async () => { await page.close(); assert.deepEqual(errors, []); });
  await page.addInitScript(() => sessionStorage.setItem('agentdrive-token', 'test-token'));
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    requests.push(url);
    if (url.pathname === '/api/workspaces') {
      await route.fulfill({json:[{id:'workspace', name:'Review workspace'}]});
    } else if (url.pathname === '/api/workspaces/workspace/archive') {
      await route.fulfill({json:data.status});
    } else if (url.pathname === '/api/workspaces/workspace') {
      await route.fulfill({json:data});
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
  await page.locator(target.startsWith('/archive') ? '.archive-branch[data-branch="feature"]' : '#branch-feature').waitFor();
  await page.waitForFunction(() => !document.querySelector('#refresh').disabled);
  return {page, data, requests, branch:data.status.branches[1]};
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
  assert.equal(await page.locator('#history-count').textContent(), '0');
  assert.equal(await page.locator('.no-revisions').textContent(), '此分支尚未生成新版本。');
  await page.locator('#branch-main').click();
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

test('archive remote confirmation requires matching branch state even with an empty publication queue', async t => {
  const data = fixture(), branch = data.status.branches[1];
  branch.owner = 'local'; data.status.capabilities.remote_configured = true;
  data.status.remote_branches = [{branch:branch.id,owner:'local',authority_epoch:1,generation:1,durable_seq:1,working_root:branch.working_root}];
  const {page} = await open(t,data,'/archive');
  const remote = page.locator('.archive-state').nth(1).locator('strong');
  assert.equal(await remote.textContent(),'远端已保存');
  branch.mutation_seq++;
  await page.locator('#refresh').click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await remote.textContent(),'等待远端确认');
  assert.equal(await page.locator('.archive-state').first().locator('strong').textContent(),'有未保存修改');
  data.status.pending_jobs = 1;
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
