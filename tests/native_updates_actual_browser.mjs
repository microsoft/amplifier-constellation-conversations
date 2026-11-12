// Against a disposable *packaged* Companion supplied by the native controller.
// This driver cannot create calls, prompts, tasks, or arbitrary host commands.
// The parent independently compares all protected installation/work state.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
let input='';for await (const chunk of process.stdin) input+=chunk;
const fixture=JSON.parse(input);input='';
assert.equal(new URL(fixture.url).hostname,'127.0.0.1');
assert.equal(typeof fixture.candidate,'string');
assert.ok(fixture.candidate.length);
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
const context=await browser.newContext({viewport:{width:1100,height:850}}),page=await context.newPage();
if(fixture.cookies) await context.addCookies(fixture.cookies.map(cookie=>({name:cookie.name,value:cookie.value,url:fixture.url,httpOnly:true,sameSite:'Strict'})));
const errors=[],forbidden=[],mutations=[],checks=[];
const root='/api/v1/companion/update';
await context.addInitScript(()=>{
  globalThis.__updateMediaCalls=0;
  if(navigator.mediaDevices) navigator.mediaDevices.getUserMedia=async()=>{globalThis.__updateMediaCalls++;throw new Error('Update test cannot request media');};
});
page.on('pageerror',e=>errors.push(e.message));
page.on('request',request=>{
  const url=new URL(request.url());
  if(url.origin!==new URL(fixture.url).origin || (url.pathname.startsWith('/api/') && !url.pathname.startsWith(root))) forbidden.push(request.method()+' '+url.pathname);
  if(request.method()==='POST' && url.pathname.startsWith(root)) mutations.push({path:url.pathname,body:request.postDataJSON()});
});
async function snapshot() {
  const response=await context.request.get(fixture.url+root,{timeout:10000});
  assert.equal(response.status(),200);return response.json();
}
async function screenshot(name) {
  if(fixture.artifact_dir){await mkdir(fixture.artifact_dir,{recursive:true});await page.screenshot({path:resolve(fixture.artifact_dir,name),fullPage:true});}
}
try {
  await page.goto(fixture.url+'/updates');
  if(!fixture.cookies) {
    await page.locator('#update-login').waitFor({state:'visible'});
    await page.locator('#update-password').fill(fixture.password);await page.locator('#update-sign-in').click();
  }
  await page.locator('#updates-content').waitFor({state:'visible'});
  const before=await snapshot();assert.equal(before.owner,'standalone');assert.equal(before.mode,'normal');
  assert.equal(before.actions.check,true);
  await page.locator('#update-package-path').fill(fixture.candidate);await page.locator('#update-check').click();
  await page.locator('#update-apply').waitFor({state:'visible',timeout:60000});
  const checked=await snapshot();assert.equal(checked.check.available,true);assert.ok(checked.check.id);
  checks.push('Administrator checks the actual complete candidate package through the browser');
  let accepted=null,dropReply=true;
  await page.route('**/api/v1/companion/update/apply',async route=>{
    if(!dropReply){await route.continue();return;}
    dropReply=false;
    const response=await route.fetch({timeout:60000});
    assert.ok(response.status()>=200 && response.status()<300);
    accepted=await response.json();await response.dispose();
    // Simulate loss only after the real installation durably accepted the
    // request. There is no new mutation or repeated native process launch.
    await route.abort('failed');
  });
  await page.locator('#update-apply').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Update complete',{},{timeout:360000});
  const applied=await snapshot();
  assert.ok(accepted?.operation?.id);assert.equal(applied.operation.id,accepted.operation.id);
  assert.equal(applied.operation.outcome,'applied');assert.equal(applied.operation.transaction_id,accepted.operation.transaction_id);
  assert.equal(applied.current.version,checked.check.version);
  assert.equal(mutations.filter(x=>x.path.endsWith('/apply')).length,1);
  await page.reload();await page.locator('#updates-content').waitFor({state:'visible'});
  const reloaded=await snapshot();assert.equal(reloaded.operation.transaction_id,applied.operation.transaction_id);
  assert.equal(mutations.filter(x=>x.path.endsWith('/apply')).length,1);
  checks.push('Real apply survives a deliberately lost browser reply; reload retains the original transaction without resubmission');
  await screenshot('actual-updates-applied.png');
  await page.locator('#update-rollback').waitFor({state:'visible'});await page.locator('#update-rollback').click();
  assert.equal(mutations.filter(x=>x.path.endsWith('/recover')).length,0);
  await page.locator('#update-confirm-rollback').click();
  await page.waitForFunction(()=>document.querySelector('#update-progress-title').textContent==='Previous version restored',{},{timeout:360000});
  const restored=await snapshot();
  assert.equal(restored.operation.outcome,'rolled_back');assert.notEqual(restored.operation.transaction_id,applied.operation.transaction_id);
  assert.equal(restored.current.version,before.current.version);
  assert.equal(mutations.filter(x=>x.path.endsWith('/recover')).length,1);
  assert.equal(mutations.find(x=>x.path.endsWith('/recover')).body.operation_id,applied.operation.id);
  assert.equal(await page.locator('#update-login').isVisible(),false);
  checks.push('Deliberate browser rollback completes a fresh transaction and retains the existing authenticated account');
  await screenshot('actual-updates-rolled-back.png');
  const exposed=await page.evaluate(secrets=>secrets.some(secret=>document.body.innerText.includes(secret)||location.href.includes(secret)||JSON.stringify(sessionStorage).includes(secret)||JSON.stringify(localStorage).includes(secret)),[fixture.password,...(fixture.cookies||[]).map(cookie=>cookie.value)].filter(Boolean));
  assert.equal(exposed,false);assert.equal(await page.evaluate(()=>globalThis.__updateMediaCalls),0);
  assert.deepEqual(errors,[]);assert.deepEqual(forbidden,[]);
  process.stdout.write(JSON.stringify({status:'passed',checks,before,checked,applied,restored,
    mutation_ids:mutations.map(x=>({path:x.path,request_id:x.body.request_id,operation_id:x.body.operation_id,check_id:x.body.check_id})),
    media_calls:0,provider_calls:0,actions:[],page_errors:errors,
    limitations:['Parent controller must independently verify preserved state and OS process/startup identities','Disposable packaged installation; no physical audio or paid service qualification']})+'\n');
} finally {await context.close();await browser.close();}
