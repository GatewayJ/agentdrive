const {test, before, after} = require('node:test');
const assert = require('node:assert/strict');
const {createServer} = require('node:http');
const {readFile} = require('node:fs/promises');
const path = require('node:path');
const {chromium} = require('playwright');

let server, browser, endpoint;
before(async () => {
  const source = path.resolve(__dirname, '../../crates/agentdrive-mcp/dashboard/commands.js');
  server = createServer(async (request, response) => {
    if (request.url === '/commands.js') {
      response.writeHead(200, {'Content-Type':'text/javascript'}).end(await readFile(source));
      return;
    }
    response.writeHead(200, {'Content-Type':'text/html'}).end(`<!doctype html><input id="draft"><main></main>
      <script type="module">
        import {createCommandPanel} from '/commands.js';
        const element = (tag, className, text) => {
          const node = document.createElement(tag); if (className) node.className = className;
          if (text !== undefined) node.textContent = text; return node;
        };
        const api = async (url, options) => {
          const response = await fetch(url, options), value = await response.json();
          if (!response.ok) throw Object.assign(new Error(value.error.message), {status:response.status});
          return value;
        };
        window.changeCalls = 0;
        window.commandPanel = createCommandPanel({api, element, container:document.querySelector('main'), scope:'test',
          changed:async () => { window.changeCalls++; }, setBusy:value => { document.querySelector('#draft').disabled = value; }});
        window.commandPanel.show('workspace');
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

async function open(t, handle) {
  const page = await browser.newPage();
  const calls = [], errors = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(async () => { await page.close(); assert.deepEqual(errors, []); });
  await page.route('**/api/commands/**', async route => {
    const tool = new URL(route.request().url()).pathname.split('/').at(-1);
    const body = route.request().postDataJSON(); calls.push({tool, body});
    await handle(route, tool, body);
  });
  await page.goto(endpoint);
  await page.waitForFunction(() => window.commandPanel);
  return {page, calls};
}

function receipt(phase, recovery = null, pending = null) {
  const complete = phase === 'complete';
  return {id:'operation', phase, local_saved:complete, remote_confirmed:false, mount_ready:complete,
    pending, recovery, error:complete || phase === 'cancelled' ? null : {code:'IO', message:'Temporary failure', retryable:false}};
}
const retryAction = {kind:'retry_request'};
const releaseAction = {kind:'release_mount', expected_binding_generation:7};
const pendingMount = {kind:'mount_change', old:{generation:7}, new:null};
const unmountRequest = {mount:'mount', generation:7};

async function run(page, tool, request = {}) {
  await page.evaluate(({tool,request}) => window.commandPanel.run(tool,request,'Operation'), {tool,request});
}
async function finished(page) {
  await page.getByText('操作已完成', {exact:true}).waitFor();
  await page.waitForFunction(() => !document.querySelector('#draft').disabled);
}

test('server recovery action enables branch switch retry after a nonretryable error', async t => {
  let attempts = 0;
  const {page,calls} = await open(t, route => route.fulfill({json:attempts++ ? receipt('complete') : receipt('recovery_required',retryAction,{kind:'branch_switch'})}));
  await run(page,'branch_switch',{workspace:'workspace',branch:'target'});
  assert.equal(await page.locator('#draft').isDisabled(),true);
  await page.getByRole('button',{name:'重试请求'}).click();
  await finished(page);
  assert.deepEqual(calls[1],calls[0]);
});

test('a new mount without an old binding can retry a recoverable driver failure', async t => {
  let attempts = 0;
  const {page,calls} = await open(t, route => route.fulfill({json:attempts++ ? receipt('complete') : receipt('waiting_for_quiesce',retryAction,{kind:'mount_change',old:null,new:{generation:7}})}));
  await run(page,'mount_prepare',{workspace:'workspace',path:'/projects/new',durability:'local'});
  assert.equal(await page.getByRole('button',{name:'已关闭占用，继续操作'}).isVisible(),false);
  await page.getByRole('button',{name:'重试请求'}).click();
  await finished(page);
  assert.deepEqual(calls[1],calls[0]);
});

test('failed release attempts retain their identity and recover after reload', async t => {
  let releases = 0, complete = false;
  const pending = receipt('waiting_for_quiesce',releaseAction,pendingMount);
  const {page,calls} = await open(t, (route,tool) => {
    if (tool === 'mount_release') {
      complete = releases++ > 0;
      return route.fulfill({json:receipt(complete ? 'complete' : 'failed')});
    }
    return route.fulfill({json:complete ? receipt('complete') : pending});
  });
  await run(page,'mount_unmount',unmountRequest);
  const release = page.getByRole('button',{name:'已关闭占用，继续操作'});
  await release.click();
  await page.waitForFunction(() => document.querySelector('.command-error').textContent.includes('Temporary failure') && ![...document.querySelectorAll('button')].find(button=>button.textContent==='已关闭占用，继续操作').disabled);
  assert.equal(await page.getByRole('button',{name:'重试请求'}).isVisible(),false);
  await page.reload();
  await release.click();
  await finished(page);
  const attempts = calls.filter(value=>value.tool==='mount_release');
  assert.equal(attempts.length,2);
  assert.deepEqual(attempts[1],attempts[0]);
  assert.deepEqual(attempts[0].body.request,{operation:'operation',expected_binding_generation:7});
  assert.equal(calls.filter(value=>value.tool==='mount_unmount').length,1);
});

test('a lost release acknowledgement reuses the recorded request after reload', async t => {
  let attempts = 0, complete = false;
  const {page,calls} = await open(t, (route,tool) => {
    if (tool === 'mount_release') {
      complete = true;
      if (!attempts++) return route.abort('failed');
      return route.fulfill({json:receipt('complete')});
    }
    return route.fulfill({json:complete ? receipt('complete') : receipt('waiting_for_quiesce',releaseAction,pendingMount)});
  });
  await run(page,'mount_unmount',unmountRequest);
  await page.getByRole('button',{name:'已关闭占用，继续操作'}).click();
  await page.waitForFunction(() => document.querySelector('.command-error').textContent.length > 0);
  await page.reload();
  await page.getByRole('button',{name:'已关闭占用，继续操作'}).click();
  await finished(page);
  const releases = calls.filter(value=>value.tool==='mount_release');
  assert.equal(releases.length,2);
  assert.deepEqual(releases[1],releases[0]);
});

test('a restored legacy receipt fetches recovery actions before continuing', async t => {
  const pending = receipt('recovery_required',retryAction,{kind:'branch_switch'});
  const {page,calls} = await open(t, (route,tool) => route.fulfill({json:tool==='operation_get' ? pending : receipt('complete')}));
  const legacy = {...pending}; delete legacy.recovery;
  await page.evaluate(record => sessionStorage.setItem('agentdrive-action:test:workspace', JSON.stringify({
    tool:'branch_switch', request:{workspace:'workspace',branch:'target'}, request_id:'original-request', label:'Operation',record,
  })),legacy);
  await page.reload();
  await page.getByRole('button',{name:'重试请求'}).click();
  await finished(page);
  assert.equal(calls[0].tool,'operation_get');
  assert.equal(calls[1].body.request_id,'original-request');
});

test('committed mount recovery uses its advertised binding generation', async t => {
  let complete = false;
  const pending = {...receipt('committed_mount_pending',releaseAction,{kind:'apply_revision',mount:{generation:7}}),local_saved:true};
  const {page,calls} = await open(t, (route,tool) => {
    if (tool==='mount_release') complete=true;
    return route.fulfill({json:complete ? receipt('complete') : pending});
  });
  await run(page,'branch_restore',{durability:'local'});
  await page.getByRole('button',{name:'恢复挂载',exact:true}).click();
  await finished(page);
  assert.equal(calls.find(value=>value.tool==='mount_release').body.request.expected_binding_generation,7);
});

test('completed, cancelled and terminally failed receipts do not offer retries', async t => {
  let phase = 'complete';
  const {page} = await open(t, route => route.fulfill({json:{...receipt(phase),error:phase==='failed' ? {message:'Rejected',retryable:true} : null}}));
  for (phase of ['complete','cancelled','failed']) {
    await run(page,'branch_create',{name:'next'});
    assert.equal(await page.locator('#draft').isDisabled(),false);
    assert.equal(await page.getByRole('button',{name:'重试请求'}).isVisible(),false);
    assert.equal(await page.getByRole('button',{name:'恢复挂载',exact:true}).isVisible(),false);
  }
});
