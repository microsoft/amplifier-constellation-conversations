import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFile,writeFile} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
let input='';for await(const chunk of process.stdin)input+=chunk;const cfg=JSON.parse(input);
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1280,height:900}});
const page=await context.newPage(),errors=[],checks=[];page.on('pageerror',e=>errors.push(e.message));
async function wait(test,message){for(let i=0;i<150;i++){if(await test())return;await page.waitForTimeout(50);}throw new Error(message);}
async function api(path,method='GET',body){const r=await context.request.fetch(cfg.url+path,{method,headers:{Authorization:`Bearer ${cfg.control}`},data:body});const value=await r.json();assert(r.ok(),`${path}: ${r.status()} ${JSON.stringify(value)}`);return value;}
async function frames(name='frames'){try{return(await readFile(`${cfg.home}/${name}.jsonl`,'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);}catch{return[];}}
async function send(text){await page.locator('#message').fill(text);await wait(async()=>await page.locator('#send').isEnabled(),'composer did not become available');await page.locator('#send').click();}
async function room(){return api(`/api/v1/conversations/${new URL(page.url()).searchParams.get('conversation')}`);}
try{
 await context.addCookies([{name:cfg.cookie_name,value:cfg.cookie,url:cfg.url,secure:true,httpOnly:true,sameSite:'Strict'}]);
 await page.goto(cfg.url);await page.locator('#app:not([hidden])').waitFor();
 await page.locator('#new-conversation').click();await wait(async()=>new URL(page.url()).searchParams.has('conversation'),'room not selected');
 await send('Original browser task');await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();
 const approval=(await room()).permissions[0].id;
 await send('Keep this correction');await wait(async()=>(await frames()).some(v=>v.method==='_session/steering'),'follow-up was not steered');
 assert.equal((await room()).permissions[0].id,approval);assert(await page.locator('#stop').isVisible());checks.push('busy composer and unchanged approval');
 // Lose only the response after the server durably accepted the message.
 let lost=false;
 await page.route('**/api/v1/conversations/*/messages',async route=>{if(!lost&&route.request().method()==='POST'){lost=true;await route.fetch();await route.abort('failed');}else await route.continue();});
 await send('Survive a lost browser acknowledgment');await wait(async()=>await page.locator('#global-error').isVisible(),'lost acknowledgment not surfaced');
 await wait(async()=>(await frames()).filter(v=>v.method==='_session/steering').length===2,'accepted correction not steered');
 await page.unroute('**/api/v1/conversations/*/messages');await page.reload();await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();
 assert.equal(await page.locator('#message').inputValue(),'Survive a lost browser acknowledgment');
 assert.equal((await room()).permissions[0].id,approval);
 await page.locator('#send').click();await wait(async()=>await page.locator('#message').inputValue()==='','duplicate acknowledgment did not clear draft');
 assert.equal((await room()).messages.filter(m=>m.text==='Survive a lost browser acknowledgment').length,1);
 assert.equal((await frames()).filter(v=>v.method==='_session/steering').length,2);checks.push('reload retries the same source ID without duplicate intake');
 await page.getByRole('button',{name:'Allow once',exact:true}).click();
 await wait(async()=>(await frames()).some(v=>v.effect==='write-once'),'approved effect missing');
 await send('After the effect, keep going');await wait(async()=>(await frames()).filter(v=>v.method==='_session/steering').length===3,'post-effect update missing');
 await writeFile(`${cfg.home}/release`,'1');await wait(async()=>(await room()).status==='idle','work did not finish');
 await wait(async()=>(await page.locator('#messages').innerText()).includes('Included in model context'),'incorporation receipt missing');
 assert.equal((await frames()).filter(v=>v.method==='session/prompt').length,1);assert.equal((await frames()).filter(v=>v.effect==='write-once').length,1);assert(!(await frames()).some(v=>v.method==='session/cancel'));checks.push('one prompt and effect across repeated corrections');
 // A different endpoint demonstrates uncertain delivery and explicit no-replay recovery.
 const agent=structuredClone((await api('/api/v1/agents')).items[0].config);agent.label='Recovery fixture';delete agent.env.STEERING_APPROVAL;agent.env.STEERING_MODE='disconnect';agent.env.FIXTURE_LOG=`${cfg.home}/recovery.jsonl`;agent.env.STEERING_RELEASE=`${cfg.home}/recovery-release`;
 await api('/api/v1/agents','POST',{id:'recovery',config:agent});const r=await api('/api/v1/conversations','POST',{title:'Recovery',agent_ids:['recovery']});
 await page.goto(`${cfg.url}/?conversation=${r.id}`);await page.locator('#app:not([hidden])').waitFor();await wait(async()=>await page.locator('#conversation-title').innerText()==='Recovery','recovery room missing');
 await send('Original uncertain work');await wait(async()=>(await frames('recovery')).some(v=>v.method==='session/prompt'),'recovery prompt missing');
 await send('Do not replay this correction');await wait(async()=>(await room()).status==='error','unknown not visible');
 // Seed completed history only in this disposable database to exercise paging.
 execFileSync('python3',['-c',`import sqlite3,sys
with sqlite3.connect(sys.argv[1]+'/state.sqlite3') as db:
 for i in range(300):
  key='older-fixture-'+str(i)
  db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?,?,'user','Completed history','complete',1,1,'fixture-owner')",(key,sys.argv[2]))
  db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?,?,?,'recovery','direct','completed',1)",(key,key,sys.argv[2]))`,cfg.home,r.id]);
 await page.reload();await page.getByRole('button',{name:/Review earlier message/}).first().click();await page.getByRole('button',{name:'Continue without resending',exact:true}).first().waitFor();checks.push('older unresolved delivery remains reachable after newer history');
 assert.match(await page.locator('#messages').innerText(),/Outcome unknown/);
 await send('An unrelated next question');await wait(async()=>(await room()).status==='error','blocked new question did not settle');
 assert.equal((await frames('recovery')).filter(v=>v.method==='session/prompt').length,1);
 await writeFile(`${cfg.home}/recovery-release`,'1');
 const pending=(await room()).deliveries.filter(d=>d.state==='unknown');assert.equal(pending.length,2);
 // Every available button has the same authenticated programmatic counterpart.
 await page.evaluate(id=>window.agentConversationsControl.perform({action:'delivery_resolve',id}),pending[0].id);
 await wait(async()=>(await room()).status!=='working','first review not settled');
 await page.evaluate(id=>window.agentConversationsControl.perform({action:'delivery_show',id}),pending[1].id);
 await page.locator(`[data-delivery-id="${pending[1].id}"]`).getByRole('button',{name:'Continue without resending',exact:true}).click();
 await wait(async()=>(await frames('recovery')).filter(v=>v.method==='session/prompt').length===2,'reviewed next prompt missing');
 await wait(async()=>(await room()).status==='idle','reviewed conversation did not continue');
 assert.equal((await frames('recovery')).filter(v=>v.method==='_session/steering').length,1);
 assert.equal((await frames('recovery')).filter(v=>v.method==='session/prompt').length,2);checks.push('unknown survives reload; explicit view control and button never replay');
 await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.screenshot({path:cfg.screenshot.replace('.png','-steering-mobile.png'),fullPage:true});checks.push('receipt and recovery controls fit mobile layout');
 assert.deepEqual(errors,[]);console.log(JSON.stringify({passed:checks.length,checks},null,2));
}finally{await browser.close();}
