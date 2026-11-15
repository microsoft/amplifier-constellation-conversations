// Browser interaction checks against a disposable API fixture. Native worker
// and package qualification remain separate; no media or agent work is run.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {resolve,dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const repo=resolve(dirname(fileURLToPath(import.meta.url)),'..'),root='/api/v1/companion/update';
const csrf='disposable-browser-csrf',password='disposable browser password';
const assets=new Map([['/updates','native-updates.html'],['/native-updates.mjs','native-updates.mjs'],['/native-updates.css','native-updates.css'],['/companion.css','companion.css']]);
const checks=[],errors=[],unexpected=[];let fixture;
function fresh(){return {authenticated:true,admin:true,mode:'normal',owner:'standalone',current:{version:'1.0.0'},offer:{id:randomUUID(),version:'1.1.0',current_version:'1.0.0'},update:null,operation:null,actions:{request:true,recover:false,rollback:false},requests:new Map(),controls:new Map(),posts:[],gets:[],fail:null};}
function status(url){
  const id=url.searchParams.get('request_id'),controlId=url.searchParams.get('control_id'),saved=id?fixture.requests.get(id):null;
  return {supported:true,owner:fixture.owner,mode:fixture.mode,current:fixture.current,actions:fixture.actions,operation:fixture.operation,
    ...(!fixture.legacy?{update:saved||fixture.update}:{}),offer:!fixture.legacy&&url.searchParams.get('offer_id')===fixture.offer.id?fixture.offer:null,
    ...(id?{request:{id,state:saved||fixture.operation?.request_id===id?'recorded':'not_recorded'}}:{}),
    ...(controlId?{control:fixture.controls.get(controlId)||{id:controlId,status:'not_recorded'}}:{})};
}
function accepted(id,state='accepted'){return {id,state,phase:state,message:'Saved update progress.',version:'1.1.0',revision:1,can_cancel:state==='accepted',can_continue:false};}
const server=createServer(async(req,res)=>{
  try{
    const url=new URL(req.url,'http://fixture');
    if(assets.has(url.pathname)){const name=assets.get(url.pathname);res.setHeader('Content-Type',name.endsWith('.mjs')?'text/javascript':name.endsWith('.css')?'text/css':'text/html');res.end(await readFile(resolve(repo,'ui',name)));return;}
    const reply=(code,value)=>{res.writeHead(code,{'Content-Type':'application/json'});res.end(JSON.stringify(value));};
    const session=()=>({authenticated:fixture.authenticated,admin:fixture.admin&&fixture.authenticated,owner:'fixture',auth_mode:'local_password',csrf:fixture.authenticated?csrf:undefined});
    if(url.pathname==='/auth/session'){reply(200,session());return;}
    let bytes='';for await(const chunk of req)bytes+=chunk;const body=bytes?JSON.parse(bytes):{};
    if(req.method==='POST')fixture.posts.push({path:url.pathname,body});else fixture.gets.push(url);
    if(url.pathname==='/auth/login'){assert.equal(body.password,password);fixture.authenticated=true;reply(200,session());return;}
    if(!fixture.authenticated){reply(401,{error:'Sign in required'});return;}
    if(req.method==='POST')assert.equal(req.headers['x-csrf-token'],csrf);
    if(url.pathname==='/auth/logout'){fixture.authenticated=false;reply(200,{});return;}
    if(url.pathname===root&&req.method==='GET'){if(fixture.fail==='offline'){reply(503,{error:'Unavailable'});return;}reply(200,status(url));return;}
    if(url.pathname===root+'/request'){
      if(fixture.requests.has(body.request_id)){reply(202,status(url));return;}
      if(fixture.fail==='before-record'){fixture.fail=null;reply(503,{error:'Reply unavailable'});return;}
      assert.equal(Boolean(body.offer_id),!body.package_path);if(body.offer_id)assert.equal(body.offer_id,fixture.offer.id);else assert.equal(body.package_path,'/disposable/Companion package');
      fixture.update=accepted(body.request_id);fixture.requests.set(body.request_id,fixture.update);
      if(fixture.fail==='after-record'){fixture.fail=null;reply(503,{error:'Reply unavailable'});return;}
      reply(202,status(url));return;
    }
    if([root+'/cancel',root+'/continue'].includes(url.pathname)){
      assert.equal(body.request_id,fixture.update.id);assert.match(body.control_id,/^[a-f0-9-]{36}$/);
      if(fixture.fail==='control-before-record'){fixture.fail=null;reply(503,{error:'Reply unavailable'});return;}
      const action=url.pathname.split('/').at(-1),control={id:body.control_id,action,status:'recorded',outcome:{saved:true}};
      fixture.controls.set(control.id,control);Object.assign(fixture.update,{state:action==='cancel'?'cancelled':'preparing',phase:'prepare',revision:fixture.update.revision+1,can_cancel:action!=='cancel',can_continue:false});
      reply(202,{...status(url),control});return;
    }
    if(url.pathname===root+'/recover'){
      assert.equal(body.operation_id,fixture.operation.id);assert(['resume','rollback'].includes(body.action));
      fixture.operation={id:randomUUID(),transaction_id:randomUUID(),request_id:body.request_id,phase:'completed',outcome:body.action==='rollback'?'rolled_back':'applied'};
      reply(200,status(url));return;
    }
    unexpected.push(req.method+' '+url.pathname);reply(404,{error:'Unexpected fixture route'});
  }catch(e){errors.push(e.stack);res.writeHead(500,{'Content-Type':'application/json'});res.end(JSON.stringify({error:'Fixture failed'}));}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});let context,page;
async function open(){if(context)await context.close();fixture=fresh();context=await browser.newContext({viewport:{width:1100,height:850}});page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());}
async function goto(offer=true){await page.goto(origin+'/updates'+(offer?'?offer='+fixture.offer.id:''));await page.locator('#updates-content').waitFor({state:'visible'});await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);}
async function refresh(){await page.locator('#update-refresh').click();await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);}
async function begin(){await page.locator('#update-start').click();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update requested');await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);}
async function screenshot(name){if(process.env.ARTIFACT_DIR){await mkdir(process.env.ARTIFACT_DIR,{recursive:true});await page.screenshot({path:resolve(process.env.ARTIFACT_DIR,name),fullPage:true});}}
try{
  await open();await goto();assert.equal(await page.locator('#update-start').innerText(),'Update');assert.equal(await page.locator('#update-package-form').isVisible(),false);
  await screenshot('updates-offer-desktop.png');await page.setViewportSize({width:390,height:844});assert.equal(await page.evaluate(()=>document.body.scrollWidth<=innerWidth),true);await screenshot('updates-offer-phone.png');
  await begin();const id=fixture.update.id,posts=fixture.posts.length;await page.reload();await page.locator('#update-progress').waitFor({state:'visible'});
  assert.equal(fixture.posts.length,posts);assert.equal(fixture.gets.at(-1).searchParams.get('request_id'),id);assert.equal(await page.locator('#update-reference-value').textContent(),id);
  checks.push('One Update from the opened offer; reload observes the original saved request without Check or Apply');

  Object.assign(fixture.update,{state:'waiting',message:'Waiting for your active call to finish.',can_cancel:true});await refresh();assert.equal(await page.locator('#update-progress-title').innerText(),'Waiting for current activity');
  assert.equal(await page.locator('#update-progress-detail').innerText(),'Waiting for your active call to finish.');await page.locator('#update-cancel').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update cancelled');const cancel=fixture.posts.at(-1).body;assert.equal(cancel.request_id,id);assert.notEqual(cancel.control_id,id);
  checks.push('Waiting uses saved human-readable progress; explicit Cancel has its own receipt and keeps the original Update ID');

  await open();await goto();await begin();const failedId=fixture.update.id;Object.assign(fixture.update,{state:'failed',can_cancel:false,can_continue:true});fixture.actions.request=false;await refresh();
  assert.equal(await page.locator('#update-start').isVisible(),false);assert.equal(await page.locator('#update-continue').isVisible(),true);
  fixture.actions.request=true;await refresh();await page.locator('#update-start').click();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update requested');
  assert.notEqual(fixture.update.id,failedId);assert.equal(fixture.posts.length,2);
  checks.push('A failed update offers a new request only after explicit server admission, independently of Continue');

  await open();await goto();fixture.fail='after-record';await begin();assert.equal(fixture.posts.filter(p=>p.path===root+'/request').length,1);
  Object.assign(fixture.update,{state:'needs_attention',message:'A saved work result needs review.',can_cancel:false,can_continue:true});await refresh();await page.locator('#update-continue').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Preparing the update');assert.equal(fixture.posts.filter(p=>p.path===root+'/continue').length,1);
  checks.push('Lost Update reply reconciles by GET; Continue is offered only when the installation advertises it');

  await open();await goto();await begin();fixture.fail='control-before-record';await page.locator('#update-cancel').click();await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);
  const uncertainControl=fixture.posts.at(-1).body;await page.reload();await page.locator('#update-progress').waitFor({state:'visible'});assert.equal(fixture.posts.length,2);
  assert.equal(await page.locator('#update-cancel').isDisabled(),true);assert.equal(fixture.gets.at(-1).searchParams.get('control_id'),uncertainControl.control_id);
  fixture.controls.set(uncertainControl.control_id,{id:uncertainControl.control_id,action:'cancel',status:'recorded',outcome:{cancelled:true}});Object.assign(fixture.update,{state:'cancelled',can_cancel:false});await refresh();
  assert.equal(await page.locator('#update-request-status').isVisible(),false);assert.equal(fixture.posts.length,2);
  checks.push('Lost Cancel reply persists across reload and resolves only through its exact control receipt, with no resend');

  await open();await goto();fixture.fail='before-record';await page.locator('#update-start').click();await page.locator('#update-request-status').waitFor({state:'visible'});await page.waitForFunction(()=>!document.querySelector('#update-refresh').disabled);
  const original=fixture.posts[0].body,stored=await page.evaluate(()=>JSON.stringify(sessionStorage));assert.ok(stored.includes(original.request_id));assert(!stored.includes(csrf));assert(!stored.includes(password));
  await page.locator('#update-sign-out').click();await page.locator('#update-login').waitFor({state:'visible'});assert.equal(await page.locator('#updates-content').isVisible(),false);
  await page.locator('#update-password').fill(password);await page.locator('#update-sign-in').click();await page.locator('#updates-content').waitFor({state:'visible'});await page.reload();await page.locator('#update-request-status').waitFor({state:'visible'});
  assert.equal(fixture.posts.filter(p=>p.path===root+'/request').length,1);assert.equal(await page.locator('#update-start').isDisabled(),true);
  fixture.update=accepted(original.request_id);fixture.requests.set(original.request_id,fixture.update);await refresh();assert.equal(await page.locator('#update-request-status').isVisible(),false);
  checks.push('Not-recorded stays unconfirmed through sign-out/login/reload; late admission keeps the same request without retry');

  fixture.fail='offline';await refresh();assert.equal(await page.locator('#update-connection').isVisible(),true);fixture.fail=null;Object.assign(fixture.update,{state:'unknown',message:'The worker result is not confirmed.',can_cancel:false,can_continue:false});await refresh();
  assert.equal(await page.locator('#update-progress-title').innerText(),'Update result unconfirmed');assert.equal(await page.locator('#update-connection').isVisible(),false);assert.equal(await page.locator('#update-cancel').isVisible(),false);assert.equal(await page.locator('#update-continue').isVisible(),false);
  await screenshot('updates-attention-phone.png');checks.push('Reconnect preserves saved progress; unknown state exposes no inferred success or unsupported action');

  await open();await goto(false);await page.locator('#update-advanced>summary').click();await page.locator('#update-package-path').fill('/disposable/Companion package');await page.locator('#update-package-submit').click();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update requested');
  assert.equal(fixture.posts.length,1);assert.equal(fixture.posts[0].path,root+'/request');assert.equal(fixture.posts[0].body.package_path,'/disposable/Companion package');assert.equal((await page.evaluate(()=>JSON.stringify(sessionStorage))).includes('/disposable/'),false);
  checks.push('Advanced folder input requests the same background flow in one action without storing the local path');

  await open();fixture.mode='recovery';fixture.authenticated=false;fixture.operation={id:randomUUID(),transaction_id:randomUUID(),phase:'unknown'};fixture.actions={request:false,recover:true,rollback:true};
  await page.goto(origin+'/updates');await page.locator('#update-login').waitFor({state:'visible'});assert.equal(await page.locator('#update-username').isVisible(),false);
  await page.locator('#update-password').fill(password);await page.locator('#update-sign-in').click();await page.locator('#update-resume').waitFor({state:'visible'});
  assert.equal(await page.locator('#return-to-companion').isVisible(),false);assert.equal(await page.locator('#update-package-form').isVisible(),false);assert.equal(await page.locator('#update-password').inputValue(),'');await screenshot('updates-recovery-desktop.png');
  const beforeRollback=fixture.posts.length,operation=fixture.operation.id;await page.locator('#update-rollback').click();await page.locator('#update-cancel-rollback').click();assert.equal(fixture.posts.length,beforeRollback);
  await page.locator('#update-rollback').click();await page.locator('#update-confirm-rollback').click();await page.waitForFunction(()=>document.querySelector('#update-recovery-status').textContent.startsWith('Previous version restored'));
  assert.equal(fixture.posts.at(-1).body.operation_id,operation);assert.equal(fixture.posts.at(-1).body.action,'rollback');
  checks.push('Existing authenticated recovery and deliberate rollback remain available under Advanced, without normal-work navigation');

  await open();await goto();await begin();const completedParent=fixture.update.id,completedChild=randomUUID();Object.assign(fixture.update,{state:'completed',can_cancel:false,operation_id:completedChild});fixture.operation={id:completedChild,phase:'completed',outcome:'applied'};fixture.actions.rollback=true;await refresh();
  await page.locator('#update-advanced>summary').click();await page.locator('#update-rollback').click();await page.locator('#update-confirm-rollback').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored');const rollbackRequest=fixture.posts.at(-1).body;
  assert.equal(fixture.update.id,completedParent);assert.equal(fixture.update.operation_id,completedChild);assert.notEqual(fixture.operation.id,completedChild);
  await page.reload();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored');assert.equal(fixture.gets.at(-1).searchParams.get('request_id'),rollbackRequest.request_id);assert.equal(fixture.posts.length,2);
  checks.push('An explicit rollback remains foreground across reload even while its original parent still says completed');

  await open();await goto();await begin();const childId=randomUUID();Object.assign(fixture.update,{state:'installing',operation_id:childId,can_cancel:false});await refresh();
  const persisted=JSON.parse(await page.evaluate(()=>sessionStorage.getItem('companion-native-update')));assert.equal(persisted.operation_id,childId);
  fixture.legacy=true;fixture.update=null;fixture.operation={id:randomUUID(),phase:'restored',outcome:'rolled_back'};fixture.actions={recover:true,rollback:true};await refresh();
  assert.equal(await page.locator('#update-progress-title').innerText(),'Update result unconfirmed');assert.equal(await page.locator('#update-package-form').isVisible(),false);
  await page.locator('#update-advanced>summary').click();assert.equal(await page.locator('#update-resume').isDisabled(),true);
  fixture.operation.id=childId;await refresh();assert.equal(await page.locator('#update-progress-title').innerText(),'Previous version restored');
  const beforeLegacyReload=fixture.posts.length;await page.reload();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored');assert.equal(fixture.posts.length,beforeLegacyReload);
  checks.push('Cached new page recognizes only its previously received native child after old-schema rollback, including reload; unrelated results stay unconfirmed');

  await open();fixture.owner='enrolled';await goto();assert.equal(await page.locator('#update-start').isVisible(),false);assert.equal(await page.locator('#update-advanced').isVisible(),false);assert.match(await page.locator('#update-owner').innerText(),/existing updater/);assert.equal(fixture.posts.length,0);
  checks.push('Enrolled installations retain their existing updater owner');
  await open();await context.addInitScript(()=>{Storage.prototype.setItem=()=>{throw Error('storage unavailable');};});await goto();await page.locator('#update-start').click();await page.locator('#update-error').waitFor({state:'visible'});assert.equal(fixture.posts.length,0);
  checks.push('Missing durable browser reference storage refuses a new request before any POST');
  await open();fixture.current.version='<img src=x onerror=alert(1)>';await goto();assert.equal(await page.locator('#update-installed img').count(),0);assert.match(await page.locator('#update-installed').textContent(),/<img/);
  checks.push('Server strings are rendered as text');
  assert.deepEqual(errors,[]);assert.deepEqual(unexpected,[]);assert.equal(await page.locator('#update-check,#update-apply,#update-retry-request').count(),0);
  process.stdout.write(JSON.stringify({status:'passed',checks,page_errors:errors,limitations:['Browser/API fixture only; actual background worker/OS install qualification is separate','No native manager, media, provider or model calls']})+'\n');
}finally{if(context)await context.close();await browser.close();await new Promise(r=>server.close(r));}
