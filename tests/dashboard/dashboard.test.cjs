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
    const name = request.url === '/dashboard' ? 'index.html' : request.url.slice(1);
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
    source_branch:null, working_root:{id:'tree-parent-commit'}, dirty:false, mutation_seq:1, saved_seq:1, state:'writable'};
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
async function open(t, data = fixture()) {
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
    } else if (url.pathname === '/api/workspaces/workspace') {
      await route.fulfill({json:data});
    } else if (url.pathname.endsWith('/changes')) {
      await route.fulfill({json:{total:2, next:null, entries:[{path:'/first.txt', kind:'modified'}, {path:'/second.txt', kind:'added'}]}});
    } else if (url.pathname.endsWith('/diff')) {
      const inode = {kind:'file', size:5, mode:420, uid:1000, gid:1000, content:{id:'content'}};
      await route.fulfill({json:{status:'text', before:inode, after:inode,
        hunks:[[{kind:'added', before_line:null, after_line:1, text:'hello', newline:true}]]}});
    } else await route.fulfill({status:404, json:{error:{message:'Unknown test endpoint'}}});
  });
  await page.goto(endpoint+'/dashboard');
  await page.locator('#branch-feature').waitFor();
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
  assert.equal(await page.locator('dialog').evaluate(el => el.open), false);
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
  const bounds = await page.locator('dialog').boundingBox();
  await page.mouse.click(bounds.x - 4, bounds.y + 5);
  assert.equal(await page.locator('dialog').evaluate(el => el.open), false);
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
  assert.equal(await page.locator('dialog').evaluate(el => el.open), true);
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
  assert.equal(await page.locator('dialog').evaluate(el => el.open), true);
  await page.setViewportSize({width:390, height:844});
  const layout = await page.locator('dialog').evaluate(el => ({width:el.clientWidth, scrollWidth:el.scrollWidth,
    left:el.getBoundingClientRect().left, right:el.getBoundingClientRect().right}));
  assert(layout.scrollWidth <= layout.width && layout.left >= 0 && layout.right <= 390);
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'origin-main-parent-commit');
});
