// A real browser exercises the public caller UI against a disposable API and
// explicitly fake media. No microphone, OpenAI credentials or paid calls.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const files = new Map(await Promise.all(['companion.html','companion.js','companion.css'].map(async name => [name,await readFile(new URL('../ui/' + name,import.meta.url))])));
const requests = [], calls = new Map(), messages = new Map(), events = [], checks = [];
let loggedIn = false, pollFails = false, controlFails = false, catalogAvailable = true, nextCall = 1;
let authOwner = 'fixture-user', holdNextClose = false, heldClose = null;
const csrf = 'fixture-csrf';
const server = createServer(async (request,response) => {
  const url = new URL(request.url,'http://fixture');
  const send = (status,value) => { response.writeHead(status,{'Content-Type':'application/json','Cache-Control':'no-store'}); response.end(JSON.stringify(value)); };
  if (['/companion','/companion.html','/companion.js','/companion.css'].includes(url.pathname)) {
    const name = url.pathname === '/companion' ? 'companion.html' : url.pathname.slice(1);
    response.writeHead(200,{'Content-Type':name.endsWith('.html')?'text/html':name.endsWith('.css')?'text/css':'text/javascript'}); response.end(files.get(name)); return;
  }
  if (url.pathname === '/icon.svg') { response.writeHead(204); response.end(); return; }
  let input=''; for await (const chunk of request) input += chunk;
  let body; try { body = input ? JSON.parse(input) : null; } catch { send(400,{error:'bad JSON'}); return; }
  requests.push({method:request.method,path:url.pathname,body,csrf:request.headers['x-csrf-token']});
  if (url.pathname === '/auth/session') { send(200,{authenticated:loggedIn,owner:authOwner,csrf:loggedIn?csrf:null}); return; }
  if (url.pathname === '/auth/login') { loggedIn=true; authOwner=body.username; send(200,{authenticated:true,owner:authOwner,csrf}); return; }
  if (request.method !== 'GET' && request.headers['x-csrf-token'] !== csrf) { send(403,{error:'missing csrf'}); return; }
  if (url.pathname === '/auth/logout') { loggedIn=false; send(200,{}); return; }
  if (url.pathname === '/api/v1/companion') {
    const active=[...calls.values()].find(call=>call.owner===authOwner&&['starting','connecting','connected','closing'].includes(call.state));
    send(200,{available:catalogAvailable,active_call:active?{id:active.id,conversation_id:active.conversation_id,state:active.state}:null,contacts:[{id:'coach',label:'Coach',voice:'marin',description:'A thoughtful native teammate.'},{id:'rook',label:'Rook',voice:'cedar',description:'<img src=x onerror="window.injection=true"> Codex teammate.'},{id:'jarvis',label:'Jarvis',voice:'ash',description:'Local reasoning, cloud voice.',image_input:false}]}); return;
  }
  if (url.pathname === '/api/v1/companion/calls' && request.method === 'POST') {
    let call = [...calls.values()].find(value => value.request.request_id === body.request_id);
    if (call && JSON.stringify(call.request) !== JSON.stringify(body)) { send(409,{error:'changed request'}); return; }
    if (!call) { call = {id:'call-' + nextCall++,owner:authOwner,request:body,conversation_id:'room-' + body.agent_id,session_id:'fixture-live',sdp:'fixture-answer',state:'connecting'}; calls.set(call.id,call); }
    send(200,call); return;
  }
  const callPath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)(?:\/(heartbeat|close))?$/);
  if (callPath) {
    const call = calls.get(callPath[1]); if (!call) { send(404,{error:'missing call'}); return; }
    if (controlFails) { send(503,{error:'fixture control outage'}); return; }
    if (callPath[2] === 'close') {
      if (call.state !== 'closed') { call.state='closing'; call.closeChecks=2; }
      if (holdNextClose) { holdNextClose=false; heldClose=()=>send(503,{error:'fixture delayed close failure'}); return; }
      send(200,{state:call.state}); return;
    }
    if (callPath[2] === 'heartbeat') { send(200,{}); return; }
    if (pollFails) { send(503,{error:'fixture network outage'}); return; }
    if (call.state === 'closing' && --call.closeChecks <= 0) call.state=call.request.agent_id==='jarvis'?'interrupted':'closed';
    const after = Number(url.searchParams.get('after') || 0);
    send(200,{...call,state:['closing','closed','interrupted'].includes(call.state)?call.state:'connected',events:events.filter(event => event.seq > after),next_cursor:events.at(-1)?.seq || 0}); return;
  }
  if (/\/attachments$/.test(url.pathname)) { send(200,{id:'fixture-photo'}); return; }
  if (/\/messages$/.test(url.pathname)) {
    const prior=messages.get(body.request_id);
    if(prior&&JSON.stringify(prior)!==JSON.stringify(body)){send(409,{error:'changed message'});return;}
    messages.set(body.request_id,body);send(200,{id:'fixture-message'});return;
  }
  send(404,{error:'fixture route not found'});
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
const url = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE || undefined,args:['--no-sandbox']});
const context = await browser.newContext({viewport:{width:1280,height:900}});
const page = await context.newPage(), errors=[]; page.on('pageerror', error => errors.push(error.message));
await page.addInitScript(() => {
  const fixture = window.fixture = {media:[],tracks:[],peers:[],channels:[],level:0,wakeLocks:[],audioContexts:[],delayMic:false,denyCamera:false};
  const actualNow=Date.now;fixture.clockOffset=0;Date.now=()=>actualNow()+fixture.clockOffset;
  const makeStream = kind => {
    const track = new EventTarget(); track.kind=kind; track.enabled=true; track.stopped=false; track.stop=()=>{track.stopped=true;}; fixture.tracks.push(track);
    const stream = new MediaStream(); Object.defineProperties(stream,{getTracks:{value:()=>[track]},getAudioTracks:{value:()=>kind==='audio'?[track]:[]}}); return stream;
  };
  Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:async constraints=>{
    fixture.media.push(constraints);
    if (constraints.video && fixture.denyCamera) throw new DOMException('denied','NotAllowedError');
    if (constraints.audio && fixture.delayMic) return new Promise(resolve=>{fixture.resolveMic=()=>resolve(makeStream('audio'));});
    return makeStream(constraints.video?'video':'audio');
  }}});
  Object.defineProperty(navigator,'wakeLock',{configurable:true,value:{request:async()=>{
    const lock=new EventTarget(); lock.released=false; lock.release=async()=>{lock.released=true;lock.dispatchEvent(new Event('release'));}; fixture.wakeLocks.push(lock); return lock;
  }}});
  class FakePeer extends EventTarget {
    constructor(){super();this.connectionState='new';this.iceGatheringState='complete';this.order=[];this.closed=false;fixture.peers.push(this);}
    addTrack(){this.order.push('track');}
    createDataChannel(label){this.order.push('channel'); const channel=new EventTarget();channel.label=label;channel.sent=[];channel.send=value=>channel.sent.push(value);channel.close=()=>{channel.closed=true;};fixture.channels.push(channel);return channel;}
    async createOffer(){this.order.push('offer');return {type:'offer',sdp:'fixture-offer-' + fixture.peers.length};}
    async setLocalDescription(offer){this.localDescription=offer;}
    async setRemoteDescription(answer){this.remoteDescription=answer;this.connectionState='connected';this.dispatchEvent(new Event('connectionstatechange'));const event=new Event('track');event.streams=[new MediaStream()];this.dispatchEvent(event);}
    close(){this.closed=true;this.connectionState='closed';}
  }
  window.RTCPeerConnection=FakePeer;
  window.AudioContext=class {
    constructor(){this.closed=false;fixture.audioContexts.push(this);}
    async resume(){} async close(){this.closed=true;}
    createMediaStreamSource(){return {connect(){},disconnect(){}};}
    createAnalyser(){return {fftSize:256,getByteTimeDomainData(values){values.fill(128+fixture.level);}};}
  };
  const playing=new WeakSet();
  Object.defineProperty(HTMLMediaElement.prototype,'paused',{get(){return !playing.has(this);}});
  HTMLMediaElement.prototype.play=async function(){playing.add(this);this.dispatchEvent(new Event('playing'));};
  HTMLMediaElement.prototype.pause=function(){playing.delete(this);this.dispatchEvent(new Event('pause'));};
  Object.defineProperty(HTMLVideoElement.prototype,'videoWidth',{get(){return 640;}});
  Object.defineProperty(HTMLVideoElement.prototype,'videoHeight',{get(){return 480;}});
  CanvasRenderingContext2D.prototype.drawImage=function(){};
});
const wait = async (condition,message) => {
  for (let i=0;i<150;i++) { if (await condition()) return; await page.waitForTimeout(50); }
  throw new Error(message);
};
const emit = (kind,data) => events.push({seq:(events.at(-1)?.seq || 0)+1,kind,data});
try {
  await page.goto(url + '/companion'); await page.locator('#login:not([hidden])').waitFor();
  assert.equal(await page.evaluate(()=>fixture.media.length),0);
  await page.locator('#username').fill('fixture-user'); await page.locator('#password').fill('fixture-only'); await page.locator('#sign-in').click();
  await page.locator('#app:not([hidden])').waitFor(); assert.equal(await page.locator('.contact-card').count(),3);
  assert.equal(await page.locator('.contact-card img').count(),0); assert.equal(await page.evaluate(()=>window.injection),undefined);
  assert.match(await page.locator('.call-note').innerText(),/local reasoning uses cloud audio/);
  assert.equal(await page.evaluate(()=>fixture.media.length),0); checks.push('login and contact rendering never acquire media; untrusted text stays text');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-contacts.png'),fullPage:true});

  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','call did not connect');
  assert.deepEqual(await page.evaluate(()=>fixture.peers[0].order),['track','channel','offer']);
  assert.equal(await page.evaluate(()=>fixture.channels[0].label),'oai-events');
  assert.equal(await page.evaluate(()=>fixture.channels[0].sent.length),0);
  const firstRequest=requests.find(request=>request.path==='/api/v1/companion/calls');assert.equal(firstRequest.csrf,csrf);assert.equal(firstRequest.body.agent_id,'coach');
  assert.match(firstRequest.body.request_id,/^[\da-f-]{36}$/); checks.push('explicit Call creates one CSRF-protected SDP session; data channel is receive-only');

  emit('transcript',{role:'assistant',text:'Generated speech is not playback.',fragment:true,playback:'unconfirmed',start_ms:0,end_ms:500});
  emit('provider',{type:'session.usage.updated',usage:{seconds:1}});
  emit('transcript',{role:'assistant',text:' A little more context.',fragment:true,playback:'unconfirmed',start_ms:500,end_ms:1000});
  emit('delegation',{id:'d1',state:'dispatched'});
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Working together','delegated work not visible');
  assert.equal(await page.locator('.transcript-entry').count(),1);assert.match(await page.locator('.transcript-entry').innerText(),/playback\. A little/);
  assert.equal(await page.locator('.presence.speaking').count(),0);
  await page.evaluate(()=>fixture.channels[0].dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'session.output_transcript.delta',delta:'DUPLICATE'})})));
  assert.doesNotMatch(await page.locator('#transcript').innerText(),/DUPLICATE/);
  await page.evaluate(()=>{fixture.level=20;}); await page.locator('.presence.speaking').waitFor();
  await page.evaluate(()=>{fixture.level=0;}); await wait(async()=>await page.locator('.presence.speaking').count()===0,'speaker meter did not settle');
  emit('card',{delegation_id:'d1',text:'<script>window.injected=true</script> A useful result.',conversation_id:'room-coach'});
  emit('delegation',{id:'d1',state:'completed'});
  await wait(async()=>await page.locator('.output-card').count()===1,'result card missing');
  assert.equal(await page.locator('.output-card script').count(),0); assert.equal(await page.evaluate(()=>window.injected),undefined);
  assert.match(await page.locator('.output-card p').innerText(),/<script>/);
  await page.waitForTimeout(1100); assert.equal(await page.locator('.output-card').count(),1);
  checks.push('durable transcript/result events deduplicate; only actual audio energy animates speaking');

  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),false);assert.equal(await page.locator('#mute').getAttribute('aria-pressed'),'true');
  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),true);
  await page.locator('#camera-toggle').click();await page.locator('#camera-panel:not([hidden])').waitFor();
  assert.equal(requests.filter(request=>/\/attachments$/.test(request.path)).length,0);
  await page.locator('#capture-photo').click();await page.locator('#photo-form:not([hidden])').waitFor();
  assert.match(await page.locator('#photo-status').innerText(),/Not sent/);
  assert.equal(requests.filter(request=>/\/attachments$/.test(request.path)).length,0);
  let lostPhoto=false;
  await page.route('**/api/v1/conversations/*/messages',async route=>{
    if(!lostPhoto){lostPhoto=true;await route.fetch();await route.abort('failed');}else await route.continue();
  });
  await page.locator('#photo-question').fill('What is on my workbench?');await page.locator('#send-photo').click();
  await wait(async()=>await page.locator('#send-photo').innerText()==='Check the same send','lost photo receipt not surfaced');
  await page.locator('#photo-question').fill('Changed question');await page.locator('#send-photo').click();
  assert.match(await page.locator('#notice').innerText(),/original question/);
  await page.locator('#photo-question').fill('What is on my workbench?');await page.locator('#send-photo').click();
  await wait(async()=>await page.locator('#send-photo').innerText()==='Photo sent','photo was not sent');
  const photoRequest=requests.find(request=>/\/messages$/.test(request.path));
  assert.deepEqual(photoRequest.body.target_agent_ids,['coach']);assert.deepEqual(photoRequest.body.attachment_ids,['fixture-photo']);assert.equal(photoRequest.csrf,csrf);
  assert.equal(messages.size,1);assert.equal(requests.filter(request=>/\/attachments$/.test(request.path)).length,1);
  checks.push('camera stays local until explicit send; lost photo acknowledgment reuses the same message identity');

  pollFails=true;
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connection interrupted','network interruption not visible');
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,1);
  pollFails=false;await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','existing call did not recover');
  await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  const controls=await page.locator('.control-symbol').evaluateAll(nodes=>nodes.map(node=>({width:node.getBoundingClientRect().width,height:node.getBoundingClientRect().height})));
  assert(controls.every(size=>size.width>=44&&size.height>=44));
  await page.locator('#transcript-tab').click();assert.equal(await page.locator('#transcript-tab').getAttribute('aria-selected'),'true');
  checks.push('network recovery observes the same call; mobile controls and panels fit the viewport');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT,fullPage:true});

  await page.locator('#hangup').click();await page.locator('#call-ended:not([hidden])').waitFor();
  assert.match(await page.locator('#ended-detail').innerText(),/Closing/);
  await wait(async()=>calls.get('call-1').state==='closed','close not reported');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)&&fixture.channels.every(channel=>channel.closed)&&fixture.wakeLocks.every(lock=>lock.released)&&fixture.audioContexts.every(context=>context.closed)));
  assert(!requests.some(request=>/cancel|stop/.test(request.path)));
  assert.equal(await page.evaluate(()=>Object.keys(localStorage).length),0);
  checks.push('hangup releases devices, peer, audio and wake lock; close acceptance is not terminal; backend work is not cancelled');

  await page.locator('#back-to-contacts').click();events.length=0;
  let lostCreate = false;
  await page.route('**/api/v1/companion/calls', async route => {
    if (!lostCreate && route.request().method() === 'POST') { lostCreate=true; await route.fetch(); await route.abort('failed'); }
    else await route.continue();
  });
  await page.getByRole('button',{name:'Call Rook',exact:true}).click();await page.locator('#recover-call:not([hidden])').waitFor();
  const lostRequest=requests.filter(request=>request.path==='/api/v1/companion/calls').at(-1).body;
  await page.locator('#recover-call').click();await wait(async()=>await page.locator('#recover-call').isHidden(),'lost response recovery did not finish');
  const retriedRequest=requests.filter(request=>request.path==='/api/v1/companion/calls').at(-1).body;
  assert.deepEqual(lostRequest,retriedRequest);assert.equal(calls.size,2);assert.equal(calls.get('call-2').state,'closed');
  checks.push('lost creation response is resolved using the exact same UUID and SDP, with no duplicate call');

  await page.locator('#back-to-contacts').click();
  await page.getByRole('button',{name:'Call Jarvis',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','Jarvis fixture did not connect');
  await page.locator('#camera-toggle').click();await page.locator('#capture-photo').click();
  await page.locator('#photo-form:not([hidden])').waitFor();assert.equal(await page.locator('#send-photo').isDisabled(),true);
  assert.match(await page.locator('#photo-capability-note').innerText(),/does not currently accept image/);assert.equal(messages.size,1);
  await page.locator('#hangup').click();await wait(async()=>calls.get('call-3').state==='interrupted','Jarvis close not settled');
  await wait(async()=>/provider closure was not confirmed/.test(await page.locator('#ended-detail').innerText()),'unconfirmed provider closure was misreported');
  checks.push('known unsupported image input disables sending while retaining local camera preview');

  await page.locator('#back-to-contacts').click();
  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','outage fixture did not connect');
  controlFails=true;await page.evaluate(()=>{fixture.clockOffset+=16000;});
  await page.locator('#call-ended:not([hidden])').waitFor();
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)));
  assert.equal(calls.size,4);await wait(async()=>await page.locator('#recover-call').isEnabled(),'outage close uncertainty was not retained');
  assert.match(await page.locator('#ended-detail').innerText(),/not yet confirmed/);
  controlFails=false;await page.locator('#recover-call').click();await wait(async()=>await page.locator('#recover-call').isHidden(),'same-call outage recovery did not close');
  assert.equal(calls.size,4);checks.push('sustained control outage stops otherwise-live WebRTC and all devices, retaining same-call closure recovery without redial');

  await page.locator('#back-to-contacts').click();await page.evaluate(()=>{fixture.delayMic=true;});
  await page.getByRole('button',{name:'Call Jarvis',exact:true}).click();await page.locator('#call-screen:not([hidden])').waitFor();
  await page.locator('#hangup').click();await page.evaluate(()=>fixture.resolveMic());await page.waitForTimeout(150);
  assert.equal(calls.size,4);assert(await page.evaluate(()=>fixture.tracks.at(-1).stopped));
  checks.push('late microphone permission after hangup is released without creating a call');

  calls.set('reloaded-call',{id:'reloaded-call',owner:authOwner,conversation_id:'earlier-room',state:'connecting',request:{agent_id:'coach'}});catalogAvailable=false;
  const beforeReload=requests.filter(request=>request.path==='/api/v1/companion/calls').length;
  await page.reload();await page.getByRole('button',{name:'End earlier call',exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>fixture.media.length),0);assert.equal(await page.evaluate(()=>fixture.peers.length),0);
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,beforeReload);
  assert.match(await page.locator('#ended-detail').innerText(),/already accepted can continue/);
  assert.equal(await page.locator('#ended-work-link').getAttribute('href'),'/?conversation=earlier-room');
  await page.getByRole('button',{name:'End earlier call',exact:true}).click();
  await wait(async()=>await page.locator('#recover-call').isHidden(),'previous call did not close');
  assert.equal(calls.get('reloaded-call').state,'closed');assert.equal(await page.evaluate(()=>fixture.media.length),0);
  catalogAvailable=true;
  await page.locator('#back-to-contacts').click();await page.getByRole('button',{name:'Call Rook',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','fresh call blocked after explicit prior closure');
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,beforeReload+1);
  await page.locator('#hangup').click();await wait(async()=>await page.locator('#recover-call').isHidden(),'last fixture call did not close');
  checks.push('reload discovers the owner’s existing call without media or redial; explicit close permits a later fresh call');

  await page.locator('#back-to-contacts').click();events.length=0;
  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','logout fixture did not connect');
  emit('transcript',{role:'user',text:'Private prior caller text.',fragment:true,start_ms:0,end_ms:100});
  emit('card',{text:'Private prior caller result.',conversation_id:'room-coach'});
  await wait(async()=>await page.locator('.output-card').count()===1,'logout fixture result missing');
  holdNextClose=true;await page.locator('#sign-out').click();
  await page.locator('#login:not([hidden])').waitFor();await wait(async()=>heldClose!==null,'close was not pending at sign-out');
  assert.equal(await page.locator('.transcript-entry').count(),0);assert.equal(await page.locator('.output-card').count(),0);
  assert.equal(await page.locator('#continue-work').getAttribute('href'),'/');assert.equal(await page.locator('#ended-work-link').getAttribute('href'),'/');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)));
  await page.locator('#username').fill('next-fixture-user');await page.locator('#password').fill('fixture-only');await page.locator('#sign-in').click();
  await page.locator('#app:not([hidden])').waitFor();heldClose();heldClose=null;await page.waitForTimeout(200);
  assert.equal(await page.locator('#recover-call').isHidden(),true);assert.equal(await page.locator('#call-ended').isHidden(),true);
  assert.equal(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled(),true);
  assert.doesNotMatch(await page.locator('body').textContent(),/Private prior caller/);
  assert.equal(await page.evaluate(()=>Object.keys(localStorage).length+Object.keys(sessionStorage).length),0);
  checks.push('sign-out clears private presentation and devices; a delayed old close failure cannot contaminate the next owner’s login');
  assert.deepEqual(errors,[]);
  console.log(JSON.stringify({passed:checks.length,checks,scope:'real Chromium UI; fake API, media, and provider; no paid calls'},null,2));
} finally { await browser.close(); await new Promise(resolve=>server.close(resolve)); }
