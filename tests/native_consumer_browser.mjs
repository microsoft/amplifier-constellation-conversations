// Actual packaged local HTTP Companion; controller supplies only disposable
// credentials. No provider, microphone, camera or model operation is permitted.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
let input='';for await (const chunk of process.stdin) input+=chunk;
const fixture=JSON.parse(input);input='';
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
const checks=[],errors=[],forbidden=[];
const context=await browser.newContext({viewport:{width:1280,height:900}});
await context.addInitScript(()=>{
  globalThis.__consumerMediaCalls=0;
  if(navigator.mediaDevices) navigator.mediaDevices.getUserMedia=async()=>{globalThis.__consumerMediaCalls++;throw new Error('Unexpected media request during setup');};
});
const page=await context.newPage();
page.on('pageerror',e=>errors.push(e.message));
page.on('request',r=>{
  if(new URL(r.url()).origin!==new URL(fixture.url).origin || /\/companion\/calls(?:$|\?)/.test(new URL(r.url()).pathname)) forbidden.push(r.method()+' '+new URL(r.url()).pathname);
});
async function screenshot(name){if(fixture.artifact_dir){await mkdir(fixture.artifact_dir,{recursive:true});await page.screenshot({path:resolve(fixture.artifact_dir,name),fullPage:true});}}
try {
  await page.goto(fixture.url+'/companion#setup='+fixture.setup_token);
  await page.locator('#first-account').waitFor({state:'visible'});
  assert.equal(new URL(page.url()).hash,'');
  assert.equal(await page.evaluate(()=>document.body.scrollWidth<=innerWidth),true);
  await screenshot('consumer-first-account-desktop.png');
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(()=>document.body.scrollWidth<=innerWidth),true);
  await screenshot('consumer-first-account-phone.png');
  await page.locator('#new-password').fill(fixture.password);
  await page.locator('#confirm-password').fill(fixture.password);
  assert.equal(await page.locator('#start-at-signin').isChecked(),false);
  await page.locator('#create-account').click();
  await page.locator('#app').waitFor({state:'visible'});
  await page.locator('#setup-agent').selectOption(fixture.agent_id);
  await page.locator('#setup-voice').selectOption('marin');
  await page.locator('#setup-openai-key').fill(fixture.fixture_key);
  await page.locator('#save-contact').click();
  await page.locator('#contacts .contact-card').first().waitFor({state:'visible'});
  assert.equal(await page.locator('#setup-openai-key').inputValue(),'');
  checks.push('Real first-account creation and contact/key setup; capability removed from URL');
  assert.equal(await page.locator('#contacts .contact-card').count(),1);
  assert.ok((await page.locator('#contacts').innerText()).includes(fixture.agent_label));
  assert.equal(await page.evaluate(()=>globalThis.__consumerMediaCalls),0);
  await screenshot('consumer-contact-phone.png');
  await page.setViewportSize({width:1280,height:900});
  await screenshot('consumer-contact-desktop.png');
  const privateLeaks=await page.evaluate(({key,token,password})=>{
    const text=document.body.innerText+'\n'+JSON.stringify(localStorage)+'\n'+JSON.stringify(sessionStorage)+'\n'+location.href;
    return [key,token,password].some(secret=>text.includes(secret));
  },{key:fixture.fixture_key,token:fixture.setup_token,password:fixture.password});
  assert.equal(privateLeaks,false);
  checks.push('No setup capability, password or provider key exposed in page text, URL or browser storage');
  await page.locator('#contact-setup').evaluate(el=>el.open=true);
  await page.locator('#setup-agent').selectOption(fixture.agent_id);
  await page.locator('#save-contact').click();
  await page.waitForFunction(()=>document.querySelector('#contact-setup-status').textContent.startsWith('Contact saved.'));
  assert.equal(await page.locator('#contacts .contact-card').count(),1);
  checks.push('Reselecting saved contact leaves one contact; server independently checks original endpoint/bindings');
  await page.locator('#sign-out').click();
  await page.locator('#login').waitFor({state:'visible'});
  assert.equal(await page.locator('#username').isVisible(),false);
  await page.locator('#password').fill(fixture.password);
  await page.locator('#sign-in').click();
  await page.locator('#contacts .contact-card').first().waitFor({state:'visible'});
  assert.equal(await page.locator('#contacts .contact-card').count(),1);
  assert.equal(await page.locator('#first-account').isVisible(),false);
  checks.push('Local password sign-out/sign-in restores same contact without first-account reset');
  assert.deepEqual(errors,[]);assert.deepEqual(forbidden,[]);
  assert.equal(await page.evaluate(()=>globalThis.__consumerMediaCalls),0);
  process.stdout.write(JSON.stringify({status:'passed',checks,media_calls:0,provider_calls:0,page_errors:errors,limitations:['Controller browser on Linux; native six-target package/startup receipts remain separate','No microphone/camera or paid provider readiness claim']})+'\n');
} finally {await context.close();await browser.close();}
