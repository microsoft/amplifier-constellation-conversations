// Packaged one-action Update through the candidate's restricted local listener.
// The Python parent owns actual manager observations and terminal cleanup.
import assert from 'node:assert/strict';
import {lstat,readdir,writeFile} from 'node:fs/promises';
import {isAbsolute,resolve} from 'node:path';
import {createInterface} from 'node:readline';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const input=createInterface({input:process.stdin,crlfDelay:Infinity}),frames=input[Symbol.asyncIterator]();
async function readFrame(timeout=30000){let timer;try{const next=await Promise.race([frames.next(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Controller checkpoint acknowledgement timed out')),timeout);})]);assert(!next.done);assert(Buffer.byteLength(next.value,'utf8')<=32768);return JSON.parse(next.value);}finally{clearTimeout(timer);}}
const fixture=await readFrame(),root='/api/v1/companion/update';
assert.equal(fixture.type,'init');const origin=new URL(fixture.url);assert.equal(origin.protocol,'http:');assert.equal(origin.hostname,'127.0.0.1');assert.equal(origin.origin,fixture.url);
assert.match(fixture.offer_id,/^[a-f0-9-]{36}$/);assert.equal(fixture.ack_target_seconds,5);assert.equal(fixture.request_seconds,40);assert.equal(fixture.background_seconds,360);
assert(isAbsolute(fixture.artifact_dir));assert.equal(resolve(fixture.artifact_dir),fixture.artifact_dir);
const artifactIdentity=await lstat(fixture.artifact_dir);assert(artifactIdentity.isDirectory()&&!artifactIdentity.isSymbolicLink());assert.deepEqual(await readdir(fixture.artifact_dir),[]);
async function assertArtifactIdentity(){const value=await lstat(fixture.artifact_dir);assert(value.isDirectory()&&!value.isSymbolicLink());assert.equal(value.dev,artifactIdentity.dev);assert.equal(value.ino,artifactIdentity.ino);}
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
const context=await browser.newContext({viewport:{width:1100,height:850}}),page=await context.newPage();
const errors=[],forbidden=[],mutations=[],progress=[],checks=[];let parentId=null;
await context.addInitScript(()=>{globalThis.__updateMediaCalls=0;if(navigator.mediaDevices)navigator.mediaDevices.getUserMedia=async()=>{globalThis.__updateMediaCalls++;throw Error('No media in the update fixture');};});
page.on('pageerror',error=>errors.push(error.message));
page.on('request',request=>{const url=new URL(request.url());if(url.origin!==origin.origin||(url.pathname.startsWith('/api/')&&!url.pathname.startsWith(root)))forbidden.push(request.method()+' '+url.pathname);if(request.method()==='POST'&&url.pathname.startsWith(root))mutations.push({path:url.pathname,body:request.postDataJSON()});});
await page.route('**/*',route=>new URL(route.request().url()).origin===origin.origin?route.continue():route.abort());
page.on('response',async response=>{try{const url=new URL(response.url());if(response.request().method()==='GET'&&url.pathname===root&&response.ok()){const value=await response.json(),u=value.update;if(u&&u.id===parentId&&progress.length<64){const row={id:u.id,state:u.state,phase:u.phase,revision:u.revision,operation_id:u.operation_id??null};if(JSON.stringify(progress.at(-1))!==JSON.stringify(row))progress.push(row);}}}catch{/* A dropped observation grants no outcome. */}});
async function snapshot(id){const suffix=new URLSearchParams({offer_id:fixture.offer_id});if(id)suffix.set('request_id',id);const response=await context.request.get(fixture.url+root+'?'+suffix,{timeout:10000,maxRedirects:0});assert.equal(response.status(),200);return response.json();}
async function checkpoint(phase,value){process.stdout.write(JSON.stringify({type:'checkpoint',phase,snapshot:value})+'\n');const ack=await readFrame(60000);assert.equal(ack.type,'ack');assert.equal(ack.phase,phase);if(phase!=='authenticated')assert.equal(ack.transaction_id,value.operation.transaction_id);}
async function screenshot(name){await assertArtifactIdentity();const bytes=await page.screenshot({fullPage:true});await assertArtifactIdentity();await writeFile(resolve(fixture.artifact_dir,name),bytes,{flag:'wx',mode:0o600});await assertArtifactIdentity();}
try{
  await page.goto(fixture.url+'/updates?offer='+fixture.offer_id);
  await page.locator('#update-login').waitFor({state:'visible'});await page.locator('#update-password').fill(fixture.password);await page.locator('#update-sign-in').click();
  await page.locator('#update-start').waitFor({state:'visible'});const before=await snapshot();assert.equal(before.assistant,true);assert.equal(before.owner,'standalone');assert.equal(before.current.version,fixture.previous_version);assert.equal(before.offer.id,fixture.offer_id);assert.equal(before.offer.version,fixture.candidate_version);assert.equal(before.actions.advanced_package,false);
  assert.equal(await page.locator('#update-check,#update-apply').count(),0);assert.equal(await page.locator('#update-package-form').isVisible(),false);await checkpoint('authenticated',before);
  let resolveAck,rejectAck;const acknowledgement=new Promise((resolve,reject)=>{resolveAck=resolve;rejectAck=reject;});let intercepted=false;
  await page.route('**/api/v1/companion/update/request',async route=>{
    if(intercepted){await route.abort('failed');rejectAck(Error('A second Update POST was attempted'));return;}intercepted=true;
    const started=performance.now();let reply;
    try{const response=await route.fetch({timeout:40000,maxRetries:0,maxRedirects:0});const elapsed=(performance.now()-started)/1000;assert.equal(response.status(),202);reply=await response.json();await response.dispose();parentId=route.request().postDataJSON().request_id;assert.equal(reply.update.id,parentId);
      const evidence={type:'acknowledgement',request_id:parentId,elapsed_seconds:elapsed,target_seconds:5,http_status:202,reply_discarded:true,mutation_retries:0};process.stdout.write(JSON.stringify(evidence)+'\n');await route.abort('failed');assert(elapsed<5,'Durable Update acknowledgement exceeded its separate five-second target');resolveAck({reply,evidence});
    }catch(error){try{await route.abort('failed');}catch{}rejectAck(error);}
  });
  const applyDeadline=performance.now()+360000;
  await page.locator('#update-start').click();const ack=await acknowledgement;
  await page.waitForFunction(id=>document.querySelector('#update-reference-value').textContent===id,parentId,{timeout:15000});
  await page.reload();await page.locator('#updates-content').waitFor({state:'visible'});
  await page.waitForFunction(id=>document.querySelector('#update-reference-value').textContent===id,parentId,{timeout:15000});
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update complete',null,{timeout:Math.max(1,applyDeadline-performance.now())});
  assert(performance.now()<=applyDeadline,'Apply exceeded the existing background allowance');
  const applied=await snapshot(parentId);assert.equal(applied.update.id,parentId);assert.equal(applied.update.state,'completed');assert.equal(applied.operation.id,applied.update.operation_id);assert.equal(applied.operation.outcome,'applied');assert.equal(applied.current.version,fixture.candidate_version);assert.equal(mutations.filter(m=>m.path===root+'/request').length,1);
  checks.push('One durable Update acknowledgement under five seconds; deliberately lost reply and reload retain the same parent with no replay');
  await screenshot('single-update-applied.png');await checkpoint('applied',applied);
  await page.locator('#update-advanced>summary').click();await page.locator('#update-rollback').waitFor({state:'visible'});await page.locator('#update-rollback').click();assert.equal(mutations.filter(m=>m.path===root+'/recover').length,0);const rollbackDeadline=performance.now()+360000;await page.locator('#update-confirm-rollback').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored',null,{timeout:Math.max(1,rollbackDeadline-performance.now())});
  assert(performance.now()<=rollbackDeadline,'Rollback exceeded the existing background allowance');
  const rollback=mutations.find(m=>m.path===root+'/recover');assert(rollback);assert.equal(rollback.body.operation_id,applied.operation.id);const restored=await snapshot(rollback.body.request_id);
  assert.equal(restored.operation.request_id,rollback.body.request_id);assert.equal(restored.operation.outcome,'rolled_back');assert.notEqual(restored.operation.id,applied.operation.id);assert.notEqual(restored.operation.transaction_id,applied.operation.transaction_id);assert.equal(restored.current.version,fixture.previous_version);
  await page.reload();await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored',null,{timeout:15000});assert.equal(mutations.filter(m=>m.path===root+'/recover').length,1);
  await screenshot('single-update-rolled-back.png');await checkpoint('rolled_back',restored);
  assert.equal(mutations.length,2);assert.deepEqual(mutations.map(m=>m.path),[root+'/request',root+'/recover']);assert.equal(await page.evaluate(()=>globalThis.__updateMediaCalls),0);assert.deepEqual(errors,[]);assert.deepEqual(forbidden,[]);
  assert.equal(await page.evaluate(password=>document.body.innerText.includes(password)||location.href.includes(password)||JSON.stringify(sessionStorage).includes(password)||JSON.stringify(localStorage).includes(password),fixture.password),false);
  checks.push('Explicit rollback is a separately identified native child, restores the supplied previous package and survives reload');
  const closeResponse=await context.request.get(fixture.url+root+'/assistant-close',{timeout:10000,maxRedirects:0});assert.equal(closeResponse.status(),200);const close=await closeResponse.json();assert.equal(close.can_close,true);assert.deepEqual(close.identity,fixture.assistant_identity);
  const authResponse=await context.request.get(fixture.url+'/auth/session',{timeout:10000,maxRedirects:0});assert.equal(authResponse.status(),200);const auth=await authResponse.json();assert.equal(auth.authenticated,true);
  const closeEvidence={type:'assistant_close',state:'intent_recorded',attempts:1};process.stdout.write(JSON.stringify(closeEvidence)+'\n');
  try{const response=await context.request.post(fixture.url+root+'/assistant-close',{data:{listener_nonce:close.identity.nonce},headers:{Origin:fixture.url,'X-CSRF-Token':auth.csrf},timeout:40000,maxRetries:0,maxRedirects:0});assert.equal(response.status(),200);const value=await response.json();assert.equal(value.closing,true);assert.equal(value.listener_nonce,close.identity.nonce);closeEvidence.state='reply_received';}
  catch(error){if(error.name==='AssertionError')throw error;closeEvidence.state='transport_unknown';}
  process.stdout.write(JSON.stringify(closeEvidence)+'\n');
  process.stdout.write(JSON.stringify({type:'result',status:'passed',scope:'managed_actual_browser_single_update',checks,assistant_close:closeEvidence,acknowledgement:ack.evidence,before,applied,restored,progress,mutation_ids:mutations.map(m=>({path:m.path,request_id:m.body.request_id,operation_id:m.body.operation_id??null,offer_id:m.body.offer_id??null})),mutation_retries:0,media_calls:0,provider_calls:0,page_errors:errors})+'\n');
}finally{input.close();await context.close();await browser.close();}
