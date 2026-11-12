// Browser interaction checks against a disposable API fixture. The packaged
// native worker/lifecycle fixture separately proves actual app replacement.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile, mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {resolve, dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = '/api/v1/companion/update';
const csrf = 'disposable-browser-csrf', password = 'disposable browser password';
const assets = new Map([['/updates','native-updates.html'],['/native-updates.mjs','native-updates.mjs'],['/native-updates.css','native-updates.css'],['/companion.css','companion.css']]);
const checks = [], errors = [], unexpected = [];
let fixture;
function fresh() {
  return {authenticated:true,mode:'normal',owner:'standalone',current:{version:'1.0.0'},check:null,operation:null,
    actions:{check:true,apply:false,recover:false,rollback:false},requests:new Map(),posts:[],fail:null,phase:'accepted'};
}
function status(url) {
  const id = url.searchParams.get('request_id'), saved = id ? fixture.requests.get(id) : null;
  return {supported:true,owner:fixture.owner,mode:fixture.mode,current:fixture.current,check:fixture.check,
    operation:saved || fixture.operation,actions:fixture.actions,
    ...(id ? {request:{id,state:saved?'recorded':'not_recorded',...(saved?{operation_id:saved.id}:{})}} : {})};
}
const server = createServer(async(req,res) => {
  try {
    const url = new URL(req.url, 'http://fixture');
    if (assets.has(url.pathname)) {
      const name=assets.get(url.pathname);res.setHeader('Content-Type',name.endsWith('.mjs')?'text/javascript':name.endsWith('.css')?'text/css':'text/html');res.end(await readFile(resolve(repo,'ui',name)));return;
    }
    const reply=(code,value)=>{res.writeHead(code,{'Content-Type':'application/json'});res.end(JSON.stringify(value));};
    if (url.pathname==='/auth/session') {reply(200,{authenticated:fixture.authenticated,admin:fixture.authenticated,owner:'fixture',auth_mode:'local_password',csrf:fixture.authenticated?csrf:undefined});return;}
    let bytes='';for await(const chunk of req) bytes+=chunk;
    const body=bytes?JSON.parse(bytes):{};
    if(req.method==='POST') fixture.posts.push({path:url.pathname,body});
    if(url.pathname===root+'/apply' && fixture.fail==='unauthorized-before-record') {fixture.fail=null;fixture.authenticated=false;reply(401,{error:'Sign in required'});return;}
    if(url.pathname==='/auth/login') {assert.equal(body.password,password);fixture.authenticated=true;reply(200,{authenticated:true,admin:true,owner:'fixture',auth_mode:'local_password',csrf});return;}
    if(!fixture.authenticated) {reply(401,{error:'Sign in required'});return;}
    if(req.method==='POST') assert.equal(req.headers['x-csrf-token'],csrf);
    if(url.pathname==='/auth/logout') {fixture.authenticated=false;reply(200,{});return;}
    if(url.pathname===root && req.method==='GET') {reply(200,status(url));return;}
    if(url.pathname===root+'/check') {
      assert.equal(body.package_path,'/disposable/Companion package');fixture.check={id:randomUUID(),version:'1.1.0',available:true,summary:'Complete package verified.'};fixture.actions.apply=true;reply(200,status(url));return;
    }
    if(url.pathname===root+'/apply' || url.pathname===root+'/recover') {
      const prior=fixture.requests.get(body.request_id);
      if(prior) {reply(200,{...status(url),operation:prior});return;}
      if(fixture.fail==='before-record') {fixture.fail=null;reply(503,{error:'Reply unavailable'});return;}
      const isApply=url.pathname.endsWith('/apply');
      if(isApply) assert.equal(body.check_id,fixture.check.id);
      else assert.equal(body.operation_id,fixture.operation.id);
      const freshRollback=!isApply && body.action==='rollback' && ['completed','restored'].includes(fixture.operation?.phase);
      fixture.operation={id:freshRollback?randomUUID():fixture.operation?.id || randomUUID(),transaction_id:freshRollback?randomUUID():fixture.operation?.transaction_id || randomUUID(),request_id:body.request_id,
        phase:isApply?fixture.phase:'completed',outcome:isApply?null:body.action==='rollback'?'rolled_back':'applied',message:'Internal worker transaction diagnostic'};
      fixture.requests.set(body.request_id,fixture.operation);
      fixture.actions={check:false,apply:false,recover:fixture.phase==='unknown',rollback:false};
      if(fixture.fail==='after-record') {fixture.fail=null;reply(503,{error:'Reply unavailable'});return;}
      if(fixture.fail==='close-after-record') {fixture.fail=null;req.socket.destroy();return;}
      reply(200,status(url));return;
    }
    unexpected.push(req.method+' '+url.pathname);reply(404,{error:'Unexpected fixture route'});
  } catch(e) {errors.push(e.stack);res.writeHead(500,{'Content-Type':'application/json'});res.end(JSON.stringify({error:'Fixture failed'}));}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const origin='http://127.0.0.1:'+server.address().port;
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
let context,page;
async function open() {
  if(context) await context.close();
  fixture=fresh();context=await browser.newContext({viewport:{width:1100,height:850}});page=await context.newPage();
  page.on('pageerror',e=>errors.push(e.message));
  await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
}
async function goto() {await page.goto(origin+'/updates');await page.locator('#updates-content').waitFor({state:'visible'});}
async function check() {await page.locator('#update-package-path').fill('/disposable/Companion package');await page.locator('#update-check').click();await page.locator('#update-apply').waitFor({state:'visible'});}
async function screenshot(name) {if(process.env.ARTIFACT_DIR){await mkdir(process.env.ARTIFACT_DIR,{recursive:true});await page.screenshot({path:resolve(process.env.ARTIFACT_DIR,name),fullPage:true});}}
async function refresh() {await page.locator('#update-refresh').click();await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);}
try {
  await open();await goto();await check();
  await screenshot('updates-package-desktop.png');await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(()=>document.body.scrollWidth<=innerWidth),true);
  assert.ok(await page.locator('#return-to-companion').evaluate(el=>parseFloat(getComputedStyle(el).fontSize)>=13));
  await screenshot('updates-package-phone.png');
  await page.locator('#update-apply').click();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update requested');
  assert.equal(await page.locator('#update-progress-detail').innerText(),'Checking that Companion can safely stop.');
  const operation=fixture.operation,posts=fixture.posts.length;
  await page.reload();await page.locator('#update-progress').waitFor({state:'visible'});
  assert.equal(fixture.posts.length,posts);assert.equal(fixture.operation.transaction_id,operation.transaction_id);
  checks.push('Package check and apply; reload reads the same operation without another mutation');

  fixture.operation.phase='failed';fixture.operation.outcome='rolled_back';
  fixture.actions={check:true,apply:false,recover:false,rollback:false};
  await refresh();
  assert.equal(await page.locator('#update-progress-title').innerText(),'Update wasn’t installed');
  assert.equal(await page.locator('#update-progress-detail').innerText(),'The previous version was restored. You can keep using Companion.');
  assert.equal(await page.locator('#update-resume').isVisible(),false);
  checks.push('A failed update with verified rollback says the previous app is restored without offering unavailable recovery');

  await open();await goto();await check();fixture.fail='after-record';fixture.phase='unknown';
  await page.locator('#update-apply').click();await page.locator('#update-resume').waitFor({state:'visible'});
  await page.waitForFunction(()=>!document.querySelector('#update-resume').disabled);
  assert.equal(fixture.posts.filter(p=>p.path.endsWith('/apply')).length,1);
  const lost=fixture.operation.transaction_id;
  await page.reload();await page.locator('#update-resume').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update complete');
  assert.equal(fixture.operation.transaction_id,lost);
  assert.equal(fixture.posts.filter(p=>p.path.endsWith('/recover')).length,1);
  checks.push('Lost reply after durable admission reconciles once; explicit recovery retains the transaction');

  await open();await goto();await check();fixture.fail='close-after-record';fixture.phase='unknown';
  await page.locator('#update-apply').click();await page.locator('#update-resume').waitFor({state:'visible'});
  await page.waitForFunction(()=>!document.querySelector('#update-resume').disabled);
  const transportRequests=fixture.posts.filter(p=>p.path.endsWith('/apply')).map(p=>JSON.stringify(p.body));
  assert.equal(new Set(transportRequests).size,1);assert.equal(fixture.requests.size,1);
  checks.push('Socket closure and any browser transport retransmission keep one immutable request and one operation');

  await open();await goto();await check();fixture.fail='before-record';
  await page.locator('#update-apply').click();await page.locator('#update-retry-request').waitFor({state:'visible'});
  const original=fixture.posts.find(p=>p.path.endsWith('/apply')).body;
  const retained=await page.evaluate(()=>sessionStorage.getItem('companion-native-update-request'));
  assert.ok(retained.includes(original.request_id));
  for(const value of [csrf,password,'/disposable/Companion package']) assert.equal(retained.includes(value),false);
  await page.reload();await page.locator('#update-retry-request').waitFor({state:'visible'});
  assert.equal(fixture.posts.filter(p=>p.path.endsWith('/apply')).length,1);
  await page.locator('#update-retry-request').click();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update requested');
  assert.deepEqual(fixture.posts.filter(p=>p.path.endsWith('/apply')).map(p=>p.body),[original,original]);
  checks.push('Unknown pre-admission reply survives reload; deliberate retry uses the identical request ID and payload');

  await open();await goto();await check();fixture.fail='before-record';
  await page.locator('#update-apply').click();await page.locator('#update-retry-request').waitFor({state:'visible'});
  const delayed=fixture.posts.find(p=>p.path.endsWith('/apply')).body;
  await page.locator('#update-sign-out').click();await page.locator('#update-login').waitFor({state:'visible'});
  assert.equal(await page.locator('#updates-content').isVisible(),false);
  await page.locator('#update-password').fill(password);await page.locator('#update-sign-in').click();
  await page.locator('#update-retry-request').waitFor({state:'visible'});await page.reload();
  await page.locator('#update-retry-request').waitFor({state:'visible'});
  assert.equal(await page.locator('#update-apply').isEnabled(),false);
  assert.equal(JSON.parse(await page.evaluate(()=>sessionStorage.getItem('companion-native-update-request'))).request_id,delayed.request_id);
  fixture.operation={id:randomUUID(),transaction_id:randomUUID(),request_id:delayed.request_id,phase:'accepted'};
  fixture.requests.set(delayed.request_id,fixture.operation);fixture.actions={check:false,apply:false,recover:false,rollback:false};
  await refresh();assert.equal(fixture.posts.filter(p=>p.path.endsWith('/apply')).length,1);
  assert.equal(await page.locator('#update-retry-request').isVisible(),false);
  checks.push('Sign-out/login/reload preserves an unresolved request; a late original admission is reconciled without another UUID');

  await open();await goto();await check();fixture.fail='before-record';
  await page.locator('#update-apply').click();await page.locator('#update-retry-request').waitFor({state:'visible'});
  const uncertain=fixture.posts.find(p=>p.path.endsWith('/apply')).body;
  fixture.fail='unauthorized-before-record';await page.locator('#update-retry-request').click();
  await page.locator('#update-login').waitFor({state:'visible'});
  await page.locator('#update-password').fill(password);await page.locator('#update-sign-in').click();
  await page.locator('#update-retry-request').waitFor({state:'visible'});await page.reload();
  await page.locator('#update-retry-request').waitFor({state:'visible'});
  assert.equal(JSON.parse(await page.evaluate(()=>sessionStorage.getItem('companion-native-update-request'))).request_id,uncertain.request_id);
  fixture.operation={id:randomUUID(),transaction_id:randomUUID(),request_id:uncertain.request_id,phase:'accepted'};
  fixture.requests.set(uncertain.request_id,fixture.operation);fixture.actions={check:false,apply:false,recover:false,rollback:false};
  await refresh();
  assert.deepEqual(fixture.posts.filter(p=>p.path.endsWith('/apply')).map(p=>p.body),[uncertain,uncertain]);
  checks.push('A rejected retry cannot erase the earlier unknown request; reauthentication reconciles its original UUID');

  await open();fixture.mode='recovery';fixture.authenticated=false;
  fixture.operation={id:randomUUID(),transaction_id:randomUUID(),request_id:randomUUID(),phase:'unknown'};fixture.actions={check:false,apply:false,recover:true,rollback:false};
  await page.goto(origin+'/updates');await page.locator('#update-login').waitFor({state:'visible'});
  assert.equal(await page.locator('#update-username').isVisible(),false);
  await page.locator('#update-password').fill(password);await page.locator('#update-sign-in').click();await page.locator('#update-resume').waitFor({state:'visible'});
  assert.equal(await page.locator('#update-package-form').isVisible(),false);assert.equal(await page.locator('#update-rollback').isVisible(),false);
  assert.equal(await page.locator('#return-to-companion').isVisible(),false);assert.equal(await page.locator('#update-password').inputValue(),'');
  assert.equal(await page.locator('a[href="/companion"]:visible').count(),0);
  await screenshot('updates-recovery-desktop.png');
  checks.push('Recovery uses the existing password and offers only server-authorized recovery, without normal work navigation');

  await open();fixture.owner='enrolled';fixture.actions={check:true,apply:true,recover:true,rollback:true};await goto();
  assert.equal(await page.locator('#update-package-form').isVisible(),false);
  for(const id of ['update-apply','update-resume','update-rollback']) assert.equal(await page.locator('#'+id).isVisible(),false);
  assert.match(await page.locator('#update-owner').innerText(),/existing updater/);
  assert.equal(fixture.posts.length,0);checks.push('Enrolled installation never exposes a competing standalone updater');

  await open();fixture.operation={id:randomUUID(),transaction_id:randomUUID(),request_id:randomUUID(),phase:'completed',outcome:'applied'};fixture.actions.rollback=true;await goto();
  const completed=structuredClone(fixture.operation);
  await page.locator('#update-rollback').click();assert.equal(fixture.posts.length,0);
  await page.locator('#update-cancel-rollback').click();assert.equal(fixture.posts.length,0);
  await page.locator('#update-rollback').click();await page.locator('#update-confirm-rollback').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored');
  assert.equal(fixture.posts.length,1);assert.equal(fixture.posts[0].body.action,'rollback');
  assert.equal(fixture.posts[0].body.operation_id,completed.id);assert.notEqual(fixture.operation.transaction_id,completed.transaction_id);
  checks.push('Completed-update rollback requires confirmation, references the prior operation, and receives a fresh transaction');

  await open();await goto();fixture.authenticated=false;await refresh().catch(()=>{});
  await page.locator('#update-login').waitFor({state:'visible'});
  assert.equal(await page.locator('#updates-content').isVisible(),false);checks.push('Expired sign-in clears private update presentation');
  await open();fixture.current.version='<img src=x onerror=alert(1)>';await goto();
  assert.equal(await page.locator('#update-installed img').count(),0);assert.match(await page.locator('#update-installed').textContent(),/<img/);
  checks.push('Version and operation fields are text, never interpreted markup');
  assert.deepEqual(errors,[]);assert.deepEqual(unexpected,[]);
  process.stdout.write(JSON.stringify({status:'passed',checks,page_errors:errors,limitations:['Browser/API fixture only; actual worker/OS launch/update qualification remains separate','No provider/media/model calls']})+'\n');
} finally {if(context)await context.close();await browser.close();await new Promise(r=>server.close(r));}
