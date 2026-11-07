// A real browser exercises the public caller UI against a disposable API and
// explicitly fake media. No microphone, OpenAI credentials or paid calls.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {randomUUID} from 'node:crypto';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const files = new Map(await Promise.all(['companion.html','companion.js','companion.css'].map(async name => [name,await readFile(new URL('../ui/' + name,import.meta.url))])));
const requests = [], calls = new Map(), messages = new Map(), uploads = new Map(), attachments = new Map(), voices = new Map(), events = [], checks = [];
const surfaces = new Map(), surfaceCommands = new Map(), permissions = new Map();
let holdNextSurfaceAck = false, heldSurfaceAck = null;
let failNextResult = false;
const fullResultText = "Complete saved answer. " + "Source fact. ".repeat(6000) + " FINAL QUALIFIER: broader testing remains incomplete. <script>window.fullResultInjected=true</script> [Source](https://example.com/source)";
let rejectNextUpload = false, holdNextUpload = false, heldUpload = null;
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
  if (url.pathname === '/') { response.writeHead(200,{'Content-Type':'text/html'}); response.end('<!doctype html><title>Fixture work</title><p>Conversation work surface</p>'); return; }
  if (/\/attachments\/[^/]+$/.test(url.pathname) && request.method === 'GET') { response.writeHead(200,{'Content-Type':'image/png'}); response.end(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==','base64')); return; }
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
    send(200,{available:catalogAvailable,active_call:active?{id:active.id,conversation_id:active.conversation_id,state:active.state}:null,contacts:[{id:'coach',label:'Coach',voice:voices.get('coach') || 'marin',description:'A thoughtful native teammate.'},{id:'rook',label:'Rook',voice:'cedar',description:'<img src=x onerror="window.injection=true"> Codex teammate.'},{id:'jarvis',label:'Jarvis',voice:'ash',description:'Local reasoning, cloud voice.',image_input:false}]}); return;
  }
  const agentPath = url.pathname.match(/^\/api\/v1\/companion\/agents\/([^/]+)\/(voice-settings|capabilities)$/);
  if (agentPath) {
    if (agentPath[2] === 'capabilities') { send(200,{image_input:agentPath[1] !== 'jarvis',capabilities_available:true}); return; }
    if (request.method === 'PUT') voices.set(agentPath[1],body.voice);
    send(200,{voice:voices.get(agentPath[1]) || 'marin',available_voices:['marin','cedar','ash'],applies_to:'next_call'}); return;
  }
  const uploadPath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)\/uploads$/);
  if (uploadPath) {
    const key = body.upload_id + ':' + body.state;
    if (!uploads.has(key)) {
      uploads.set(key,body); events.push({seq:(events.at(-1)?.seq || 0)+1,kind:'upload',data:{...body,conversation_id:calls.get(uploadPath[1]).conversation_id}});
    }
    send(200,{ok:true,upload_id:body.upload_id,state:body.state,voice_awareness:'queued'}); return;
  }
  if (url.pathname === '/api/v1/companion/calls' && request.method === 'POST') {
    let call = [...calls.values()].find(value => value.request.request_id === body.request_id);
    if (call && JSON.stringify(call.request) !== JSON.stringify(body)) { send(409,{error:'changed request'}); return; }
    if (!call) { call = {id:'call-' + nextCall++,owner:authOwner,request:body,conversation_id:'room-' + body.agent_id,session_id:'fixture-live',sdp:'fixture-answer',state:'connecting'}; calls.set(call.id,call); }
    send(200,call); return;
  }
  const resultPath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)\/results\/([^/]+)$/);
  if (resultPath) {
    if (failNextResult) { failNextResult=false; send(503,{error:'Fixture result temporarily unavailable.'}); return; }
    send(200,{message_id:resultPath[2],text:fullResultText}); return;
  }
  const surfacePath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)\/surface\/(presence|commands)(?:\/([^/]+)\/ack)?$/);
  if (surfacePath) {
    const call=calls.get(surfacePath[1]);if(!call || call.owner!==authOwner){send(404,{error:'missing call'});return;}
    if(surfacePath[2]==='presence') {surfaces.set(call.id,body);send(200,{...body,expires_at:Date.now()+30000});return;}
    if(request.method==='GET') {send(200,{commands:[...surfaceCommands.values()].filter(v=>v.callId===call.id&&v.browser_id===url.searchParams.get('browser_id')&&v.state==='queued')});return;}
    const command=surfaceCommands.get(surfacePath[3]);if(!command||command.callId!==call.id||command.browser_id!==body.browser_id){send(403,{error:'wrong command browser'});return;}
    if(body.state==='accepted') {
      const execute=command.state==='queued'&&command.expires_at>Date.now()&&!['closing','closed','interrupted'].includes(call.state);
      if(execute)command.state='accepted';const result={...command,execute};
      if(holdNextSurfaceAck){holdNextSurfaceAck=false;heldSurfaceAck=()=>send(200,result);return;}
      send(200,result);return;
    }
    if(body.state==='failed'&&command.state==='queued'){send(400,{error:'not accepted'});return;}
    if(body.state==='uploaded'&&(!uploads.has(command.operation_id+':available')||!body.attachment_ids?.every(id=>attachments.has(id)))){send(400,{error:'image not saved'});return;}
    command.state=body.state;command.artifact_ids=body.attachment_ids||[];send(200,{...command,execute:false});return;
  }
  const callPath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)(?:\/(heartbeat|close))?$/);
  if (callPath) {
    const call = calls.get(callPath[1]); if (!call) { send(404,{error:'missing call'}); return; }
    if (controlFails) { send(503,{error:'fixture control outage'}); return; }
    if (callPath[2] === 'close') {
      if (call.state !== 'closed') { call.state='closing'; call.closeChecks=2; }
      if (holdNextClose) { holdNextClose=false; heldClose=(status=503)=>send(status,status===200?{state:call.state}:{error:'fixture delayed close failure'}); return; }
      send(200,{state:call.state}); return;
    }
    if (callPath[2] === 'heartbeat') { send(200,{}); return; }
    if (pollFails) { send(503,{error:'fixture network outage'}); return; }
    if (call.state === 'closing' && --call.closeChecks <= 0) call.state=call.request.agent_id==='jarvis'?'interrupted':'closed';
    const after = Number(url.searchParams.get('after') || 0);
    send(200,{...call,permissions:[...permissions.values()].filter(p=>p.conversation_id===call.conversation_id),state:['closing','closed','interrupted'].includes(call.state)?call.state:'connected',events:events.filter(event => event.seq > after),next_cursor:events.at(-1)?.seq || 0}); return;
  }
  const permissionPath = url.pathname.match(/^\/api\/v1\/conversations\/([^/]+)\/permissions\/([^/]+)$/);
  if (permissionPath) {
    const permission=permissions.get(permissionPath[2]);
    if(!permission || permission.conversation_id!==permissionPath[1] || ![...calls.values()].some(call=>call.conversation_id===permission.conversation_id&&call.owner===authOwner)) {send(404,{error:'missing permission'});return;}
    if(body.option_id!==null&&!permission.options.some(o=>o.optionId===body.option_id)) {send(400,{error:'unoffered option'});return;}
    permissions.delete(permission.id);send(200,{ok:true});return;
  }
  if (/\/attachments$/.test(url.pathname)) {
    if (rejectNextUpload) { rejectNextUpload=false; send(400,{error:'Fixture upload rejected.'}); return; }
    const id='fixture-attachment-' + (attachments.size+1); attachments.set(id,body);
    if (holdNextUpload) { holdNextUpload=false; heldUpload=()=>send(200,{id}); return; }
    send(200,{id}); return;
  }
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
  const fixture = window.fixture = {media:[],tracks:[],peers:[],channels:[],level:0,wakeLocks:[],audioContexts:[],delayMic:false,denyCamera:false,draws:0,shutters:0};
  const actualNow=Date.now;fixture.clockOffset=0;Date.now=()=>actualNow()+fixture.clockOffset;
  const makeStream = kind => {
    const track = new EventTarget(); track.kind=kind; track.enabled=true; track.stopped=false; track.readyState='live';track.stop=()=>{track.stopped=true;track.readyState='ended';}; fixture.tracks.push(track);
    const stream = new MediaStream(); Object.defineProperties(stream,{getTracks:{value:()=>[track]},getAudioTracks:{value:()=>kind==='audio'?[track]:[]},getVideoTracks:{value:()=>kind==='video'?[track]:[]}}); return stream;
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
    constructor(){this.closed=false;this.state='running';this.sampleRate=48000;this.destination={};fixture.audioContexts.push(this);}
    async resume(){this.state='running';} async suspend(){this.state='suspended';} async close(){this.closed=true;this.state='closed';}
    createMediaStreamSource(){return {connect(){},disconnect(){}};}
    createAnalyser(){return {fftSize:256,getByteTimeDomainData(values){values.fill(128+fixture.level);}};}
    createBuffer(channels,length,rate){const samples=new Float32Array(length);return {getChannelData:()=>samples};}
    createBufferSource(){return {connect(){},disconnect(){},start(){if(this.buffer.getChannelData(0).some(x=>x!==0))fixture.shutters++;}};}
  };
  const playing=new WeakSet();
  Object.defineProperty(HTMLMediaElement.prototype,'paused',{get(){return !playing.has(this);}});
  HTMLMediaElement.prototype.play=async function(){playing.add(this);this.dispatchEvent(new Event('playing'));};
  HTMLMediaElement.prototype.pause=function(){playing.delete(this);this.dispatchEvent(new Event('pause'));};
  Object.defineProperty(HTMLVideoElement.prototype,'videoWidth',{get(){return 640;}});
  Object.defineProperty(HTMLVideoElement.prototype,'videoHeight',{get(){return 480;}});
  CanvasRenderingContext2D.prototype.drawImage=function(){fixture.draws++;};
});
const wait = async (condition,message) => {
  for (let i=0;i<150;i++) { if (await condition()) return; await page.waitForTimeout(50); }
  throw new Error(message);
};
const emit = (kind,data) => events.push({seq:(events.at(-1)?.seq || 0)+1,kind,data});
const surfaceCommand = (kind,command,options={}) => {
  const callId=options.callId||'call-1';
  const receipt={callId,operation_id:randomUUID(),delegation_id:'fixture-current-delegation',browser_id:surfaces.get(callId)?.browser_id,kind,command,state:'queued',created_at:Date.now(),expires_at:Date.now()+30000,artifact_ids:[],inspection:false,...options};
  surfaceCommands.set(receipt.operation_id,receipt);emit('surface_command',{...receipt});return receipt;
};
try {
  await page.goto(url + '/companion'); await page.locator('#login:not([hidden])').waitFor();
  assert.equal(await page.evaluate(()=>fixture.media.length),0);
  await page.locator('#username').fill('fixture-user'); await page.locator('#password').fill('fixture-only'); await page.locator('#sign-in').click();
  await page.locator('#app:not([hidden])').waitFor(); assert.equal(await page.locator('.contact-card').count(),3);
  assert.equal(await page.locator('.contact-card img').count(),0); assert.equal(await page.evaluate(()=>window.injection),undefined);
  assert.match(await page.locator('.call-note').innerText(),/local reasoning uses cloud audio/);
  assert.equal(await page.evaluate(()=>fixture.media.length),0); checks.push('login and contact rendering never acquire media; untrusted text stays text');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-contacts.png'),fullPage:true});

  await page.getByRole('button',{name:'Voice settings for Coach',exact:true}).click();
  await page.locator('#voice-choice:not([disabled])').waitFor(); await page.locator('#voice-choice').selectOption('ash'); await page.locator('#save-voice').click();
  await wait(async()=>/next call/.test(await page.locator('#voice-settings-status').innerText()),'voice save missing');
  assert.equal(voices.get('coach'),'ash'); assert.equal(requests.find(request=>request.method==='PUT').csrf,csrf);
  assert.equal(await page.evaluate(()=>fixture.media.length),0); await page.locator('#close-voice-settings').click();
  checks.push('discoverable per-agent voice choice persists through authenticated API and applies to the next call');

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
  const captionBeforeProgress=await page.locator('#caption-text').innerText();
  emit('progress',{delegation_id:'d1',request_id:'request-1',state:'submitted',elapsed_seconds:65});
  await wait(async()=>/65 seconds/.test(await page.locator('#work-status').innerText()),'quiet pending receipt missing');
  assert.equal(await page.locator('#caption-text').innerText(),captionBeforeProgress);assert.equal(await page.locator('.transcript-entry').count(),1);
  emit('progress',{delegation_id:'d1',request_id:'request-1',state:'awaiting_permission',elapsed_seconds:75});
  await wait(async()=>/waiting for your decision/.test(await page.locator('#work-status').innerText()),'permission receipt missing');
  checks.push('durable progress updates remain quiet status, preserve captions, and distinguish a pending result from a decision');
  emit('card',{delegation_id:'d1',text:'<script>window.injected=true</script> A useful result. [Project](https://example.com/project) [Bad](javascript:alert(1)) https://media.example/clip.mp4',media:[{url:'data:text/html,<script>alert(1)</script>',mime_type:'video/mp4'},{url:'/auth/logout',mime_type:'image/png'}],conversation_id:'room-coach'});
  emit('delegation',{id:'d1',state:'completed'});
  await wait(async()=>await page.locator('.output-card').count()===1,'result card missing');
  assert.equal(await page.locator('.output-card script').count(),0); assert.equal(await page.evaluate(()=>window.injected),undefined);
  assert.match(await page.locator('.output-card p').innerText(),/<script>/);
  await page.waitForTimeout(1100); assert.equal(await page.locator('.output-card').count(),1);assert.equal(await page.locator('#work-status').isHidden(),true);
  checks.push('durable transcript/result events deduplicate; only actual audio energy animates speaking');

  emit('provider',{type:'error',error:{client_event_id:'append-fixture',code:'private-error-code',message:'PRIVATE PROVIDER DETAILS'}});
  await wait(async()=>/voice update could not be confirmed/.test(await page.locator('#notice-text').innerText()),'rejected voice update was invisible');
  await page.evaluate(()=>{fixture.noticeChanges=0;fixture.noticeObserver=new MutationObserver(()=>fixture.noticeChanges++);fixture.noticeObserver.observe(document.getElementById('notice-text'),{childList:true,characterData:true,subtree:true});});
  emit('context_delivery',{state:'provider_error',event_id:'append-fixture'});
  emit('transcript',{role:'assistant',text:'Conversation continues after the rejected update.',fragment:true,playback:'unconfirmed',start_ms:4000,end_ms:4500});
  await wait(async()=>/Conversation continues/.test(await page.locator('#caption-text').innerText()),'transcripts stopped after a server error');
  assert.equal(await page.evaluate(()=>fixture.noticeChanges),0,'the correlated receipt must not duplicate the provider notice');
  assert.equal(await page.locator('.output-card').count(),1);assert.equal(await page.locator('#call-status-text').innerText(),'Connected');
  assert(await page.evaluate(()=>!fixture.peers[0].closed));assert(!requests.some(request=>request.path.endsWith('/close')));
  assert.doesNotMatch(await page.locator('#notice-text').innerText(),/PRIVATE PROVIDER DETAILS|private-error-code/);
  emit('provider',{type:'error',error:{message:'OTHER PRIVATE DETAILS'}});
  await wait(async()=>/voice service reported a problem/.test(await page.locator('#notice-text').innerText()),'uncorrelated server error was invisible');
  assert.equal(await page.locator('#call-status-text').innerText(),'Connected');
  await page.evaluate(()=>fixture.noticeObserver.disconnect());await page.locator('#dismiss-notice').click();
  checks.push('provider errors show one nonfatal safe notice; correlated receipts stay quiet, saved results remain, and later speech continues without retry or hangup');

  assert.equal(await page.locator('.output-card a[href="https://example.com/project"]').count(),1);
  assert.equal(await page.locator('.output-card a[href^="javascript:"]').count(),0);
  assert.equal(await page.locator('.output-card iframe,.output-card img[src="/auth/logout"]').count(),0);
  assert.equal(await page.locator('.output-card video').count(),0);
  assert.equal(await page.getByRole('button',{name:'Load video preview',exact:true}).count(),1);
  let remoteMediaLoads=0; await page.route('https://media.example/**',async route=>{remoteMediaLoads++;await route.fulfill({status:200,contentType:'video/mp4',body:''});});
  await page.getByRole('button',{name:'Load video preview',exact:true}).click();
  assert.equal(await page.locator('.output-card video').getAttribute('src'),'https://media.example/clip.mp4');
  assert.equal(await page.locator('.output-card video').getAttribute('autoplay'),null);
  const popup = context.waitForEvent('page'); await page.locator('#continue-work').click(); const workPage = await popup; await workPage.waitForLoadState();
  await page.locator('.conversation-link').first().click(); await page.waitForTimeout(100);
  assert.equal(context.pages().length,2); assert.match(workPage.url(),/conversation=room-coach/);
  assert.equal(await page.locator('#call-screen').isVisible(),true); await workPage.close();
  checks.push('answer links stay safe and clickable; media is opt-in without HTML; conversation links reuse one work tab without ending audio');

  emit('card',{title:'Complete answer',text:'A shortened saved answer.',truncated:true,message_id:'full-result-1',result_url:'/api/v1/companion/calls/call-1/results/full-result-1',conversation_id:'room-coach',late:true,request_text:'The live voice conversation delegated help at 00:42.\nDelegation ID: private-delegation-1\nCaller request: Earlier research question'});
  await page.getByRole('button',{name:'Show full result',exact:true}).waitFor();
  const fullCard=page.locator('.output-card').nth(1);
  assert.equal(await fullCard.locator('.card-context').innerText(),'Answer to your earlier request');
  assert.doesNotMatch(await page.locator('body').innerText(),/The live voice conversation delegated help at|private-delegation-1|Earlier research question/);
  failNextResult=true;await fullCard.getByRole('button',{name:'Show full result'}).click();
  await wait(async()=>/temporarily unavailable/.test(await fullCard.locator('.result-status').innerText()),'full result failure missing');
  assert.match(await fullCard.locator('.card-answer').innerText(),/shortened/);
  await fullCard.getByRole('button',{name:'Show full result'}).click();
  await wait(async()=>/Full saved result shown/.test(await fullCard.locator('.result-status').innerText()),'full saved result did not load');
  assert.match(await fullCard.locator('.card-answer').innerText(),/FINAL QUALIFIER: broader testing remains incomplete/);
  assert.equal(await fullCard.locator('script').count(),0);assert.equal(await page.evaluate(()=>window.fullResultInjected),undefined);
  assert.equal(await fullCard.locator('a[href="https://example.com/source"]').count(),1);
  assert.equal(requests.filter(request=>request.path.endsWith('/results/full-result-1')).length,2);
  emit('card',{title:'Invalid result URL',text:'Short result.',truncated:true,message_id:'full-result-2',result_url:'https://media.example/full-result',conversation_id:'room-coach'});
  emit('card',{title:'Video demonstration',text:'https://media.example/watch?id=demo',url:'https://media.example/watch?id=demo',kind:'video',conversation_id:'room-coach'});
  emit('card',{title:'Reference only',text:'https://media.example/clip.mp4',url:'https://media.example/clip.mp4',kind:'link',conversation_id:'room-coach'});
  await wait(async()=>await page.locator('.output-card').count()===5,'explicit media cards missing');
  assert.equal(await page.locator('.output-card').nth(2).locator('.full-result-button').count(),0);
  const explicitVideo=page.locator('.output-card').nth(3);
  assert.match(await explicitVideo.locator('h2').innerText(),/Video demonstration/);
  assert.equal(await explicitVideo.locator('video').count(),0);
  await explicitVideo.getByRole('button',{name:'Load video preview',exact:true}).click();
  assert.equal(await explicitVideo.locator('video').getAttribute('src'),'https://media.example/watch?id=demo');
  assert.equal(await explicitVideo.locator('video').getAttribute('autoplay'),null);
  assert.equal(await page.locator('.output-card').nth(4).locator('.media-preview-button,video,iframe').count(),0);
  checks.push('full saved results load only from this call’s authenticated route and remain safe text; explicit extensionless video previews need a click and link-only cards stay links');


  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),false);assert.equal(await page.locator('#mute').getAttribute('aria-pressed'),'true');
  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),true);
  await page.locator('#camera-toggle').click();await page.locator('#camera-panel:not([hidden])').waitFor();
  assert.equal(requests.filter(request=>/\/attachments$/.test(request.path)).length,0);
  let lostNotification=false;
  await page.route('**/api/v1/companion/calls/*/uploads',async route=>{
    if (!lostNotification && route.request().postDataJSON().state === 'available') { lostNotification=true; await route.fetch(); await route.abort('failed'); }
    else await route.continue();
  });
  holdNextUpload=true;await page.locator('#capture-photo').click();
  await wait(async()=>heldUpload!==null,'capture did not start upload');
  assert.equal(await page.locator('.upload-card').first().getAttribute('data-state'),'uploading');
  heldUpload();heldUpload=null;
  const photo = page.locator('.upload-card').first();
  await wait(async()=>/notification is unconfirmed/.test(await photo.innerText()),'lost availability receipt not surfaced');
  assert.equal(await page.locator('#photo-question').count(),0);
  assert.equal(await photo.getAttribute('data-state'),'available');
  await photo.getByRole('button',{name:'Check voice notification'}).click();
  await wait(async()=>/notification queued/.test(await photo.innerText()),'same availability was not acknowledged');
  assert.equal(attachments.size,1); assert.equal(messages.size,0);
  assert.deepEqual([...uploads.values()].map(upload=>upload.state),['captured','uploading','available']);
  const sharedPhoto = [...uploads.values()].find(upload=>upload.state==='available');
  assert.equal(sharedPhoto.source,'camera'); assert.deepEqual(sharedPhoto.attachment_ids,['fixture-attachment-1']);
  const notices=requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available');
  assert.deepEqual(notices[0].body,notices[1].body); assert(notices.every(request=>request.csrf===csrf));
  assert.equal([...uploads.values()].filter(upload=>upload.state==='available').length,1);
  checks.push('Capture immediately uploads without a question; same-ID voice notice recovers a lost acknowledgment without duplicate storage or forced summary');
  await page.locator('#camera-toggle').click();

  await page.evaluate(()=>{
    const clipboardData=new DataTransfer(); clipboardData.setData('text/plain','Notes pasted while talking.');
    document.getElementById('space-panel').dispatchEvent(new ClipboardEvent('paste',{bubbles:true,cancelable:true,clipboardData}));
  });
  await wait(async()=>attachments.size===2,'pasted text was not shared');
  await page.evaluate(()=>{
    const dataTransfer=new DataTransfer(); dataTransfer.items.add(new File(['%PDF-1.7\nfixture'],'notes.pdf',{type:'application/pdf'}));
    document.getElementById('space-panel').dispatchEvent(new DragEvent('drop',{bubbles:true,cancelable:true,dataTransfer}));
  });
  await wait(async()=>attachments.size===3,'dropped PDF was not shared');
  await page.evaluate(()=>{
    const clipboardData=new DataTransfer(); clipboardData.items.add(new File([Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg=='),c=>c.charCodeAt(0))],'pasted.png',{type:'image/png'}));
    document.getElementById('space-panel').dispatchEvent(new ClipboardEvent('paste',{bubbles:true,cancelable:true,clipboardData}));
  });
  await wait(async()=>attachments.size===4,'pasted image was not shared');
  assert.equal(attachments.get('fixture-attachment-2').mime_type,'text/plain');
  assert.equal(Buffer.from(attachments.get('fixture-attachment-2').data,'base64').toString(),'Notes pasted while talking.');
  assert.equal(attachments.get('fixture-attachment-3').mime_type,'application/pdf');
  assert.equal(attachments.get('fixture-attachment-4').mime_type,'image/png');
  await page.locator('#share-files').setInputFiles({name:'not-allowed.html',mimeType:'text/html',buffer:Buffer.from('<script>bad()</script>')});
  await wait(async()=>await page.locator('.upload-card[data-state="failed"]').count()===1,'unsupported file state missing');
  assert.equal(attachments.size,4);
  rejectNextUpload=true;
  await page.locator('#share-files').setInputFiles({name:'rejected.txt',mimeType:'text/plain',buffer:Buffer.from('Reject this fixture.')});
  await wait(async()=>await page.locator('.upload-card[data-state="failed"]').count()===2,'rejected upload state missing');
  assert.equal(attachments.size,4);
  let lostUpload=false;
  await page.route('**/api/v1/conversations/*/attachments',async route=>{if(!lostUpload){lostUpload=true;await route.fetch();await route.abort('failed');}else await route.continue();});
  await page.locator('#share-files').setInputFiles({name:'uncertain.txt',mimeType:'text/plain',buffer:Buffer.from('Uncertain receipt.')});
  await wait(async()=>await page.locator('.upload-card[data-state="unconfirmed"]').count()===1,'uncertain upload state missing');
  assert.equal(attachments.size,5); await page.waitForTimeout(250); assert.equal(attachments.size,5);
  assert.equal(await page.locator('.upload-card[data-state="unconfirmed"] button:not([hidden])').count(),0);
  checks.push('paste/drop share UTF-8 text, images, and PDFs; unsupported and rejected files are distinct; unknown upload outcomes are never replayed');

  await wait(async()=>surfaces.get('call-1')?.camera_enabled===false,'browser surface presence missing');
  let beforeDraws=await page.evaluate(()=>fixture.draws),beforeStorage=attachments.size;
  const beforeUnconsentedShutters=await page.evaluate(()=>fixture.shutters);
  const beforeSensorRequests=await page.evaluate(()=>fixture.media.length);
  const disabledCapture=surfaceCommand('capture',{photos:1,max_bytes:524288});
  await wait(async()=>requests.some(r=>r.path.endsWith(`/${disabledCapture.operation_id}/ack`)),'disabled capture was not handled');
  assert.equal(await page.evaluate(()=>fixture.media.length),beforeSensorRequests);assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws);
  assert.ok(!requests.some(r=>r.path.endsWith(`/${disabledCapture.operation_id}/ack`)&&r.body.state==='accepted'));
  await page.locator('#camera-toggle').click();await page.locator('#camera-panel:not([hidden])').waitFor();
  await wait(async()=>surfaces.get('call-1')?.camera_enabled===true,'camera-ready presence missing');
  assert.equal(surfaces.get('call-1').capture_consent,false);
  const noConsent=surfaceCommand('capture',{photos:1,max_bytes:524288});
  await wait(async()=>requests.some(r=>r.path.endsWith(`/${noConsent.operation_id}/ack`)),'unconsented capture was not handled');
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws);assert.equal(attachments.size,beforeStorage);
  assert.equal(await page.evaluate(()=>fixture.shutters),beforeUnconsentedShutters,'an unconsented request must not sound like a captured photo');
  await page.locator('#voice-photos').check();await wait(async()=>surfaces.get('call-1')?.capture_consent===true,'explicit voice photo consent missing');
  checks.push('a voice capture cannot enable a camera or take a frame before the browser’s explicit consent');

  const shuttersBefore=await page.evaluate(()=>fixture.shutters);
  const photoCommand=surfaceCommand('capture',{photos:1,max_bytes:524288});
  await wait(async()=>photoCommand.state==='uploaded','voice photo was not saved and acknowledged');
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws+1);assert.equal(attachments.size,beforeStorage+1);
  assert.deepEqual(requests.filter(r=>r.path.endsWith(`/${photoCommand.operation_id}/ack`)).map(r=>r.body.state),['accepted','executed','uploaded']);
  const photoEvents=[...uploads.values()].filter(v=>v.upload_id===photoCommand.operation_id);
  assert.deepEqual(photoEvents.map(v=>v.state),['captured','uploading','available']);assert(photoEvents.every(v=>v.source==='camera'));
  assert.equal(await page.evaluate(()=>fixture.shutters),shuttersBefore+1,'captured frame must play one shutter cue');
  assert.deepEqual(photoCommand.artifact_ids,photoEvents.at(-1).attachment_ids);
  const saved=attachments.get(photoCommand.artifact_ids[0]);assert.equal(saved.mime_type,'image/jpeg');assert(Buffer.from(saved.data,'base64').length<=524288);
  emit('surface_command',{...photoCommand,state:'queued'});await page.waitForTimeout(1100);
  assert.equal(await page.evaluate(()=>fixture.shutters),shuttersBefore+1,'replayed command must not replay a shutter cue');
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws+1);assert.equal(attachments.size,beforeStorage+1);
  assert.equal(requests.filter(r=>r.path.endsWith(`/${photoCommand.operation_id}/ack`)&&r.body.state==='accepted').length,1);assert.equal(messages.size,0);
  checks.push('one consented voice photo uses the operation UUID for saved upload evidence and duplicate commands never recapture or force a new summary');

  beforeDraws=await page.evaluate(()=>fixture.draws);beforeStorage=attachments.size;
  const lostAccept=surfaceCommand('capture',{photos:1,max_bytes:524288});
  await page.route(`**/surface/commands/${lostAccept.operation_id}/ack`,async route=>{if(route.request().postDataJSON().state==='accepted'){await route.fetch();await route.abort('failed');}else await route.continue();});
  await wait(async()=>lostAccept.state==='accepted','lost acceptance fixture did not reach backend');await page.waitForTimeout(100);
  emit('surface_command',{...lostAccept,state:'queued'});await page.waitForTimeout(1100);
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws);assert.equal(attachments.size,beforeStorage);
  assert.equal(await page.evaluate(()=>fixture.shutters),shuttersBefore+1,'an unconfirmed acceptance must not sound like a captured photo');
  assert.equal(requests.filter(r=>r.path.endsWith(`/${lostAccept.operation_id}/ack`)&&r.body.state==='accepted').length,1);
  const lostExecuted=surfaceCommand('capture',{photos:1,max_bytes:524288});
  await page.route(`**/surface/commands/${lostExecuted.operation_id}/ack`,async route=>{if(route.request().postDataJSON().state==='executed'){await route.fetch();await route.abort('failed');}else await route.continue();});
  await wait(async()=>lostExecuted.state==='executed','lost execution receipt fixture did not capture');await page.waitForTimeout(100);
  emit('surface_command',{...lostExecuted,state:'queued'});await page.waitForTimeout(1100);
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws+1);assert.equal(attachments.size,beforeStorage);
  assert.equal(await page.evaluate(()=>fixture.shutters),shuttersBefore+2,'a lost execution receipt must not replay the shutter');
  const expired=surfaceCommand('capture',{photos:1,max_bytes:524288},{expires_at:Date.now()-1});await page.waitForTimeout(1100);
  assert.ok(!requests.some(r=>r.path.endsWith(`/${expired.operation_id}/ack`)));assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws+1);
  holdNextSurfaceAck=true;const staleAccepted=surfaceCommand('capture',{photos:1,max_bytes:524288},{expires_at:Date.now()+1800});
  await wait(async()=>heldSurfaceAck!==null,'delayed accept fixture did not reach backend');await page.waitForTimeout(1850);heldSurfaceAck();heldSurfaceAck=null;await page.waitForTimeout(150);
  assert.equal(await page.evaluate(()=>fixture.draws),beforeDraws+1);assert.equal(attachments.size,beforeStorage);
  checks.push('lost accepted/executed receipts and expired queued/accepted commands never replay a frame or an upload');
  await page.locator('#camera-toggle').click();assert.equal(await page.locator('#voice-photos').isChecked(),false);

  const desktopControls = await page.locator('#hangup').boundingBox();
  const cardsBeforeAccumulation=await page.locator('.output-card').count();
  for(let i=0;i<30;i++) emit('card',{text:'Result '+i+'\n'+ 'Long result content. '.repeat(50),conversation_id:'room-coach'});
  await wait(async()=>await page.locator('.output-card').count()===Math.min(40,cardsBeforeAccumulation+30),'accumulated cards did not arrive');
  const desktopAfter = await page.locator('#hangup').boundingBox(); assert(Math.abs(desktopControls.y-desktopAfter.y)<1);
  assert(desktopAfter.y+desktopAfter.height<=900);
  assert(await page.locator('#space-panel').evaluate(node=>node.scrollHeight>node.clientHeight));
  await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
  assert(await page.locator('#space-panel').evaluate(node=>node.scrollTop>0));
  assert(await page.evaluate(()=>document.documentElement.scrollHeight<=innerHeight));
  checks.push('long result lists scroll inside a bounded desktop shared pane with fixed call controls');
  const focusId=await page.locator('.output-card[data-card-id]').first().getAttribute('data-card-id');
  const beforeViewSensors=await page.evaluate(()=>fixture.media.length),beforeViewFrames=await page.evaluate(()=>fixture.draws);
  const focus=surfaceCommand('view',{action:'focus_card',card_id:focusId});await wait(async()=>focus.state==='executed','focus-card command did not execute');
  assert.equal(await page.locator(`.output-card[data-card-id="${focusId}"].surface-focused`).count(),1);
  const transcript=surfaceCommand('view',{action:'show_transcript',card_id:null});await wait(async()=>transcript.state==='executed','transcript command missing');assert.equal(await page.locator('#transcript-tab').getAttribute('aria-selected'),'true');
  const shared=surfaceCommand('view',{action:'show_shared',card_id:null});await wait(async()=>shared.state==='executed','shared pane command missing');
  const scroll=surfaceCommand('view',{action:'scroll_down',card_id:null});await wait(async()=>scroll.state==='executed','bounded scroll command missing');
  assert.equal(await page.evaluate(()=>scrollY),0);const afterView=await page.locator('#hangup').boundingBox();assert(Math.abs(desktopAfter.y-afterView.y)<1);
  assert.equal(await page.evaluate(()=>fixture.media.length),beforeViewSensors);assert.equal(await page.evaluate(()=>fixture.draws),beforeViewFrames);
  checks.push('agent view controls focus owned cards and switch or scroll only the contained pane without moving hangup or opening media');
  const permission={id:randomUUID(),conversation_id:'room-coach',agent:'Coach',expires_at:Date.now()+60000,tool_call:{title:'Check Minecraft status',rawInput:{command:'<img src=x onerror="window.permissionInjected=true">'}},options:[{optionId:'always',name:'Always',kind:'allow_always'},{optionId:'once',name:'Read basic status',kind:'allow_once'},{optionId:'detail',name:'Read detailed status',kind:'allow_once'},{optionId:'no',name:'Reject once',kind:'reject_once'}]};
  permissions.set(permission.id,permission);
  await page.locator('#transcript-tab').click();
  await page.locator('#permission-alert:not([hidden])').waitFor();await page.locator('#permission-alert').click();
  const approval=page.locator('.call-permission');await approval.locator('summary').click();
  assert.match(await approval.locator('pre').innerText(),/<img src=x/);assert.equal(await page.evaluate(()=>window.permissionInjected),undefined);
  assert.equal(await approval.getByRole('button',{name:'Always',exact:true}).count(),0);
  assert.equal(await approval.getByRole('button',{name:'Read detailed status (this request only)',exact:true}).count(),1);
  await approval.getByRole('button',{name:'Read basic status (this request only)',exact:true}).click();
  await wait(async()=>permissions.size===0,'permission decision was not received');
  const decision=requests.find(r=>r.path.endsWith(`/permissions/${permission.id}`));assert.deepEqual(decision.body,{option_id:'once'});assert.equal(decision.csrf,csrf);
  await page.locator('#permission-alert').waitFor({state:'hidden'});
  checks.push('native approval is visible during a call; safe action details preserve distinct offered choices and an exact single-use decision uses the existing private permission endpoint');
  const uncertainPermission={...permission,id:randomUUID(),tool_call:{title:'Read another status'},options:[{optionId:'once',name:'Allow once',kind:'allow_once'}]};
  permissions.set(uncertainPermission.id,uncertainPermission);
  let uncertainDecisionAttempts=0;
  await page.route(`**/permissions/${uncertainPermission.id}`,async route=>{
    uncertainDecisionAttempts++; await route.fetch();
    // Simulate a stale pending snapshot while the decision reply is lost.
    permissions.set(uncertainPermission.id,uncertainPermission);await route.abort('failed');
  });
  const uncertainCard=page.locator(`.call-permission[data-permission-id="${uncertainPermission.id}"]`);
  await uncertainCard.getByRole('button',{name:'Allow once (this request only)',exact:true}).click();
  await wait(async()=>/receipt is unconfirmed/.test(await uncertainCard.innerText()),'lost decision response must stay uncertain');
  assert(await uncertainCard.getByRole('button',{name:'Allow once (this request only)',exact:true}).isDisabled());
  const anotherPermission={...uncertainPermission,id:randomUUID(),tool_call:{title:'Independent pending action'}};
  permissions.set(anotherPermission.id,anotherPermission);
  await page.locator(`.call-permission[data-permission-id="${anotherPermission.id}"]`).waitFor();
  assert(await uncertainCard.getByRole('button',{name:'Allow once (this request only)',exact:true}).isDisabled(),'a rerender must not re-enable an uncertain decision');
  assert.equal(uncertainDecisionAttempts,1);permissions.clear();
  await page.locator('#permission-alert').waitFor({state:'hidden'});
  checks.push('a lost permission decision response remains uncertain across later snapshots and never retries or re-enables its choices');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-desktop-shared.png'),fullPage:true});

  pollFails=true;
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connection interrupted','network interruption not visible');
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,1);
  pollFails=false;await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','existing call did not recover');
  await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  assert(await page.evaluate(()=>document.documentElement.scrollHeight<=innerHeight));
  const mobileHangup=await page.locator('#hangup').boundingBox();assert(mobileHangup.y>=0&&mobileHangup.y+mobileHangup.height<=844);
  assert(await page.locator('#space-panel').evaluate(node=>node.scrollHeight>node.clientHeight));
  await page.locator('#space-panel').evaluate(node=>{node.scrollTop=0;});
  const mobileAfter=await page.locator('#hangup').boundingBox();assert(Math.abs(mobileHangup.y-mobileAfter.y)<1);
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-mobile-shared.png'),fullPage:true});
  await page.setViewportSize({width:844,height:390});
  const landscapeHangup=await page.locator('#hangup').boundingBox();assert(landscapeHangup.y>=0&&landscapeHangup.y+landscapeHangup.height<=390);
  assert(await page.evaluate(()=>document.documentElement.scrollHeight<=innerHeight&&document.documentElement.scrollWidth<=innerWidth));
  await page.setViewportSize({width:390,height:844});
  const controls=await page.locator('.control-symbol').evaluateAll(nodes=>nodes.map(node=>({width:node.getBoundingClientRect().width,height:node.getBoundingClientRect().height})));
  assert(controls.every(size=>size.width>=44&&size.height>=44));
  await page.locator('#transcript-tab').click();assert.equal(await page.locator('#transcript-tab').getAttribute('aria-selected'),'true');
  checks.push('network recovery observes the same call; mobile controls and panels fit the viewport');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT,fullPage:true});

  holdNextSurfaceAck=true;const lateView=surfaceCommand('view',{action:'show_shared',card_id:null});
  await wait(async()=>heldSurfaceAck!==null,'late surface acceptance fixture not ready');
  holdNextClose=true;
  const voiceHangup=surfaceCommand('hangup',{action:'end_call'});
  await page.locator('#call-ended:not([hidden])').waitFor();
  await wait(async()=>voiceHangup.state==='executed','voice hangup did not acknowledge released media');
  await wait(async()=>heldClose!==null,'graceful close was not requested');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)));
  assert(await page.evaluate(()=>fixture.peers.every(peer=>!peer.closed)&&fixture.channels.every(channel=>!channel.closed)&&fixture.audioContexts.every(context=>!context.closed)),'silent transport must remain until terminal close');
  heldClose(200);heldClose=null;
  heldSurfaceAck();heldSurfaceAck=null;await page.waitForTimeout(100);
  assert.ok(!requests.some(r=>r.path.endsWith(`/${lateView.operation_id}/ack`)&&r.body.state==='executed'));
  assert.match(await page.locator('#ended-detail').innerText(),/Closing/);
  await wait(async()=>calls.get('call-1').state==='closed','close not reported');
  await wait(async()=>page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)&&fixture.channels.every(channel=>channel.closed)&&fixture.wakeLocks.every(lock=>lock.released)&&fixture.audioContexts.every(context=>context.closed)),'terminal close did not release retained transports');
  assert(!requests.some(request=>/cancel|stop/.test(request.path)));
  assert.equal(await page.evaluate(()=>Object.keys(localStorage).length),0);
  checks.push('voice hangup releases devices, peer, audio and wake lock; close acceptance is not terminal; backend work is not cancelled');

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
  await wait(async()=>/cannot inspect/.test(await page.locator('#share-capability').innerText()),'effective capability missing before capture');
  await page.locator('#camera-toggle').click();
  const shuttersBeforeSuspended=await page.evaluate(async()=>{await fixture.audioContexts.at(-1).suspend();return fixture.shutters;});
  await page.locator('#capture-photo').click();
  await wait(async()=>await page.locator('.upload-card[data-state="available"]').count()===1,'unsupported-image storage missing');
  assert.match(await page.locator('.upload-card').innerText(),/cannot inspect images/);assert.equal(messages.size,0);
  assert.match(await page.locator('#notice-text').innerText(),/Photo captured.*shutter sound is unavailable/);
  assert.equal(await page.evaluate(()=>fixture.shutters),shuttersBeforeSuspended);
  checks.push('a suspended audio context shows that the photo was captured and sound was unavailable, without repeating a cue or preventing storage');
  await page.locator('#hangup').click();await wait(async()=>calls.get('call-3').state==='interrupted','Jarvis close not settled');
  await wait(async()=>/provider closure was not confirmed/.test(await page.locator('#ended-detail').innerText()),'unconfirmed provider closure was misreported');
  checks.push('effective image limitation appears before capture; saving remains available without claiming the agent can see it');

  await page.locator('#back-to-contacts').click();
  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','outage fixture did not connect');
  controlFails=true;await page.evaluate(()=>{fixture.clockOffset+=16000;});
  await page.locator('#call-ended:not([hidden])').waitFor();
  await wait(async()=>page.evaluate(()=>fixture.peers.every(peer=>peer.closed)),'failed close did not release retained transports');
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
  holdNextUpload=true;await page.locator('#share-files').setInputFiles({name:'late-private.txt',mimeType:'text/plain',buffer:Buffer.from('Private pending upload.')});
  await wait(async()=>heldUpload!==null,'late upload fixture did not reach storage');
  const availableBeforeLogout = requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available').length;
  holdNextClose=true;await page.locator('#sign-out').click();
  await page.locator('#login:not([hidden])').waitFor();await wait(async()=>heldClose!==null,'close was not pending at sign-out');
  assert.equal(await page.locator('.transcript-entry').count(),0);assert.equal(await page.locator('.output-card').count(),0);
  assert.equal(await page.locator('#continue-work').getAttribute('href'),'/');assert.equal(await page.locator('#ended-work-link').getAttribute('href'),'/');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)));
  await page.locator('#username').fill('next-fixture-user');await page.locator('#password').fill('fixture-only');await page.locator('#sign-in').click();
  await page.locator('#app:not([hidden])').waitFor();heldClose();heldClose=null;heldUpload();heldUpload=null;await page.waitForTimeout(200);
  assert.equal(requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available').length,availableBeforeLogout);
  assert.equal(await page.locator('#recover-call').isHidden(),true);assert.equal(await page.locator('#call-ended').isHidden(),true);
  assert.equal(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled(),true);
  assert.doesNotMatch(await page.locator('body').textContent(),/Private prior caller/);
  assert.equal(await page.evaluate(()=>Object.keys(localStorage).length+Object.keys(sessionStorage).length),0);
  checks.push('sign-out clears private presentation and devices; delayed old upload receipts and close failures cannot contaminate the next owner’s login');
  assert.deepEqual(errors,[]);
  console.log(JSON.stringify({passed:checks.length,checks,scope:'real Chromium UI; fake API, media, and provider; no paid calls'},null,2));
} finally { await browser.close(); await new Promise(resolve=>server.close(resolve)); }
