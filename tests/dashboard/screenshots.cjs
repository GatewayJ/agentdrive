// Deterministic demo data rendered by the shipped dashboard modules.
const {chromium} = require('playwright');
const {createServer} = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname,'../..');
const workspace = '8b700001-0000-4000-8000-000000000001';
const ids = ['86a70001-0000-4000-8000-000000000001','38b20002-0000-4000-8000-000000000002','58c30003-0000-4000-8000-000000000003'];
const revisionIds = Array.from({length:6},(_,i)=>`a6bd000${i}-0000-4000-8000-000000000001`);
const names = ['main','analysis/competitors','analysis/customers'];
const branches = ids.map((id,i)=>({id,workspace,name:names[i],owner:'local',authority_epoch:1,generation:1,mutation_seq:10+i,saved_seq:10+i,
 formal_head:revisionIds[i+3],autosave_head:null,working_root:{id:'tree-'+i},base_revision:revisionIds[0],source_branch:i ? ids[0] : null,dirty:false,state:'writable'}));
const messages = ['建立项目资料与任务说明','补充用户访谈与市场数据','确认并行分析的任务范围','汇总调研结果，准备异地交接','完成竞品分析，附上证据来源','完成用户需求分析与建议'];
const revisions = revisionIds.map((id,i)=>({id,workspace,branch:i>3 ? ids[i-3] : ids[0],kind:'commit',parents:i ? [revisionIds[i>3 ? 0 : i-1]] : [],actor:i>3 ? 'research-agent' : 'owner',root:{id:'tree-'+i},message:messages[i],created_ns:1792312200000000000+i*60000000000}));
const status = {workspace:{id:workspace,name:'市场调研'},location:'local',selected_branch:ids[0],branches,
 mounts:ids.map((id,i)=>({id:'mount-'+i,branch:id,path:'/projects/market-research/'+['main','competitors','customers'][i],state:'ready',access:'read_write'})),pending_jobs:0,capabilities:{remote_configured:true},
 remote_branches:branches.map(branch=>({branch:branch.id,owner:branch.owner,authority_epoch:1,generation:1,durable_seq:branch.saved_seq,working_root:branch.working_root}))};
const view = {status,revisions,summary:{branch_count:3,mount_count:3,ready_mount_count:3,mounted_branch_count:3},origins:branches.map((branch,i)=>({branch:branch.id,source_branch:branch.source_branch,revision:revisionIds[0],revision_available:true,workspace_root:i===0}))};
const content = {
 '/README.md':'# 市场调研\n\n面向下一阶段产品规划，整理市场资料并完成竞品和用户分析。\n\n## 项目资料\n\n- HANDOFF.md：交接说明与下一步\n- reports：各项分析成果\n- data：输入数据和访谈记录\n',
 '/HANDOFF.md':'# 下一位 Agent，从这里继续\n\n本轮调研已完成，资料与分析结果已保存。请在此基础上继续制定产品建议。\n\n## 已经完成\n\n- [x] 整理市场数据与用户访谈\n- [x] 完成竞品分析与用户需求分析\n- [x] 合并成果，并检查引用来源\n\n## 下一步任务\n\n1. 阅读 reports 中的两份分析报告。\n2. 结合 data 中的数据，提出三个优先解决的问题。\n3. 将建议写入 proposal.md，注明判断依据。\n\n## 关键资料\n\n| 文件 | 用途 |\n| --- | --- |\n| reports/竞品分析.md | 产品定位与差异比较 |\n| reports/用户需求.md | 访谈结论与需求排序 |\n| data/市场数据.csv | 原始数据与统计依据 |\n\n> 接手前拉取远端最新内容，完成后保存版本并确认远端同步。\n',
 '/tasks.md':'# 任务清单\n\n- [x] 资料整理\n- [x] 并行分析\n- [ ] 产品建议\n',
 '/reports/竞品分析.md':'# 竞品分析\n\n研究三个代表产品的使用场景与工作流程。\n',
 '/reports/用户需求.md':'# 用户需求\n\n优先改善跨机器资料交接与工作成果查看。\n',
 '/data/市场数据.csv':'category,count\nteam,42\nindividual,68\n',
 '/.metadata':'demo',
};
function nodes(directory) {
 const all = [...Object.keys(content),'/reports','/data'];
 return all.filter(name=>(name.slice(0,name.lastIndexOf('/')) || '/')===directory).map(name=>({path:name,dirty:false,inode:{kind:content[name]===undefined ? 'directory' : 'file',mode:420,size:Buffer.byteLength(content[name] || '')}}));
}
(async()=>{
 const server = createServer(async(req,res)=>{
  const url = new URL(req.url,'http://localhost');
  const json = value => res.writeHead(200,{'Content-Type':'application/json'}).end(JSON.stringify(value));
  if (url.pathname==='/api/workspaces') return json([status.workspace]);
  if (url.pathname===`/api/workspaces/${workspace}/archive`) return json(status);
  if (url.pathname===`/api/workspaces/${workspace}`) return json(view);
  if (url.pathname.endsWith('/files')) { const branch=branches.find(b=>url.pathname.includes(b.id));return json({version:`${branch.id}:1:1:${branch.mutation_seq}`,next:null,entries:nodes(url.searchParams.get('path'))}); }
  if (url.pathname.endsWith('/file')) { const bytes=Buffer.from(content[url.searchParams.get('path')] || ''); const offset=Number(url.searchParams.get('offset')),size=Number(url.searchParams.get('size')); return res.writeHead(200,{'Content-Type':'application/octet-stream'}).end(bytes.subarray(offset,offset+size)); }
  const name=['/archive','/dashboard','/'].includes(url.pathname) ? 'index.html' : url.pathname.slice(1);
  if (!/^[a-z.-]+$/.test(name)) return res.writeHead(404).end();
  try { const bytes=await fs.readFile(path.join(root,'crates/agentdrive-mcp/dashboard',name)); res.writeHead(200,{'Content-Type':{'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'}[path.extname(name)]}).end(bytes); }
  catch {res.writeHead(404).end();}
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true});
 try {
  const page=await browser.newPage({viewport:{width:1600,height:1120},deviceScaleFactor:1}); const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.addInitScript(()=>sessionStorage.setItem('agentdrive-token','demo-token'));
  const endpoint=`http://127.0.0.1:${server.address().port}`;
  await page.goto(endpoint+'/archive?'+new URLSearchParams({workspace,branch:ids[0]}));
  await page.locator('#archive-page .branch-file[data-path="/HANDOFF.md"]').click();
  await page.getByRole('heading',{name:'下一位 Agent，从这里继续'}).waitFor();
  await fs.mkdir(path.join(root,'docs/images'),{recursive:true});
  assert.equal(await page.locator('#archive-page .branch-file.active').getAttribute('data-path'),'/HANDOFF.md');
  await page.mouse.move(10,10);
  await page.screenshot({path:path.join(root,'docs/images/archive.png')});
  await page.goto(endpoint+'/dashboard?'+new URLSearchParams({workspace,branch:ids[0]}));
  await page.getByText('汇总调研结果，准备异地交接',{exact:true}).waitFor();
  await page.screenshot({path:path.join(root,'docs/images/branches.png')});
  assert.deepEqual(errors,[]); console.log('Product screenshots generated from demo data.');
 } finally {await browser.close();await new Promise(r=>server.close(r));}
})().catch(error=>{console.error(error);process.exit(1)});
