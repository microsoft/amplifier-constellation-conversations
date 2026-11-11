// Callback-bound presentation tests only. No provider, media or model calls.
// Reuse a reviewed Playwright install with PLAYWRIGHT_MODULE when necessary.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {join,isAbsolute} from 'node:path';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const source = await readFile(new URL('../ui/huddles.mjs',import.meta.url));
const css = await readFile(new URL('../ui/huddles.css',import.meta.url));
const screenshotDirectory=process.env.HUDDLES_SCREENSHOT_DIR;
if(screenshotDirectory){assert(isAbsolute(screenshotDirectory));await mkdir(screenshotDirectory,{recursive:true});}
const pageSource = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/huddles.css"><style>body{font:16px system-ui;margin:16px}main{max-width:680px}#call-controls{position:sticky;top:0;background:white;padding:8px;border:1px solid #ddd}button{min-height:44px}</style><title>Huddle fixture</title><div id="call-controls"><span>Call connected</span><button id="hangup">End call</button><audio id="call-audio"></audio></div><main id="huddles"></main><script type="module">
import {createHuddles} from '/huddles.mjs';
const original = '2026-11-10T12:13:14.000Z';
const participants = [{id:'coach',label:'Coach',kind:'agent'},{id:'rook',label:'Rook',kind:'agent'},{id:'brian',label:'Brian',kind:'human'}];
const fixture = window.fixture = {calls:[],pending:[],hold:{},mode:'recorded',lookups:'unknown',context:'caller-a',mediaRequests:0,closed:0,room:{id:'h1',title:'Build review',purpose:'Review the entry and lighting.',status:'Waiting on the review',participants,following:false,canSend:true,messages:[{id:'m1',actor:'Coach',text:'The entry is ready to review.',createdAt:original},{id:'m2',actor:'Rook',text:'I disagree about the lighting. <img src=x onerror="window.injected=true">',createdAt:original}],before:'older-1'}};
fixture.peer = {id:'same-private-call'}; fixture.audio = document.querySelector('#call-audio');
document.querySelector('#hangup').onclick=()=>{fixture.closed++;};
const callback = async(name,args,result)=>{
  fixture.calls.push({name,context:args.context.key,id:args.huddleId,requestId:args.requestId,text:args.text,targets:args.targetAgentIds,following:args.following,before:args.before});
  if(fixture.hold[name]){delete fixture.hold[name];return new Promise((resolve,reject)=>fixture.pending.push({name,resolve,reject,result,signal:args.signal}));}
  return result;
};
fixture.release = (name,result)=>{const item=fixture.pending.find(x=>x.name===name);if(!item)throw new Error('missing pending');fixture.pending.splice(fixture.pending.indexOf(item),1);item.resolve(result??item.result);};
fixture.reject = (name,status)=>{const item=fixture.pending.find(x=>x.name===name);fixture.pending.splice(fixture.pending.indexOf(item),1);item.reject(Object.assign(new Error('private diagnostic that must stay hidden'),{status}));};
const view=window.view=createHuddles({root:document.querySelector('#huddles'),
 list:args=>callback('list',args,{items:fixture.listEmpty?[]:[{...fixture.room,messages:undefined}]}),
 read:args=>callback('read',args,args.before?{...fixture.room,messages:[{id:'old1',actor:'Coach',text:'Original decision.',createdAt:'2026-11-09T01:02:03Z'}],before:null}:structuredClone(fixture.room)),
 setFollowing:async args=>{const result=await callback('follow',args,{following:args.following});fixture.room.following=result.following;return result;},
 send:async args=>{const result=await callback('send',args,{requestId:args.requestId,state:fixture.mode});if(fixture.mode==='throw')throw new Error('private token diagnostic');return result;},
 findSubmission:args=>callback('lookup',args,{requestId:args.requestId,state:fixture.lookups})
});
await view.setContext({key:'caller-a',originalConversationId:'original-a'});window.ready=true;
</script></html>`;
const server=createServer((request,response)=>{
  if(request.url==='/huddles.mjs'){response.writeHead(200,{'Content-Type':'text/javascript'});response.end(source);return;}
  if(request.url==='/huddles.css'){response.writeHead(200,{'Content-Type':'text/css'});response.end(css);return;}
  response.writeHead(200,{'Content-Type':'text/html'});response.end(pageSource);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;const checks=[];
try {
  browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
  const page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  const url=`http://127.0.0.1:${server.address().port}`;
  const reset=async()=>{await page.goto(url);await page.waitForFunction(()=>window.ready);};
  const count=name=>page.evaluate(name=>fixture.calls.filter(call=>call.name===name).length,name);
  await reset();
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Join discussion',exact:true}).waitFor();
  assert.equal(await count('send'),0);assert.equal(await count('follow'),0);
  assert.equal(await page.locator('.huddles-message').count(),2);
  assert.equal(await page.locator('.huddles-message time').first().getAttribute('datetime'),'2026-11-10T12:13:14.000Z');
  assert.equal(await page.locator('.huddles-history').getAttribute('aria-live'),'off');
  assert.equal(await page.locator('.huddles-history img').count(),0);assert.equal(await page.evaluate(()=>window.injected),undefined);
  await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  assert.equal(await count('send'),0);
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Keep the entrance clear.');
  await page.getByText('To: Everyone here',{exact:true}).click();
  await page.getByRole('textbox',{name:'Message the huddle'}).evaluate(el=>el.setSelectionRange(5,10));
  await page.getByRole('textbox',{name:'Message the huddle'}).focus();
  await page.evaluate(()=>view.refresh());
  assert.equal(await page.getByRole('textbox',{name:'Message the huddle'}).inputValue(),'Keep the entrance clear.');
  assert.deepEqual(await page.getByRole('textbox',{name:'Message the huddle'}).evaluate(el=>[document.activeElement===el,el.selectionStart,el.selectionEnd]),[true,5,10]);
  assert.equal(await page.locator('.huddles-targets').evaluate(el=>el.open),true);
  if(screenshotDirectory)await page.screenshot({path:join(screenshotDirectory,'huddles-desktop.png'),fullPage:true});
  checks.push('Inspection/join do not send; source actor/time and dissent remain; HTML is inert; refresh preserves draft and caret.');

  await page.getByRole('checkbox',{name:'Coach',exact:true}).check();
  await page.getByRole('checkbox',{name:'Rook',exact:true}).check();
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await page.waitForFunction(()=>fixture.calls.some(call=>call.name==='send'));
  const first=await page.evaluate(()=>fixture.calls.find(call=>call.name==='send'));
  assert.deepEqual(first.targets,['coach','rook']);assert.match(first.requestId,/^[0-9a-f-]{36}$/);
  assert.equal(first.context,'caller-a');assert.equal(await count('send'),1);
  await page.getByText('Your message was saved. Agent responses may follow.',{exact:true}).waitFor();
  assert.equal(await page.getByRole('textbox',{name:'Message the huddle'}).inputValue(),'');
  assert.equal(await page.evaluate(()=>fixture.closed),0);
  checks.push('One explicit send carries the original context, UUID and both selected agents; saved message does not claim executed work.');

  await page.getByRole('button',{name:'Follow details',exact:true}).click();
  await page.getByRole('button',{name:'Quiet updates',exact:true}).waitFor();
  assert.equal(await count('follow'),1);assert.equal(await count('send'),1);
  await page.getByText('Following details in this view. Voice updates are considered separately.',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Quiet updates',exact:true}).click();
  await page.getByRole('button',{name:'Follow details',exact:true}).waitFor();
  await page.getByRole('button',{name:'Back to huddles',exact:true}).click();
  assert.deepEqual(await page.evaluate(()=>[fixture.closed,fixture.peer.id,fixture.audio===document.querySelector('#call-audio')]),[0,'same-private-call',true]);
  assert.deepEqual(await page.evaluate(()=>[...new Set(fixture.calls.map(call=>call.name))].sort()),['follow','list','read','send']);
  checks.push('Quiet/follow invokes only persisted preference; leaving preserves surrounding call controls and has no Stop/access operation.');

  await page.getByRole('button',{name:'Inspect',exact:true}).click();await page.getByRole('button',{name:'Earlier messages',exact:true}).click();
  await page.getByText('Original decision.',{exact:true}).waitFor();
  assert.equal(await page.locator('.huddles-message').count(),1);
  await page.getByRole('button',{name:'Back to latest',exact:true}).click();
  await page.getByText('The entry is ready to review.',{exact:true}).waitFor();
  assert.equal(await page.locator('.huddles-message').count(),2);
  checks.push('History uses explicit bounded pages and a return-to-latest control, without merging across hidden gaps.');

  if(await page.getByRole('button',{name:'Join discussion',exact:true}).count())await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  await page.evaluate(()=>{fixture.mode='throw';});
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Unknown receipt example.');
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await page.getByRole('button',{name:'Check message status',exact:true}).waitFor();
  const uncertain=await page.evaluate(()=>fixture.calls.filter(call=>call.name==='send').at(-1));
  assert.equal(await page.getByRole('button',{name:'Send message',exact:true}).isDisabled(),true);
  await page.evaluate(()=>{fixture.hold.read=true;});
  await page.getByRole('button',{name:'Refresh discussion',exact:true}).click();
  await page.waitForFunction(()=>fixture.pending.some(item=>item.name==='read'));
  await page.evaluate(()=>fixture.reject('read',503));
  await page.getByText('The huddle could not be refreshed. Existing message status is unchanged.',{exact:true}).waitFor();
  assert.equal((await page.locator('#huddles').innerText()).includes('No message was sent.'),false);
  await page.getByRole('button',{name:'Back to huddles',exact:true}).click();
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Check message status',exact:true}).click();
  await page.waitForFunction(()=>fixture.calls.some(call=>call.name==='lookup'));
  assert.equal(await page.evaluate(()=>fixture.calls.filter(call=>call.name==='lookup').at(-1).requestId),uncertain.requestId);
  assert.equal(await count('send'),2);
  assert.equal(await page.getByRole('button',{name:'Send message',exact:true}).isDisabled(),true);
  assert.equal((await page.locator('body').innerText()).includes('private token'),false);
  await page.evaluate(uncertain=>{fixture.lookups='recorded';fixture.hold.read=true;fixture.room.submission={requestId:uncertain.requestId,text:uncertain.text,targetAgentIds:uncertain.targets,state:'unknown'};},uncertain);
  await page.getByRole('button',{name:'Refresh discussion',exact:true}).click();
  await page.waitForFunction(()=>fixture.pending.some(item=>item.name==='read'));
  await page.getByRole('button',{name:'Check message status',exact:true}).click();
  await page.waitForFunction(()=>fixture.calls.filter(call=>call.name==='lookup').length===2);
  await page.getByText('Your message was saved. Agent responses may follow.',{exact:true}).waitFor();
  await page.evaluate(()=>fixture.release('read'));
  assert.equal(await page.getByRole('button',{name:'Check message status',exact:true}).count(),0);
  assert.equal(await count('send'),2);
  checks.push('Lost acknowledgment freezes exact UUID/content across close/reopen; checking is read-only, missing evidence stays unknown, and reconciliation never resends.');

  await reset();
  await page.evaluate(()=>{fixture.hold.read=true;});
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.waitForFunction(()=>fixture.pending.length===1);
  await page.evaluate(async()=>{await view.setContext({key:'caller-b',originalConversationId:'original-b'});fixture.release('read');});
  assert.equal(await page.locator('.huddles-inspector').isHidden(),true);assert.equal(await page.locator('.huddles-message').count(),0);
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Private draft must disappear');
  await page.evaluate(()=>{fixture.hold.send=true;});
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await page.waitForFunction(()=>fixture.pending.some(item=>item.name==='send'));
  await page.evaluate(()=>{view.revoke();fixture.release('send');});
  assert.equal(await page.locator('.huddles-message').count(),0);assert.equal(await page.locator('textarea').count(),0);
  assert.equal((await page.locator('#huddles').innerText()).includes('Private draft'),false);
  await page.getByText('This huddle view is no longer available.',{exact:true}).waitFor();
  checks.push('Late history/send callbacks cannot repopulate another identity or revoked view; drafts and private content are cleared.');

  await reset();
  await page.evaluate(()=>{fixture.room.submission={requestId:'saved-request',text:'Saved unresolved input',targetAgentIds:['coach'],state:'unknown'};});
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Check message status',exact:true}).click();
  assert.equal(await page.evaluate(()=>fixture.calls.find(call=>call.name==='lookup').requestId),'saved-request');assert.equal(await count('send'),0);
  await page.evaluate(()=>{fixture.hold.read=true;});
  await page.getByRole('button',{name:'Refresh discussion',exact:true}).click();
  await page.waitForFunction(()=>fixture.pending.some(item=>item.name==='read'));
  await page.evaluate(()=>fixture.reject('read',403));
  await page.getByText('That huddle is no longer available.',{exact:true}).waitFor();
  assert.equal(await page.locator('.huddles-message').count(),0);assert.equal(await page.locator('textarea').count(),0);
  checks.push('Host-provided unresolved receipt rehydrates without send; access revocation clears the inspected room.');

  await reset();
  await page.evaluate(()=>{fixture.room.messages=Array.from({length:60},(_,i)=>({id:'long-'+i,actor:'Coach',text:('Message '+i+' original discussion. ').repeat(12),createdAt:'2026-11-10T12:13:14Z'}));});
  await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.huddles-message').length===60);
  await page.locator('.huddles-history').evaluate(el=>{el.scrollTop=100;});
  await page.evaluate(async()=>{fixture.room.messages.push({id:'new-end',actor:'Rook',text:'New distinct contribution.',createdAt:'2026-11-10T13:14:15Z'});await view.refresh();});
  assert.equal(await page.locator('.huddles-history').evaluate(el=>el.scrollTop),100);
  await page.locator('.huddles-history').evaluate(el=>{el.scrollTop=el.scrollHeight;});
  await page.evaluate(async()=>{fixture.room.messages.push({id:'newer-end',actor:'Coach',text:'Another distinct contribution.',createdAt:'2026-11-10T13:14:16Z'});await view.refresh();});
  assert.equal(await page.locator('.huddles-history').evaluate(el=>el.scrollHeight-el.scrollTop-el.clientHeight<2),true);
  checks.push('Peer refresh preserves a reader’s older scroll position; only a reader already at the end follows new messages.');

  await reset();await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Join discussion',exact:true}).waitFor();
  const quietReads=await count('read');
  await page.evaluate(async()=>{fixture.room.messages.push({id:'quiet-new',actor:'Rook',text:'New routine detail.',createdAt:'2026-11-10T13:14:15Z'});await view.refresh({automatic:true});});
  assert.equal(await count('read'),quietReads);assert.equal(await page.locator('.huddles-message').count(),2);
  await page.getByRole('button',{name:'Refresh discussion',exact:true}).click();await page.getByText('New routine detail.',{exact:true}).waitFor();
  assert.equal(await count('read'),quietReads+1);
  await page.getByRole('button',{name:'Follow details',exact:true}).click();await page.getByRole('button',{name:'Quiet updates',exact:true}).waitFor();
  await page.evaluate(async()=>{fixture.room.messages.push({id:'follow-new',actor:'Coach',text:'Following reveals this detail.',createdAt:'2026-11-10T13:14:16Z'});await view.refresh({automatic:true});});
  await page.getByText('Following reveals this detail.',{exact:true}).waitFor();assert.equal(await count('read'),quietReads+2);
  await page.getByRole('button',{name:'Quiet updates',exact:true}).click();await page.getByRole('button',{name:'Follow details',exact:true}).waitFor();
  await page.evaluate(async()=>{fixture.listEmpty=true;await view.refresh({automatic:true});});
  assert.equal(await page.locator('.huddles-inspector').isHidden(),true);assert.equal(await page.locator('.huddles-message').count(),0);
  assert.equal(await count('send'),0);assert.equal(await page.evaluate(()=>fixture.closed),0);
  checks.push('Automatic discovery keeps quiet history still; explicit Refresh and followed details fetch messages, while revoked discovery clears even a quiet inspected view.');

  await reset();await page.getByRole('button',{name:'Inspect',exact:true}).click();
  await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('Local draft before a remote Stop.');
  const metadataReads=await count('read');
  await page.evaluate(async()=>{fixture.room.status='Discussion stopped';fixture.room.canSend=false;fixture.room.participants=fixture.room.participants.filter(person=>person.id!=='rook');await view.refresh({automatic:true});});
  assert.equal(await count('read'),metadataReads);
  assert.equal(await page.getByRole('button',{name:'Join discussion',exact:true}).count(),0);
  assert.equal(await page.getByRole('button',{name:'Send message',exact:true}).count(),0,'remote Stop summary must disable stale quiet composer without an extra history read');
  assert.equal(await page.getByRole('textbox',{name:'Message the huddle'}).count(),0);
  assert.equal(await count('send'),0);
  checks.push('Quiet automatic metadata applies remote Stop and participant changes without rereading history or leaving a stale composer.');

  await reset();
  await page.evaluate(()=>{fixture.room.canSend=false;fixture.room.status='Discussion stopped';fixture.room.submission={requestId:'stopped-unknown',text:'An earlier unresolved message',targetAgentIds:['coach'],state:'unknown'};});
  await page.getByRole('button',{name:'Inspect',exact:true}).click();await page.getByRole('button',{name:'Check message status',exact:true}).waitFor();
  assert.equal(await page.getByRole('button',{name:'Join discussion',exact:true}).count(),0);
  assert.equal(await page.getByRole('button',{name:'Send message',exact:true}).isDisabled(),true);assert.equal(await page.getByRole('textbox',{name:'Message the huddle'}).isDisabled(),true);
  await page.getByRole('button',{name:'Check message status',exact:true}).click();assert.equal(await count('lookup'),1);assert.equal(await count('send'),0);
  await page.evaluate(()=>{fixture.lookups='recorded';});await page.getByRole('button',{name:'Check message status',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('.huddles-composer'));
  assert.equal(await count('send'),0);assert.equal(await count('lookup'),2);
  checks.push('Stopping a discussion denies new input but retains read-only reconciliation of an earlier unknown send without replay.');

  await reset();await page.setViewportSize({width:390,height:844});
  await page.getByRole('button',{name:'Inspect',exact:true}).click();await page.getByRole('button',{name:'Join discussion',exact:true}).click();
  await page.getByRole('textbox',{name:'Message the huddle'}).fill('A phone-sized contribution.');
  if(screenshotDirectory)await page.screenshot({path:join(screenshotDirectory,'huddles-phone-390.png'),fullPage:true});
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await page.getByText('Your message was saved. Agent responses may follow.',{exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true);
  assert.equal(await page.getByRole('button',{name:'End call',exact:true}).isVisible(),true);
  assert.equal(await page.evaluate(()=>fixture.closed),0);
  await page.evaluate(()=>view.destroy());assert.equal(await page.locator('#huddles').innerText(),'');
  assert.deepEqual(errors,[]);
  checks.push('390px inspection, targeting, send and cleanup fit the viewport while leaving call controls intact; no browser exceptions.');
  console.log(JSON.stringify({passed:checks.length,checks,models:0,providerCalls:0,playwrightModule:process.env.PLAYWRIGHT_MODULE||'playwright',screenshots:screenshotDirectory?[join(screenshotDirectory,'huddles-desktop.png'),join(screenshotDirectory,'huddles-phone-390.png')]:[]},null,2));
} finally {await browser?.close();await new Promise(resolve=>server.close(resolve));}

// The widget must also fit the actual fixed-height Companion call layout.
await import('./companion-huddle-layout-browser.mjs');
