// Actual compiled HTTPS service + cookie/CSRF browser + SQLite. No model,
// media, production account or Minecraft world is used by this journey.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
let input='';for await(const chunk of process.stdin)input+=chunk;
const cfg=JSON.parse(input),checks=[];
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1280,height:900}});
const page=await context.newPage(),errors=[];
page.on('pageerror',error=>errors.push(error.message));
const uid=()=>crypto.randomUUID();
async function api(path,method='GET',body,token=cfg.control){
  const response=await context.request.fetch(cfg.url+path,{method,headers:{Authorization:`Bearer ${token}`},data:body});
  const value=await response.json();assert(response.ok(),`${method} ${path}: ${response.status()} ${JSON.stringify(value)}`);return value;
}
async function until(test,message){for(let i=0;i<100;i++){if(await test())return;await page.waitForTimeout(100);}throw new Error(message);}
try{
  const parent=await api('/api/v1/conversations','POST',{title:'Original human request',agent_ids:[]});
  const source=uid();
  await api(`/api/v1/conversations/${parent.id}/messages`,'POST',{request_id:source,text:'Review the entrance and return the useful result.'});
  const room=await api(`/api/v1/conversations/${parent.id}/huddles`,'POST',{request_id:uid(),title:'Entrance review',purpose:'Compare observations and report the checked result.',source_message_id:source});
  const credentials={};
  for(const agent of ['coach','maker'])credentials[agent]=(await api(`/api/v1/huddles/${room.id}/participants`,'POST',{agent_id:agent,label:agent==='coach'?'Coach':'Maker',connection_id:`minecraft-${agent}`})).token;
  const post=async(agent,text,kind='message',request_id=uid())=>api(`/api/v1/huddles/${room.id}/posts`,'POST',{
    request_id,text,kind,target_agent_ids:[],outcome:kind==='outcome'?{kind:'result',evidence:[{label:'Fixture observations',reference:'fixture:entrance-review'}]}:null
  },credentials[agent]);
  await post('coach','I will check the entrance width.');
  await post('maker','I will inspect the materials and compare the route.');
  assert.equal((await api(`/api/v1/conversations/${parent.id}`)).messages.length,1);
  await context.addCookies([{name:cfg.cookie_name,value:cfg.cookie,url:cfg.url,secure:true,httpOnly:true,sameSite:'Strict'}]);
  await page.goto(`${cfg.url}/?conversation=${parent.id}`);
  await page.locator('#app:not([hidden])').waitFor();
  await page.locator('#conversation-huddles:not([hidden])').waitFor();
  await page.locator('#conversation-huddles > summary').click();
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Join discussion',exact:true}).waitFor();
  assert.equal(await page.locator('.huddles-message').count(),2);
  assert.equal(await page.locator('.huddles-history').getAttribute('aria-live'),'off');
  assert.equal((await api(`/api/v1/huddles/${room.id}/messages`)).items.length,2);
  assert.equal((await api(`/api/v1/huddles/${room.id}`)).preference.mode,'quiet');
  assert.doesNotMatch(await page.locator('#messages').innerText(),/check the entrance width/);
  checks.push('Real enrolled participants coordinate without adding routine posts to the original conversation; inspection is read-only.');

  await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Please keep the entrance clear.');
  await page.getByText('To: Everyone here',{exact:true}).click();
  await page.getByRole('checkbox',{name:'Coach',exact:true}).check();
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await until(async()=> (await api(`/api/v1/huddles/${room.id}/messages`)).items.length===3,'Human join was not saved');
  const joined=(await api(`/api/v1/huddles/${room.id}/messages`)).items.at(-1);
  assert.equal(joined.role,'user');assert.equal(joined.actor,'fixture-owner');
  assert.deepEqual(joined.target_agent_ids,['coach']);assert.equal(joined.agent_id,null);
  await page.getByRole('button',{name:'Follow details',exact:true}).click();
  await page.getByRole('button',{name:'Quiet updates',exact:true}).waitFor();
  await post('maker','The rear approach has more room.');
  await until(async()=> (await page.locator('.huddles-history').innerText()).includes('rear approach'),'Follow did not refresh inspected detail');
  await page.getByRole('button',{name:'Quiet updates',exact:true}).click();
  await page.getByRole('button',{name:'Follow details',exact:true}).waitFor();
  const quiet=await api(`/api/v1/huddles/${room.id}`);
  assert.equal(quiet.preference.mode,'quiet');assert.equal(quiet.state,'open');assert.equal(quiet.participants.length,2);
  checks.push('Authenticated human join retains source attribution and chosen recipient; Follow and Quiet leave work/membership unchanged.');

  // Commit the real POST, then discard its HTTP acknowledgment. Reload must
  // recover the existing durable view and query status without a second POST.
  let submissions=0;
  const endpoint=`${cfg.url}/api/v1/conversations/${room.id}/messages`;
  await page.route(endpoint,async route=>{
    if(route.request().method()!=='POST')return route.continue();
    submissions++;assert.equal(submissions,1);
    const committed=await route.fetch();assert.equal(committed.status(),202);
    await route.abort('connectionfailed');
  });
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Use the rear approach in the review.');
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await page.getByRole('button',{name:'Check message status',exact:true}).waitFor();
  await page.reload();
  await page.locator('#conversation-huddles:not([hidden])').waitFor();
  if(!await page.locator('#conversation-huddles').evaluate(e=>e.open))await page.locator('#conversation-huddles > summary').click();
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Check message status',exact:true}).click();
  await until(async()=>await page.getByRole('textbox',{name:'Message the huddle'}).inputValue()==='','Lost-ack status did not settle');
  await page.unroute(endpoint);
  assert.equal(submissions,1);
  const retained=(await api(`/api/v1/huddles/${room.id}/messages`)).items;
  assert.equal(retained.filter(m=>m.text==='Use the rear approach in the review.').length,1);
  checks.push('Actual committed-but-lost HTTP acknowledgment survives browser reload and resolves read-only with one source POST.');

  const resultId=uid(),resultText='Review complete: the rear approach is clear; the entrance still needs its final measurement.';
  const receipt=await post('coach',resultText,'outcome',resultId);
  const retry=await post('coach',resultText,'outcome',resultId);
  assert.deepEqual(retry,receipt);assert.equal(receipt.return.state,'stored');
  await until(async()=> (await page.locator('#messages').innerText()).includes(resultText),'Result did not return to original conversation');
  const original=await api(`/api/v1/conversations/${parent.id}`);
  assert.equal(original.messages.length,2);assert.equal(original.deliveries.length,0);
  assert.match(original.messages[1].text,/Inspect coordination/);
  assert.equal(original.messages[1].agent_id,'coach');
  assert.equal(original.messages[0].id,source);
  checks.push('One attributed meaningful result returns to the exact original human request; retries do not duplicate it or launch ACP.');

  await page.setViewportSize({width:390,height:844});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await page.screenshot({path:cfg.screenshot.replace('.png','-huddles-mobile.png'),fullPage:true});
  await page.goto(`${cfg.url}/?conversation=${room.id}`);
  await page.getByRole('button',{name:'Stop huddle',exact:true}).click();
  await until(async()=> (await api(`/api/v1/huddles/${room.id}`)).state==='stopped','Existing Stop did not close huddle');
  assert(await page.locator('#send').isDisabled());
  const rejected=await context.request.post(`${cfg.url}/api/v1/huddles/${room.id}/posts`,{headers:{Authorization:`Bearer ${credentials.coach}`},data:{request_id:uid(),text:'Must not post after Stop',kind:'message'}});
  assert.equal(rejected.status(),409);
  assert.equal((await api(`/api/v1/huddles/${room.id}/posts/${resultId}`,'GET',undefined,credentials.coach)).status,'saved');
  checks.push('390px layout is usable; existing Stop closes new coordination while retained history and operation status remain available.');
  const frames=await readFile(`${cfg.home}/frames.jsonl`,'utf8').catch(e=>{if(e.code==='ENOENT')return '';throw e;});
  assert.equal(frames,'','No ACP process should have been invoked');
  assert.deepEqual(errors,[]);
  const report={scope:'Actual compiled TLS API, SQLite, browser cookie/CSRF and UI. No real model, voice or Minecraft world.',passed:checks.length,checks,errors};
  await writeFile(cfg.screenshot.replace('.png','-huddles-result.json'),JSON.stringify(report,null,2)+'\n');
  console.log(JSON.stringify(report,null,2));
}finally{await browser.close();}
