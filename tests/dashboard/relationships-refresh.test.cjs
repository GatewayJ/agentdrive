const {test, before, after} = require('node:test');
const assert = require('node:assert/strict');
const {createServer} = require('node:http');
const {readFile} = require('node:fs/promises');
const path = require('node:path');
const {chromium} = require('playwright');

let browser, server, endpoint;
before(async () => {
  server = createServer(async (request, response) => {
    if (request.url === '/relationships.js') {
      response.writeHead(200, {'Content-Type':'text/javascript'}).end(await readFile(path.resolve(__dirname, '../../crates/agentdrive-mcp/dashboard/relationships.js')));
      return;
    }
    response.writeHead(200, {'Content-Type':'text/html'}).end(`<!doctype html><style>td{padding:12px}body{font:16px sans-serif}table{width:100%}</style><div id="relationships"></div><script type="module">
      import {createRelationshipsView} from './relationships.js';
      const element = (tag, className, text) => { const node = document.createElement(tag); if (className) node.className = className; if (text !== undefined) node.textContent = text; return node; };
      const api = async (url, options) => { const response = await fetch(url, options); const result = await response.json(); if (!response.ok) throw Object.assign(new Error(result.error.message), {code:result.error.code}); return result; };
      window.view = createRelationshipsView({api,element,container:document.getElementById('relationships')});
    </script>`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  endpoint = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({headless:true});
});
after(async () => {
  await browser?.close();
  if (server) await new Promise(resolve => server.close(resolve));
});

function fixture() {
  const branch = {id:'branch', name:'main', owner:'local', state:'writable'};
  const snapshot = {workspace:{id:'workspace'}, branches:[branch], remote_branches:[], location:'local'};
  const entries = Array.from({length:51}, (_, index) => ({
    branch:{...branch,ownership:{location:'local',source:'local_authority',authority_epoch:1,generation:1},origin:{workspace_root:true,status:'available'}},
    mount:{id:`mount-${index}`,path:`/workspace/${index}`,principal:'owner',access:'read_only',state:'ready',generation:1}, mount_location:'local',
  }));
  const result = {workspace:'workspace',observing_location:'local',selected_branch:'branch',total:entries.length};
  return {snapshot, entries, result};
}
async function open(t) {
  const data = fixture(), page = await browser.newPage({viewport:{width:1200,height:600}});
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  t.after(async () => { await page.close(); assert.deepEqual(errors, []); });
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    if (data.intercept && await data.intercept(route, url)) return;
    const cursor = url.searchParams.get('cursor');
    await route.fulfill({json:{...data.result,entries:cursor ? data.entries.slice(50) : data.entries.slice(0,50),next:cursor ? null : 'page-2'}});
  });
  await page.goto(endpoint);
  await page.waitForFunction(() => window.view);
  await page.evaluate(snapshot => window.view.show(snapshot), data.snapshot);
  await page.locator('#relationship-more').click();
  await page.waitForFunction(() => document.querySelectorAll('#relationship-list tr').length === 51);
  return {page, data};
}

test('background refresh retains loaded pages, open details and scroll position', async t => {
  const {page, data} = await open(t);
  const last = page.locator('#relationship-list tr').last();
  await last.scrollIntoViewIfNeeded();
  await last.locator('details').first().evaluate(node => { node.open = true; });
  const scroll = await page.evaluate(() => window.scrollY);
  let release, started;
  const blocked = new Promise(resolve => { release = resolve; });
  const waiting = new Promise(resolve => { started = resolve; });
  data.intercept = async (route, url) => { if (!url.searchParams.has('cursor')) { started(); await blocked; } return false; };
  const refreshing = page.evaluate(snapshot => window.view.show(snapshot), data.snapshot);
  await waiting;
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
  release(); await refreshing;
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
  assert.equal(await last.locator('details').first().evaluate(node => node.open), true);
  assert.equal(await page.evaluate(() => window.scrollY), scroll);
});

test('failed refresh keeps previous records available and provides retry', async t => {
  const {page, data} = await open(t);
  data.intercept = async route => { await route.fulfill({status:503,json:{error:{code:'UNAVAILABLE',message:'temporary outage'}}}); return true; };
  await page.evaluate(snapshot => window.view.show(snapshot), data.snapshot);
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
  assert.match(await page.locator('#relationship-status').textContent(), /保留当前列表/);
  assert.equal(await page.locator('#relationship-reload').isVisible(), true);
  delete data.intercept;
  await page.locator('#relationship-reload').click();
  await page.waitForFunction(() => document.querySelector('#relationship-reload').hidden);
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
});

test('a changed pagination cursor keeps the current view until reload succeeds', async t => {
  const {page, data} = await open(t);
  data.intercept = async (route, url) => {
    if (!url.searchParams.has('cursor')) return false;
    await route.fulfill({status:409,json:{error:{code:'TARGET_MOVED',message:'relationships changed'}}}); return true;
  };
  data.entries[0].mount.path = '/workspace/changed';
  await page.evaluate(snapshot => window.view.show(snapshot), data.snapshot);
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
  assert.equal(await page.locator('#relationship-list tr').first().locator('.mount-path').textContent(), '/workspace/0');
  assert.match(await page.locator('#relationship-status').textContent(), /关系已变化/);
  delete data.intercept;
  await page.locator('#relationship-reload').click();
  await page.waitForFunction(() => document.querySelector('#relationship-reload').hidden);
  assert.equal(await page.locator('#relationship-list tr').count(), 51);
  assert.equal(await page.locator('#relationship-list tr').first().locator('.mount-path').textContent(), '/workspace/changed');
  assert.match(await page.locator('#relationship-status').textContent(), /关系已更新/);
});
