// A real browser exercises the public caller UI against a disposable API and
// explicitly fake media. No microphone, OpenAI credentials or paid calls.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {randomUUID} from 'node:crypto';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const files = new Map(await Promise.all(['companion.html','companion.js','companion.css','companion-floor.mjs'].map(async name => [name,await readFile(new URL('../ui/' + name,import.meta.url))])));
const requests = [], calls = new Map(), messages = new Map(), uploads = new Map(), attachments = new Map(), voices = new Map(), events = [], checks = [];
const surfaces = new Map(), surfaceCommands = new Map(), permissions = new Map();
let holdNextSurfaceAck = false, heldSurfaceAck = null;
let failNextResult = false;
const fullResultText = "Complete saved answer. " + "Source fact. ".repeat(6000) + " FINAL QUALIFIER: broader testing remains incomplete. <script>window.fullResultInjected=true</script> [Source](https://example.com/source)";
let rejectNextUpload = false, holdNextUpload = false, heldUpload = null;
let loggedIn = false, pollFails = false, heartbeatFails = false, controlFails = false, catalogAvailable = true, catalogFails = false, nextCall = 1;
let authOwner = 'fixture-user', holdNextClose = false, heldClose = null;
const csrf = 'fixture-csrf';
const server = createServer(async (request,response) => {
  const url = new URL(request.url,'http://fixture');
  const send = (status,value) => { response.writeHead(status,{'Content-Type':'application/json','Cache-Control':'no-store'}); response.end(JSON.stringify(value)); };
  if (['/companion','/companion.html','/companion.js','/companion.css','/companion-floor.mjs'].includes(url.pathname)) {
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
    if (catalogFails) { send(503,{error:'Fixture contacts temporarily unavailable.'}); return; }
    const active=[...calls.values()].find(call=>call.owner===authOwner&&['starting','connecting','connected','closing'].includes(call.state));
    send(200,{available:catalogAvailable,active_call:active?{id:active.id,conversation_id:active.conversation_id,state:active.state}:null,contacts:[{id:'coach',label:'Coach',voice:voices.get('coach') || 'marin',description:'A thoughtful native teammate.'},{id:'rook',label:'Rook',voice:'cedar',description:'<img src=x onerror="window.injection=true"> Codex teammate.'},{id:'jarvis',label:'Jarvis',voice:'ash',description:'Local reasoning, cloud voice.',image_input:false}]}); return;
  }
  const agentPath = url.pathname.match(/^\/api\/v1\/companion\/agents\/([^/]+)\/(voice-settings|capabilities)$/);
  if (agentPath) {
    if (agentPath[2] === 'capabilities') { send(200,{image_input:agentPath[1] !== 'jarvis',capabilities_available:true}); return; }
    if (request.method === 'PUT') voices.set(agentPath[1],body.voice);
    send(200,{voice:voices.get(agentPath[1]) || 'marin',available_voices:['marin','cedar','ash'],applies_to:'next_call',voice_provider:'OpenAI',voice_model:'fixture-live',available_voice_models:['fixture-live'],backend_settings_editable:false,backend:agentPath[1]==='jarvis'?null:{agent:{name:'fixture-runtime',title:'Fixture reasoning runtime'},model:{id:'fixture-model',name:'Fixture reasoning model'},observed_at:1794157902184,options:[{id:'reasoning',name:'Thinking',category:'thought_level',value:'medium',label:'Medium',editable:false}]}}); return;
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
    if (!call) { call = {id:'call-' + nextCall++,owner:authOwner,request:body,conversation_id:'room-' + body.agent_id,session_id:'fixture-live',sdp:'fixture-answer',voice:voices.get(body.agent_id)||'marin',state:'connecting'}; calls.set(call.id,call); }
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
    command.state=body.state;command.reason=body.reason;command.artifact_ids=body.attachment_ids||[];send(200,{...command,execute:false});return;
  }
  const callPath = url.pathname.match(/^\/api\/v1\/companion\/calls\/([^/]+)(?:\/(heartbeat|close))?$/);
  if (callPath) {
    const call = calls.get(callPath[1]); if (!call) { send(404,{error:'missing call'}); return; }
    if (controlFails) { send(503,{error:'fixture control outage'}); return; }
    if (callPath[2] === 'close') {
      if (call.state !== 'closed') { call.state='closing'; call.closeChecks=2; }
      if (holdNextClose) { holdNextClose=false; heldClose=(status=503)=>send(status,status===200?{id:call.id,state:call.state}:{error:'fixture delayed close failure'}); return; }
      send(200,{id:call.id,state:call.state}); return;
    }
    if (callPath[2] === 'heartbeat') { send(heartbeatFails?503:200,heartbeatFails?{error:'fixture heartbeat outage'}:{}); return; }
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
  const fixture = window.fixture = {media:[],tracks:[],peers:[],channels:[],level:0,micLevel:0,wakeLocks:[],audioContexts:[],delayMic:false,denyCamera:false,delayCamera:false,draws:0,shutters:0,enumerations:0,wakeRequests:0,delayWake:false,denyWake:false,wakeGrants:[],cameras:[{kind:'videoinput',deviceId:'rear-camera',label:'Back camera'},{kind:'videoinput',deviceId:'front-camera',label:'Front camera'}]};
  const actualNow=Date.now;fixture.clockOffset=0;Date.now=()=>actualNow()+fixture.clockOffset;
  const actualInterval=window.setInterval;window.setInterval=(fn,ms,...args)=>{if(ms===500)fixture.controlTick=fn;return actualInterval(fn,ms,...args);};
  const makeStream = (kind,deviceId) => {
    const track = new EventTarget(); track.kind=kind; track.enabled=true; track.stopped=false; track.readyState='live';track.stop=()=>{track.stopped=true;track.readyState='ended';}; fixture.tracks.push(track);
    track.getSettings=()=>({deviceId:deviceId||'rear-camera'});
    const stream = new MediaStream(); Object.defineProperties(stream,{getTracks:{value:()=>[track]},getAudioTracks:{value:()=>kind==='audio'?[track]:[]},getVideoTracks:{value:()=>kind==='video'?[track]:[]}}); return stream;
  };
  const mediaDevices=Object.assign(new EventTarget(),{getUserMedia:async constraints=>{
    fixture.media.push(constraints);
    if (constraints.video && fixture.denyCamera) throw new DOMException('denied','NotAllowedError');
    if (constraints.video && fixture.delayCamera) return new Promise(resolve=>{fixture.resolveCamera=()=>resolve(makeStream('video',constraints.video.deviceId?.exact));});
    if (constraints.audio && fixture.delayMic) return new Promise(resolve=>{fixture.resolveMic=()=>resolve(makeStream('audio'));});
    return makeStream(constraints.video?'video':'audio',constraints.video?.deviceId?.exact);
  },enumerateDevices:async()=>{fixture.enumerations++;return fixture.cameras;}});
  Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:mediaDevices});
  Object.defineProperty(navigator,'wakeLock',{configurable:true,value:{request:async()=>{
    fixture.wakeRequests++;
    if(fixture.denyWake) throw new DOMException('denied','NotAllowedError');
    const grant=()=>{const lock=new EventTarget();lock.released=false;lock.release=async()=>{lock.released=true;lock.dispatchEvent(new Event('release'));};fixture.wakeLocks.push(lock);return lock;};
    if(fixture.delayWake)return new Promise(resolve=>fixture.wakeGrants.push(()=>resolve(grant())));
    return grant();
  }}});
  class FakePeer extends EventTarget {
    constructor(){super();this.connectionState='new';this.iceConnectionState='new';this.iceGatheringState='complete';this.order=[];this.closed=false;fixture.peers.push(this);}
    addTrack(){this.order.push('track');}
    createDataChannel(label){this.order.push('channel'); const channel=new EventTarget();channel.label=label;channel.readyState='open';channel.sent=[];channel.send=value=>channel.sent.push(value);channel.close=()=>{channel.closed=true;channel.readyState='closed';};fixture.channels.push(channel);return channel;}
    async createOffer(){this.order.push('offer');return {type:'offer',sdp:'fixture-offer-' + fixture.peers.length};}
    async setLocalDescription(offer){this.localDescription=offer;}
    async setRemoteDescription(answer){this.remoteDescription=answer;this.connectionState='connected';this.iceConnectionState='connected';this.dispatchEvent(new Event('connectionstatechange'));const event=new Event('track');event.streams=[new MediaStream()];this.dispatchEvent(event);}
    close(){this.closed=true;this.connectionState='closed';}
  }
  window.RTCPeerConnection=FakePeer;
  window.AudioContext=class {
    constructor(){this.closed=false;this.state='running';this.sampleRate=48000;this.destination={};fixture.audioContexts.push(this);}
    async resume(){this.state='running';} async suspend(){this.state='suspended';} async close(){this.closed=true;this.state='closed';}
    createMediaStreamSource(stream){return {connect(node){node.fixtureKind=stream.getAudioTracks().length?'mic':'output';},disconnect(){}};}
    createAnalyser(){return {fftSize:256,getByteTimeDomainData(values){values.fill(128+(this.fixtureKind==='mic'?fixture.micLevel:fixture.level));}};}
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
const emit = (kind,data,at=Date.now()) => events.push({seq:(events.at(-1)?.seq || 0)+1,kind,data,at});
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
  assert.equal(await page.evaluate(()=>fixture.media.length),0); assert.equal(await page.evaluate(()=>fixture.enumerations),0); checks.push('login and contact rendering never acquire media or enumerate cameras; untrusted text stays text');
  if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-contacts.png'),fullPage:true});

  await page.getByRole('button',{name:'Voice settings for Coach',exact:true}).click();
  await page.locator('#voice-choice:not([disabled])').waitFor();
  assert.match(await page.locator('#voice-model-facts').innerText(),/OpenAI · fixture-live/);assert.match(await page.locator('#voice-model-facts').innerText(),/Fixture reasoning runtime/);assert.match(await page.locator('#voice-model-facts').innerText(),/Fixture reasoning model/);
  await page.locator('#voice-advanced summary').click();assert.match(await page.locator('#voice-advanced-facts').innerText(),/Thinking.*Medium/s);assert.equal(await page.locator('#voice-advanced input,#voice-advanced select').count(),0);
  await page.locator('#voice-choice').selectOption('ash'); await page.locator('#save-voice').click();
  await wait(async()=>/next call/.test(await page.locator('#voice-settings-status').innerText()),'voice save missing');
  assert.equal(voices.get('coach'),'ash'); assert.equal(requests.find(request=>request.method==='PUT').csrf,csrf);
  assert.equal(await page.evaluate(()=>fixture.media.length),0); await page.locator('#close-voice-settings').click();
  checks.push('discoverable per-agent voice choice persists through authenticated API and applies to the next call');

  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','call did not connect');
  assert.deepEqual(await page.evaluate(()=>fixture.peers[0].order),['track','channel','offer']);
  assert.equal(await page.evaluate(()=>fixture.channels[0].label),'oai-events');
  assert.equal(await page.evaluate(()=>fixture.channels[0].sent.length),0);
  await page.locator('#call-voice-settings').click();await page.locator('#voice-choice:not([disabled])').waitFor();
  assert.match(await page.locator('#voice-model-facts').innerText(),/Voice in this call.*ash/s);
  await page.locator('#voice-choice').selectOption('cedar');await page.locator('#save-voice').click();
  await wait(async()=>/cedar will be used/.test(await page.locator('#voice-settings-status').innerText()),'next-call voice setting not saved');
  assert.match(await page.locator('#voice-model-facts').innerText(),/Voice in this call.*ash/s);await page.locator('#close-voice-settings').click();
  assert.equal(await page.evaluate(()=>fixture.media.length),1);assert.equal(calls.size,1);
  checks.push('voice settings show reported provider/runtime/model facts and real read-only advanced values; saving the next voice preserves the current call and its voice snapshot');
  const firstRequest=requests.find(request=>request.path==='/api/v1/companion/calls');assert.equal(firstRequest.csrf,csrf);assert.equal(firstRequest.body.agent_id,'coach');
  assert.match(firstRequest.body.request_id,/^[\da-f-]{36}$/); checks.push('explicit Call creates one CSRF-protected SDP session; data channel is receive-only');

  await wait(async()=>page.evaluate(()=>fixture.wakeLocks.some(lock=>!lock.released)),'foreground call did not acquire wake lock');
  assert(await page.locator('#screen-awake').isChecked());
  await page.locator('#screen-awake').uncheck();
  assert(await page.evaluate(()=>fixture.wakeLocks.every(lock=>lock.released)));
  await page.evaluate(()=>{fixture.delayWake=true;});
  await page.locator('#screen-awake').check();
  await wait(async()=>page.evaluate(()=>fixture.wakeGrants.length===1),'delayed screen lock request missing');
  const pendingWakeRequests=await page.evaluate(()=>fixture.wakeRequests);
  await page.evaluate(()=>{window.dispatchEvent(new Event('focus'));window.dispatchEvent(new Event('online'));});
  await page.waitForTimeout(100);assert.equal(await page.evaluate(()=>fixture.wakeRequests),pendingWakeRequests,'concurrent focus/resume must not duplicate a pending screen lock');
  await page.locator('#screen-awake').uncheck();
  await page.evaluate(()=>fixture.wakeGrants.shift()());
  await wait(async()=>page.evaluate(()=>fixture.wakeLocks.every(lock=>lock.released)),'late screen lock survived opt-out');
  await page.locator('#screen-awake').check();
  await wait(async()=>page.evaluate(()=>fixture.wakeGrants.length===1),'second delayed screen lock missing');
  await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,value:true});document.dispatchEvent(new Event('visibilitychange'));fixture.wakeGrants.shift()();});
  await wait(async()=>page.evaluate(()=>fixture.wakeLocks.every(lock=>lock.released)),'hidden page retained late screen lock');
  await page.evaluate(()=>{fixture.delayWake=false;Object.defineProperty(document,'hidden',{configurable:true,value:false});document.dispatchEvent(new Event('visibilitychange'));});
  await wait(async()=>page.evaluate(()=>fixture.wakeLocks.some(lock=>!lock.released)),'visible call did not regain screen lock');
  await page.locator('#screen-awake').uncheck();await page.evaluate(()=>{fixture.denyWake=true;});await page.locator('#screen-awake').check();
  await wait(async()=>/screen may dim/i.test(await page.locator('#screen-awake-status').innerText()),'denied screen lock was not described honestly');
  assert.equal(await page.locator('#screen-awake-status').evaluate(node=>node.classList.contains('sr-only')),false,'wake-lock denial must be visible to sighted phone users');
  assert.equal(await page.locator('#call-status-text').innerText(),'Connected');
  await page.evaluate(()=>{fixture.denyWake=false;});await page.locator('#screen-awake').uncheck();await page.locator('#screen-awake').check();
  checks.push('screen-on preference is optional; concurrent acquisition, late opt-out/hidden grants, denial and reacquisition preserve the same voice call');

  emit('transcript',{role:'assistant',text:'Generated speech is not playback.',fragment:true,playback:'unconfirmed',start_ms:0,end_ms:500});
  emit('provider',{type:'session.usage.updated',usage:{seconds:1}});
  emit('transcript',{role:'assistant',text:' A little more context.',fragment:true,playback:'unconfirmed',start_ms:500,end_ms:1000});
  emit('delegation',{id:'d1',state:'dispatched'});
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Working together','delegated work not visible');
  assert.match(await page.locator('.context-entry').textContent(),/Saved for agent/);assert.match(await page.locator('.context-entry').textContent(),/does not confirm forwarding/);
  assert.equal(await page.locator('.transcript-entry').count(),1);assert.match(await page.locator('.transcript-entry').innerText(),/playback\. A little/);
  assert.equal(await page.locator('.presence.speaking').count(),0);
  await page.evaluate(()=>fixture.channels[0].dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'session.output_transcript.delta',delta:'DUPLICATE'})})));
  assert.doesNotMatch(await page.locator('#transcript').innerText(),/DUPLICATE/);
  await page.evaluate(()=>{fixture.level=20;}); await page.locator('.presence.speaking').waitFor();
  await page.evaluate(()=>{fixture.level=0;}); await wait(async()=>await page.locator('.presence.speaking').count()===0,'speaker meter did not settle');
  // Browser media routing, not generated captions, owns suppression. The
  // microphone and output energies are independent fixtures in this test.
  await page.evaluate(()=>{fixture.level=20;fixture.micLevel=0;});
  await page.waitForTimeout(150);
  await page.evaluate(()=>{fixture.micLevel=20;}); await page.waitForTimeout(250);
  await page.evaluate(()=>{fixture.micLevel=0;});
  assert.equal(await page.locator('#remote-audio').evaluate(node=>node.muted),false,'brief listener response must preserve duplex');
  await page.waitForTimeout(350);await page.evaluate(()=>{fixture.micLevel=20;});
  await wait(async()=>page.locator('#remote-audio').evaluate(node=>node.muted),'sustained human speech did not suppress playback');
  const floorReceipt=()=>[...surfaces.values()].find(value=>value.playback?.state==='suppressed');
  await wait(async()=>Boolean(floorReceipt()?.interruption),'interruption was not reported');
  const interruptionId=floorReceipt().interruption.id;
  assert.match(interruptionId,/^[0-9a-f-]{36}$/);assert.equal(floorReceipt().interruption.phase,'suppressed');
  assert.equal(await page.locator('#remote-audio').evaluate(node=>node.paused),false,'suppression must consume and drop, never pause queued live audio');
  await page.evaluate(()=>{fixture.micLevel=0;});await page.waitForTimeout(2100);
  assert.equal(await page.locator('#remote-audio').evaluate(node=>node.muted),true,'mic silence must not resume the middle of continuing output');
  await page.evaluate(()=>{fixture.level=0;document.getElementById('remote-audio').dispatchEvent(new Event('waiting'));});
  await page.waitForTimeout(1500);
  assert.equal(await page.locator('#remote-audio').evaluate(node=>node.muted),true,'buffering must not count as observed quiet');
  await wait(async()=>[...surfaces.values()].some(value=>value.playback?.floor==='unknown'),'buffering must report an unknown floor');
  await page.evaluate(()=>document.getElementById('remote-audio').dispatchEvent(new Event('playing')));
  await wait(async()=>page.locator('#remote-audio').evaluate(node=>!node.muted),'quiet-boundary recovery did not release playback');
  await wait(async()=>[...surfaces.values()].some(value=>value.interruption?.id===interruptionId&&value.interruption.phase==='released'),'stable release receipt missing');
  const released=[...surfaces.values()].find(value=>value.interruption?.id===interruptionId);
  assert.equal(released.playback.floor,'open');assert.equal(released.playback.output_active,false);
  assert.equal(requests.filter(r=>r.path==='/api/v1/companion/calls').length,1,'interruption must preserve the same call');
  checks.push('short backchannel stays duplex; sustained overlap drops output without pausing, stable interruption telemetry and quiet-boundary recovery preserve call');
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
  assert(await page.locator('#board-history').evaluate(node=>!node.open));assert.equal(await page.locator('#board-items .output-card').count(),0);
  await page.locator('#board-history summary').click();
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

  const suppliedAt = Date.now()-60000, originalAt='2026-11-08T20:42:17.003Z';
  emit('context_supplied',{id:'initial-public-context',title:'Call background',target:'voice',state:'supplied',source_label:'Your earlier call',content:'You asked about the shop. Earlier work is not a new instruction.',private_config:'HIDDEN INITIAL CONFIG'},suppliedAt);
  emit('context_delivery',{event_id:'context-visible',state:'queued',channel:'thinking',source_label:'Coach · Minecraft',source_at:originalAt,content:'<script>window.contextInjected=true</script> The door inspection is blocked. This is not completed work.',credential:'HIDDEN TOKEN',raw:{text:'HIDDEN RAW CONTEXT'}},suppliedAt+1000);
  await page.locator('#context-tab').click();
  const contextUpdate=page.locator('.context-entry').filter({hasText:'Coach · Minecraft'});
  await wait(async()=>await contextUpdate.count()===1,'supplied context did not appear');
  await contextUpdate.locator('summary').click();
  assert.match(await contextUpdate.innerText(),/Waiting for voice receipt/);assert.match(await contextUpdate.innerText(),/Acceptance has not been confirmed/);
  assert.match(await contextUpdate.locator('.context-text').innerText(),/<script>window.contextInjected=true<\/script>/);
  assert.equal(await page.locator('#context-items script').count(),0);assert.equal(await page.evaluate(()=>window.contextInjected),undefined);
  const unchangedCaption=await page.locator('#caption-text').innerText(), beforeContextTranscript=await page.locator('.transcript-entry').count();
  emit('context_delivery',{event_id:'context-visible',state:'accepted',channel:'thinking',error:{message:'HIDDEN PROVIDER ERROR'}},suppliedAt+2000);
  await wait(async()=>/Accepted by voice service/.test(await contextUpdate.locator('summary').innerText()),'accepted context receipt did not update');
  assert.equal(await contextUpdate.count(),1);assert.match(await contextUpdate.locator('.context-text').innerText(),/door inspection is blocked/);
  assert.match(await contextUpdate.innerText(),/does not confirm it was used or spoken/);
  assert.equal(await contextUpdate.locator('time').getAttribute('datetime'),new Date(suppliedAt+2000).toISOString());
  assert.match(await contextUpdate.locator('.context-source').innerText(),/Source time:.*First recorded:.*Receipt updated:/);
  assert.equal(await page.locator('#caption-text').innerText(),unchangedCaption);assert.equal(await page.locator('.transcript-entry').count(),beforeContextTranscript);
  assert.doesNotMatch(await page.locator('#context-panel').textContent(),/HIDDEN TOKEN|HIDDEN RAW CONTEXT|HIDDEN PROVIDER ERROR|HIDDEN INITIAL CONFIG/);
  assert.match(await page.locator('#context-panel').textContent(),/not a complete view of the model’s current memory/);
  assert.match(await page.locator('.context-entry').filter({hasText:'Request to Coach'}).textContent(),/Agent response saved/);
  emit('context_delivery',{event_id:'old-context',state:'accepted'},null);
  await wait(async()=>await page.locator('.context-entry').filter({hasText:'Time not recorded'}).count()===1,'missing context time was fabricated');
  const oldContext=page.locator('.context-entry').filter({hasText:'Time not recorded'});
  assert.match(await oldContext.textContent(),/text was not retained/);
  await page.locator('#context-tab').focus();await page.keyboard.press('ArrowRight');assert.equal(await page.locator('#space-tab').getAttribute('aria-selected'),'true');
  await page.keyboard.press('End');assert.equal(await page.locator('#context-tab').getAttribute('aria-selected'),'true');
  await page.keyboard.press('ArrowLeft');assert.equal(await page.locator('#transcript-tab').getAttribute('aria-selected'),'true');
  await page.keyboard.press('Home');assert.equal(await page.locator('#space-tab').getAttribute('aria-selected'),'true');
  checks.push('Context shows exact safely rendered supplied text, source time and correlated delivery receipts without exposing raw configuration or claiming speech, comprehension, or task completion; all three tabs support keyboard navigation');
  for(const [state,expected] of [['admitted',/connector admitted.*forwarding to the agent is not confirmed/],['settled',/conversation turn ended; a reply and work outcome are not confirmed/],['interrupted',/handling was interrupted; the outcome is unconfirmed/]]) {
    emit('action_observation',{label:'Minecraft',observation:{request_id:'game-request-1',scope:'conversation',state,verified:false,fulfillment:'not_established',conversation:{host_forwarding_verified:false,reply_verified:false,task_acceptance_verified:false},observed_at:originalAt}});
    await wait(async()=>expected.test(await page.locator('#connection-status').innerText()),`conversation ${state} was overstated`);
  }
  assert.equal(await page.locator('.context-entry').filter({hasText:'Connection report'}).count(),1);
  assert.doesNotMatch(await page.locator('#connection-status').innerText(),/message was delivered|accepted your request|working on your request/);
  emit('action_observation',{label:'Minecraft',observation:{request_id:'game-request-1',scope:'conversation',state:'settled',verified:false,fulfillment:'not_established',provenance:{kind:'minecraft_input_outcome',version:1},response_association:'same_turn_source_set_not_individual_fulfillment',conversation:{blocked_reason:'The requested area is unavailable.'},responses:[{id:'public-reply',at:originalAt,audience:'public',text:'I checked the path. <img src=x onerror="window.outcomeInjected=true">'},{id:'private-reply',at:originalAt,audience:'private',text:'PRIVATE RESPONSE MUST NOT APPEAR'}],private_responses_withheld:1}});
  await wait(async()=>/A blocker was reported; details are in Context/.test(await page.locator('#connection-status').innerText()),'source-bound blocker missing');
  const outcome=page.locator('.context-entry').filter({hasText:'Public replies associated with the same turn'});
  assert.match(await outcome.textContent(),/Reported blocker: The requested area is unavailable/);
  assert.match(await outcome.textContent(),/I checked the path/);assert.match(await outcome.textContent(),/do not establish that this request was fulfilled/);
  assert.match(await outcome.textContent(),/Private response text was withheld/);
  assert.doesNotMatch(await page.locator('body').textContent(),/PRIVATE RESPONSE MUST NOT APPEAR/);assert.equal(await page.evaluate(()=>window.outcomeInjected),undefined);
  assert.equal(await page.locator('#caption-text').innerText(),unchangedCaption);
  checks.push('durable game outcomes show the actual blocker and public same-turn replies as safe Context text; no private content, spoken claim or per-request fulfillment is invented');
  const domainOutcomeBase={scope:'conversation',verified:false,fulfillment:'not_established',provenance:{kind:'minecraft_input_outcome',version:1},response_association:'same_turn_source_set_not_individual_fulfillment',observed_at:originalAt,private_responses_withheld:0,responses_omitted:0};
  emit('action_observation',{label:'Minecraft retained request',observation:{...domainOutcomeBase,request_id:'game-retained-request',state:'settled',conversation:{conversation_only:true,blocked_reason:null},responses:[{id:'retained-public-reply',at:originalAt,audience:'public',text:'I inspected the entrance; the path is clear.'}],domain_private_responses_withheld:true,domain_responses_omitted:true}});
  emit('action_observation',{label:'Minecraft unretained request',observation:{...domainOutcomeBase,request_id:'game-unretained-request',state:'unknown',outcome_reason:'not_retained',conversation:{conversation_only:null,blocked_reason:null},responses:[],domain_private_responses_withheld:false,domain_responses_omitted:false}});
  const retainedOutcome=page.locator('.context-entry').filter({hasText:'Minecraft retained request'});
  const unknownOutcome=page.locator('.context-entry').filter({hasText:'Minecraft unretained request'});
  await wait(async()=>await retainedOutcome.count()===1&&await unknownOutcome.count()===1,'mixed game outcomes did not stay separate');
  assert.match(await retainedOutcome.textContent(),/The game turn ended/);
  assert.match(await retainedOutcome.textContent(),/I inspected the entrance; the path is clear/);
  assert.match(await retainedOutcome.textContent(),/Private response text was withheld from this report/);
  assert.match(await retainedOutcome.textContent(),/Some replies are not included in this report/);
  assert.match(await retainedOutcome.textContent(),/do not establish that this request was fulfilled/);
  assert.doesNotMatch(await retainedOutcome.textContent(),/1 private|1 public|saved elsewhere|retained elsewhere|available elsewhere/);
  assert.match(await unknownOutcome.textContent(),/A retained handling record is not available for this request/);
  assert.match(await unknownOutcome.textContent(),/result of this request is not confirmed/);
  assert.doesNotMatch(await unknownOutcome.textContent(),/The game turn ended|I inspected the entrance|Private response text was withheld|Some replies are not included|task accepted|completed successfully/);
  assert.equal(await page.locator('#caption-text').innerText(),unchangedCaption);
  checks.push('mixed retained and unknown game outcomes remain separate; nullable turn scope and domain omission flags never invent counts, retrievability, private text or individual fulfillment');
  emit('action_observation',{label:'Slack',observation:{request_id:'slack-request-1',scope:'message_delivery',state:'completed',verified:false}});
  await wait(async()=>/Message delivery is unconfirmed/.test(await page.locator('#connection-status').innerText()),'unverified delivery was presented as confirmed');
  emit('action_observation',{label:'Slack',observation:{request_id:'slack-request-1',scope:'message_delivery',state:'completed',verified:true}});
  await wait(async()=>/message was delivered.*Further work is not verified/.test(await page.locator('#connection-status').innerText()),'confirmed delivery scope was lost');
  assert.equal(await page.locator('#caption-text').innerText(),unchangedCaption);assert.equal(await page.locator('.transcript-entry').count(),beforeContextTranscript);
  checks.push('local conversation admission, turn settlement and interruption never imply model forwarding, a reply, resumed work or message delivery; verified message delivery remains separate and quiet');

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
  const fullCard=page.locator('.output-card').filter({has:page.getByRole('heading',{name:/Complete answer/})});
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
  assert.equal(await page.locator('.output-card').filter({has:page.getByRole('heading',{name:/Invalid result URL/})}).locator('.full-result-button').count(),0);
  const explicitVideo=page.locator('.output-card').filter({has:page.getByRole('heading',{name:/Video demonstration/})});
  assert.match(await explicitVideo.locator('h2').innerText(),/Video demonstration/);
  assert.equal(await explicitVideo.locator('video').count(),0);
  await explicitVideo.getByRole('button',{name:'Load video preview',exact:true}).click();
  assert.equal(await explicitVideo.locator('video').getAttribute('src'),'https://media.example/watch?id=demo');
  assert.equal(await explicitVideo.locator('video').getAttribute('autoplay'),null);
  assert.equal(await page.locator('.output-card').filter({has:page.getByRole('heading',{name:/Reference only/})}).locator('.media-preview-button,video,iframe').count(),0);
  checks.push('full saved results load only from this call’s authenticated route and remain safe text; explicit extensionless video previews need a click and link-only cards stay links');


  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),false);assert.equal(await page.locator('#mute').getAttribute('aria-pressed'),'true');
  await page.locator('#mute').click();assert.equal(await page.evaluate(()=>fixture.tracks[0].enabled),true);
  assert.equal(await page.locator('#camera-label').innerText(),'Show something');
  await page.locator('#camera-toggle').click();await page.locator('#camera-panel:not([hidden])').waitFor();
  assert.equal(await page.locator('#camera-panel h2').innerText(),'Show and help');
  assert.match(await page.locator('#camera-guide').innerText(),/Point at what you want help with.*Coach/);
  assert.match(await page.locator('#camera-note').innerText(),/live camera feed is never shared/);
  await wait(async()=>await page.locator('#camera-choice option').count()===2,'granted cameras were not listed');
  assert.equal(await page.locator('#camera-choice').inputValue(),'rear-camera');
  assert.deepEqual(await page.locator('#camera-choice option').allTextContents(),['Back camera','Front camera']);
  await page.locator('#voice-photos').check();
  assert.match(await page.locator('#voice-photo-hint').innerText(),/Take a photo and help me with this/);
  {
    await page.setViewportSize({width:390,height:844});
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth&&document.documentElement.scrollHeight<=innerHeight));
    const capture=await page.locator('#capture-photo').boundingBox();
    assert(capture.y>=0&&capture.y+capture.height<=844,'capture control must be visible when camera opens');
    await page.locator('#voice-photos').scrollIntoViewIfNeeded();
    const consent=await page.locator('#voice-photos').boundingBox();
    assert(consent.y>=0&&consent.y+consent.height<=844,'one-photo consent must be reachable inside shared panel');
    assert.equal(await page.evaluate(()=>fixture.tracks[0].stopped||fixture.peers[0].closed),false);
    await page.locator('#space-panel').evaluate(node=>{node.scrollTop=0;});
    if (process.env.COMPANION_SCREENSHOT) await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-show-help-mobile.png'),fullPage:true});
    await page.setViewportSize({width:1280,height:900});
  }
  const oldCamera=await page.evaluate(()=>fixture.tracks.findIndex(t=>t.kind==='video'&&!t.stopped));
  await page.locator('#camera-choice').selectOption('front-camera');
  await wait(async()=>page.evaluate(()=>fixture.tracks.at(-1).getSettings().deviceId==='front-camera'&&!fixture.tracks.at(-1).stopped),'selected camera did not open');
  assert(await page.evaluate(index=>fixture.tracks[index].stopped,oldCamera));
  assert.equal(await page.evaluate(()=>fixture.media.at(-1).audio),false);
  assert.deepEqual(await page.evaluate(()=>fixture.media.at(-1).video.deviceId),{exact:'front-camera'});
  assert.equal(await page.evaluate(()=>fixture.tracks[0].stopped||fixture.peers[0].closed),false);
  const cameraRequests=await page.evaluate(()=>fixture.media.length);
  await page.evaluate(()=>{fixture.cameras=fixture.cameras.slice(1);navigator.mediaDevices.dispatchEvent(new Event('devicechange'));});
  await page.locator('#camera-picker').waitFor({state:'hidden'});
  assert.equal(await page.evaluate(()=>fixture.media.length),cameraRequests);
  await page.evaluate(()=>{fixture.cameras.unshift({kind:'videoinput',deviceId:'rear-camera',label:'Back camera'});navigator.mediaDevices.dispatchEvent(new Event('devicechange'));});
  await page.locator('#camera-picker').waitFor({state:'visible'});
  checks.push('camera choice lists only browser-granted video devices, switches exact cameras without touching microphone or voice, and device changes never acquire media');
  await page.evaluate(()=>{fixture.delayCamera=true;});
  await page.locator('#camera-choice').selectOption('rear-camera');
  await wait(async()=>page.evaluate(()=>Boolean(fixture.resolveCamera)),'camera switch was not pending');
  await page.locator('#camera-toggle').click();
  assert.equal(await page.locator('#voice-photos').isChecked(),false);
  await page.evaluate(()=>{fixture.delayCamera=false;fixture.resolveCamera();});await page.waitForTimeout(100);
  assert(await page.evaluate(()=>fixture.tracks.at(-1).stopped));assert(await page.locator('#camera-panel').isHidden());
  assert.equal(await page.evaluate(()=>fixture.tracks[0].stopped||fixture.peers[0].closed),false);
  await page.locator('#camera-toggle').click();await page.locator('#camera-panel:not([hidden])').waitFor();
  checks.push('Camera off revokes voice-photo consent and releases a late camera-switch result without interrupting voice');
  assert.equal(requests.filter(request=>/\/attachments$/.test(request.path)).length,0);
  let lostNotification=false;
  await page.route('**/api/v1/companion/calls/*/uploads',async route=>{
    if (!lostNotification && route.request().postDataJSON().state === 'available') { lostNotification=true; await route.fetch(); await route.abort('failed'); }
    else await route.continue();
  });
  holdNextUpload=true;await page.locator('#capture-photo').click();
  await wait(async()=>heldUpload!==null,'capture did not start upload');
  assert.equal(await page.locator('.upload-card').first().getAttribute('data-state'),'uploading');
  assert.match(await page.locator('#camera-progress').innerText(),/Sharing this photo/);
  heldUpload();heldUpload=null;
  const photo = page.locator('.upload-card').first();
  await wait(async()=>/notification is unconfirmed/.test(await photo.innerText()),'lost availability receipt not surfaced');
  assert.equal(await page.locator('#photo-question').count(),0);
  assert.equal(await photo.getAttribute('data-state'),'available');
  await photo.getByRole('button',{name:'Check voice notification'}).click();
  await wait(async()=>/notification queued/.test(await photo.innerText()),'same availability was not acknowledged');
  assert.equal(attachments.size,1); assert.equal(messages.size,0);
  assert.match(await page.locator('#camera-progress').innerText(),/Photo saved.*Inspection is not yet confirmed/);
  assert.deepEqual([...uploads.values()].map(upload=>upload.state),['captured','uploading','available']);
  const sharedPhoto = [...uploads.values()].find(upload=>upload.state==='available');
  assert.equal(sharedPhoto.source,'camera'); assert.deepEqual(sharedPhoto.attachment_ids,['fixture-attachment-1']);
  const notices=requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available');
  assert.deepEqual(notices[0].body,notices[1].body); assert(notices.every(request=>request.csrf===csrf));
  assert.equal([...uploads.values()].filter(upload=>upload.state==='available').length,1);
  checks.push('Show-and-help guides one consented photo with honest saved/inspection status; Capture immediately uploads without a question; same-ID voice notice recovers a lost acknowledgment without duplicate storage or forced summary');
  await page.locator('#camera-toggle').click();

  await context.grantPermissions(['clipboard-read','clipboard-write']);
  await page.evaluate(()=>navigator.clipboard.writeText('Notes pasted while talking.'));
  await page.getByRole('button',{name:'Paste text or an image',exact:true}).click();await page.keyboard.press('Control+V');
  await wait(async()=>await page.locator('#paste-input').inputValue()==='Notes pasted while talking.','actual focused paste did not reach the composer');
  assert.equal(attachments.size,1,'a text draft is shared only by its explicit Share button');
  await page.locator('#share-text').click();
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
  assert(await page.locator('#share-receipt').isVisible());assert.match(await page.locator('#share-receipt-text').innerText(),/pasted.png/);
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
  assert(await page.locator('.upload-card[data-state="unconfirmed"]').getByRole('button',{name:'Check voice notification',exact:true}).isHidden());
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
  const contextView=surfaceCommand('view',{action:'show_context',card_id:null});await wait(async()=>contextView.state==='executed','context command missing');assert.equal(await page.locator('#context-tab').getAttribute('aria-selected'),'true');
  const shared=surfaceCommand('view',{action:'show_shared',card_id:null});await wait(async()=>shared.state==='executed','shared pane command missing');
  const scroll=surfaceCommand('view',{action:'scroll_down',card_id:null});await wait(async()=>scroll.state==='executed','bounded scroll command missing');
  assert.equal(await page.evaluate(()=>scrollY),0);const afterView=await page.locator('#hangup').boundingBox();assert(Math.abs(desktopAfter.y-afterView.y)<1);
  assert.equal(await page.evaluate(()=>fixture.media.length),beforeViewSensors);assert.equal(await page.evaluate(()=>fixture.draws),beforeViewFrames);
  checks.push('agent view controls focus owned cards and switch or scroll only the contained pane without moving hangup or opening media');
  const savedIds=await page.locator('#cards .output-card[data-card-id]').evaluateAll(nodes=>nodes.map(node=>node.dataset.cardId).filter(id=>/^\d+$/.test(id)).slice(0,6));
  await page.locator('#board-history summary').click();
  const replaceBoard=surfaceCommand('view',{action:'replace_board',card_ids:savedIds.slice(0,3)});
  await wait(async()=>replaceBoard.state==='executed','replace_board did not select saved results');
  assert.equal(await page.locator('#board-items>.output-card').count(),3);
  assert.deepEqual(await page.locator('#board-items>.output-card').evaluateAll(nodes=>nodes.map(node=>node.dataset.cardId)),savedIds.slice(0,3));
  const fullBoard=surfaceCommand('view',{action:'add_card',card_id:savedIds[3]});
  await wait(async()=>fullBoard.state==='failed','full board command did not return a failed receipt');assert.equal(fullBoard.reason,'board_full');
  const pinned=page.locator(`#board-items>[data-card-id="${savedIds[0]}"]`);await pinned.getByRole('button',{name:'Pin',exact:true}).click();
  const protectedBoard=surfaceCommand('view',{action:'clear_board'});
  await wait(async()=>protectedBoard.state==='failed','clear removed a pinned item');assert.equal(protectedBoard.reason,'item_protected');
  assert.equal(await page.locator('#board-items>.output-card').count(),3);
  emit('card',{text:'Saved screenshot while the board is full.',presentation:'current',attachments:[{id:'full-board-image',name:'Full board screenshot',mime_type:'image/png'}],conversation_id:'room-coach'});
  await wait(async()=>await page.locator('#cards .output-card').filter({hasText:'Saved screenshot while the board is full.'}).count()===1,'requested image must remain available in history when the board is protected/full');
  assert.deepEqual(await page.locator('#board-items>.output-card').evaluateAll(nodes=>nodes.map(node=>node.dataset.cardId)),savedIds.slice(0,3));
  assert.equal(await pinned.getByRole('button',{name:'Unpin',exact:true}).count(),1);
  await wait(async()=>surfaces.get('call-1')?.pinned_card_ids?.includes(savedIds[0]),'pin was not reported to the agent surface');
  await pinned.getByRole('button',{name:'Unpin',exact:true}).click();await page.locator('#space-tab').click();
  await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
  await wait(async()=>surfaces.get('call-1')?.focused_card_id===null,'reported reading focus stayed stale after the user returned to the end');
  const removeBoard=surfaceCommand('view',{action:'remove_card',card_id:savedIds[1]});await wait(async()=>removeBoard.state==='executed','remove did not put away a saved item');
  assert.equal(await page.locator('#cards>[data-card-id="'+savedIds[1]+'"]').count(),1,'removed presentation must remain in history');
  const addBoard=surfaceCommand('view',{action:'add_card',card_id:savedIds[3]});await wait(async()=>addBoard.state==='executed','add did not use freed board capacity');
  const beforeFlood=await page.locator('#board-items>.output-card').evaluateAll(nodes=>nodes.map(node=>node.dataset.cardId));
  for(let i=0;i<8;i++)emit('card',{title:'Background update '+i,text:'Saved update only.',presentation:'later',observed_only:true});
  await wait(async()=>await page.locator('#cards .output-card').filter({hasText:'Background update 7'}).count()===1,'background updates were not saved');
  assert.deepEqual(await page.locator('#board-items>.output-card').evaluateAll(nodes=>nodes.map(node=>node.dataset.cardId)),beforeFlood);
  assert(await page.locator('#board-history').evaluate(node=>!node.open));
  checks.push('whiteboard presents at most three intentional items; full/protected commands fail atomically, pins are observable, removal keeps history, and automatic observations never flood current content');

  const readingCard=page.locator(`#board-items>[data-card-id="${savedIds[0]}"]`);
  await readingCard.locator('.card-answer').click();await page.locator('#space-panel').evaluate(node=>{node.scrollTop=100;});
  const readingTop=await page.locator('#space-panel').evaluate(node=>node.scrollTop);
  const protectedReading=surfaceCommand('view',{action:'replace_board',card_ids:[savedIds[4]]});
  await wait(async()=>protectedReading.state==='failed','agent replaced an item being read');assert.equal(protectedReading.reason,'item_protected');
  emit('card',{title:'While reading',text:'Saved quietly.',presentation:'later'});
  await wait(async()=>await page.locator('#cards .output-card').filter({hasText:'While reading'}).count()===1,'reading fixture update missing');
  assert(Math.abs(await page.locator('#space-panel').evaluate(node=>node.scrollTop)-readingTop)<2,'a background update moved the reading position');
  await page.locator('#space-tab').click();await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
  const clearBoard=surfaceCommand('view',{action:'clear_board'});await wait(async()=>clearBoard.state==='executed','unprotected board did not clear');
  assert.equal(await page.locator('#board-items>.output-card').count(),0);assert(await page.locator('#space-empty').isVisible());
  emit('card',{text:'Here is the saved Minecraft view, not a live feed.',presentation:'current',late:false,attachments:[{id:'current-output-image',name:'Saved Minecraft view',mime_type:'image/png'}],conversation_id:'room-coach'});
  const shownImage=page.locator('#board-items img.shared-media').filter({visible:true});
  await wait(async()=>shownImage.count().then(async count=>count===1&&shownImage.evaluate(img=>img.complete&&img.naturalWidth===1&&img.naturalHeight===1)),'explicit current assistant image did not decode visibly on the whiteboard');
  assert.equal(await shownImage.getAttribute('src'),new URL('/api/v1/conversations/room-coach/attachments/current-output-image',page.url()).href);
  emit('card',{text:'An earlier screenshot remains saved.',presentation:'later',late:true,attachments:[{id:'late-output-image',name:'Earlier saved view',mime_type:'image/png'}],conversation_id:'room-coach'});
  await wait(async()=>await page.locator('#cards .output-card').filter({hasText:'An earlier screenshot remains saved.'}).count()===1,'late image history missing');
  assert.equal(await page.locator('#board-items>.output-card').count(),1);
  await page.locator('#board-items>.output-card').getByRole('button',{name:'Put away',exact:true}).click();await page.locator('#space-tab').click();
  emit('card',{text:'A detailed card being read. '+ 'Keep this explanation in place. '.repeat(150),presentation:'current'});
  await wait(async()=>await page.locator('#board-items>.output-card').count()===1,'reading protection fixture missing');
  await page.locator('#board-items .card-answer').click();await page.locator('#space-panel').evaluate(node=>{node.scrollTop=40;});
  emit('card',{text:'Saved screenshot while reading.',presentation:'current',attachments:[{id:'reading-output-image',name:'Reading-time screenshot',mime_type:'image/png'}],conversation_id:'room-coach'});
  await wait(async()=>await page.locator('#cards .output-card').filter({hasText:'Saved screenshot while reading.'}).count()===1,'image presentation displaced a reader despite spare board capacity');
  assert.equal(await page.locator('#board-items>.output-card').count(),1);
  await page.locator('#space-tab').click();await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
  const afterImageClear=surfaceCommand('view',{action:'clear_board'});await wait(async()=>afterImageClear.state==='executed','image presentation fixture did not clear');
  checks.push('explicit assistant image output decodes visibly on an available whiteboard; late, full, pinned and reading-protected results remain saved without eviction');
  emit('card',{title:'Deliberate presentation',text:'One useful thing. '+ 'Detail for the current discussion. '.repeat(50),presentation:'current'});
  await wait(async()=>await page.locator('#board-items>.output-card').count()===1,'explicit show did not present');
  await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
  emit('card',{title:'Second deliberate item',text:'Another useful thing. '+ 'Supporting detail. '.repeat(50),presentation:'current'});
  await wait(async()=>await page.locator('#board-items>.output-card').count()===2,'second intentional item missing');
  await wait(async()=>page.locator('#space-panel').evaluate(node=>node.scrollHeight-node.clientHeight-node.scrollTop<48),'whiteboard did not follow from the bottom');
  await page.locator('#space-panel').evaluate(node=>{node.scrollTop=0;});
  emit('card',{title:'Third deliberate item',text:'More saved material.',presentation:'current'});
  await wait(async()=>await page.locator('#board-items>.output-card').count()===3,'third intentional item missing');
  assert.equal(await page.locator('#space-panel').evaluate(node=>node.scrollTop),0,'whiteboard dragged a scrolled-up reader to the bottom');
  await page.locator('#space-new').click();assert(await page.locator('#space-panel').evaluate(node=>node.scrollHeight-node.clientHeight-node.scrollTop<48));
  await page.locator('#transcript-tab').click();
  for(let i=0;i<20;i++)emit('transcript',{role:i%2?'user':'assistant',text:'Transcript scrolling fixture. '.repeat(12),fragment:true,start_ms:5000+i*2000,end_ms:5100+i*2000});
  await wait(async()=>await page.locator('.transcript-entry').count()>=20,'long transcript did not arrive');
  await page.locator('#transcript-panel').evaluate(node=>{node.scrollTop=0;});
  emit('transcript',{role:'user',text:'New words while reading older text.',fragment:true,start_ms:50000,end_ms:50200});
  await wait(async()=>/New words while reading/.test(await page.locator('#transcript').textContent()),'new transcript fragment missing');
  assert.equal(await page.locator('#transcript-panel').evaluate(node=>node.scrollTop),0);await page.locator('#transcript-new').click();
  emit('transcript',{role:'assistant',text:'Following from the bottom. '.repeat(12),fragment:true,start_ms:52000,end_ms:52200});
  await wait(async()=>/Following from the bottom/.test(await page.locator('#transcript').textContent()),'bottom transcript missing');
  await wait(async()=>page.locator('#transcript-panel').evaluate(node=>node.scrollHeight-node.clientHeight-node.scrollTop<48),'transcript did not follow from the bottom');
  const transcriptBeforeLongCall=await page.locator('.transcript-entry').count();
  for(let i=0;i<161;i++)emit('transcript',{role:i%2?'user':'assistant',text:`Long-call turn ${i}.`,fragment:true,start_ms:60000+i*2000,end_ms:60100+i*2000});
  await wait(async()=>await page.locator('.transcript-entry').count()===transcriptBeforeLongCall+161,'long call silently discarded earlier transcript rows');
  assert.match(await page.locator('.transcript-entry').first().innerText(),/Generated speech is not playback/);
  await page.locator('#context-tab').click();
  emit('card',{title:'Coach · Minecraft',awareness_id:'own-public-reply',text:'I checked the passage; I have not tested the mechanism.',source:{actor:'Coach',occurred_at:'2026-11-08T20:46:27Z',private:'NOT FOR CONTEXT RENDERING'},observed_only:true});
  const observed=page.locator('.context-entry').filter({hasText:'I checked the passage'});
  await wait(async()=>await observed.count()===1,'source observation absent from context');
  await observed.locator('summary').click();assert.match(await observed.innerText(),/Saved observation/);
  assert.match(await observed.innerText(),/does not confirm it reached voice context/);assert.doesNotMatch(await observed.innerText(),/NOT FOR CONTEXT RENDERING/);
  await page.locator('#context-panel').evaluate(node=>{node.scrollTop=0;});
  for(let i=0;i<12;i++)emit('context_delivery',{event_id:`context-scroll-${i}`,state:'queued',channel:'thinking',content:`Context scroll entry ${i}.`});
  await wait(async()=>await page.locator('.context-entry').filter({hasText:'Context scroll entry'}).count()===12,'context scroll receipts missing');
  assert.equal(await page.locator('#context-panel').evaluate(node=>node.scrollTop),0);await page.locator('#context-new').click();
  await wait(async()=>page.locator('#context-panel').evaluate(node=>node.scrollHeight-node.clientHeight-node.scrollTop<48),'context jump did not follow new content');
  await page.setViewportSize({width:390,height:844});
  assert(await page.locator('#context-tab').isVisible());assert(await page.locator('#hangup').isVisible());
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth&&document.documentElement.scrollHeight<=innerHeight));
  if(process.env.COMPANION_SCREENSHOT)await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-context-mobile.png'),fullPage:true});
  await page.setViewportSize({width:1280,height:900});
  await page.locator('#space-tab').click();
  checks.push('agent presentation preserves an item being read; whiteboard and transcript follow only from the bottom and offer an explicit jump for new content');
  checks.push('long calls retain earlier transcript text; observed connection facts are distinct from voice receipts; Context preserves reading position, offers a new-context jump and fits on a phone');
  {
    if(await page.locator('#notice').isVisible())await page.locator('#dismiss-notice').click();
    await page.locator('#space-panel').evaluate(node=>{node.scrollTop=node.scrollHeight;});
    const visualClear=surfaceCommand('view',{action:'clear_board'});await wait(async()=>visualClear.state==='executed','visual board did not clear');
    emit('card',{title:'A reading nook',kind:'note',text:'A comfortable chair by the window, a small table for a book, and warm light for the evening. Start with the chair; we can refine the rest together.',presentation:'current'});
    await wait(async()=>await page.locator('#board-items>.output-card').count()===1,'visual presentation missing');
    await page.locator('#space-panel').evaluate(node=>{node.scrollTop=0;});
    if(process.env.COMPANION_SCREENSHOT)await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-whiteboard-desktop.png'),fullPage:true});
    await page.setViewportSize({width:390,height:844});
    const boardMobileHangup=await page.locator('#hangup').boundingBox();assert(boardMobileHangup.y>=0&&boardMobileHangup.y+boardMobileHangup.height<=844);
    assert(await page.evaluate(()=>document.documentElement.scrollHeight<=innerHeight&&document.documentElement.scrollWidth<=innerWidth));
    const noteHeading=await page.locator('#board-items h2').boundingBox();assert(noteHeading.y<844&&noteHeading.y+noteHeading.height>0,'intentional note was not visible on the phone');
    assert(await page.getByRole('button',{name:'Paste text or an image',exact:true}).isVisible());assert(await page.locator('#choose-files').isVisible());
    if(process.env.COMPANION_SCREENSHOT)await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-whiteboard-mobile.png'),fullPage:true});
    await page.setViewportSize({width:1280,height:900});
    checks.push('an intentional plain-text note stays visible on the phone beside reachable paste, file-sharing and call controls');
  }
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

  heartbeatFails=true;await page.evaluate(()=>window.dispatchEvent(new Event('blur')));
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connection interrupted','heartbeat failure not visible');
  await page.waitForTimeout(1100);
  assert.equal(await page.locator('#call-status-text').innerText(),'Connection interrupted','a successful poll must not hide a failed heartbeat');
  heartbeatFails=false;await page.evaluate(()=>window.dispatchEvent(new Event('online')));
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','heartbeat did not recover');
  checks.push('heartbeat and poll failures remain distinct; a successful event poll cannot hide a failed heartbeat');

  let heldHeartbeat=null,heartbeatAttempts=0;
  await page.route('**/api/v1/companion/calls/call-1/heartbeat',async route=>{heartbeatAttempts++;if(!heldHeartbeat){heldHeartbeat=route;return;}await route.continue();});
  await page.evaluate(()=>window.dispatchEvent(new Event('blur')));
  await wait(async()=>heldHeartbeat!==null,'stale heartbeat fixture did not start');
  const callsBeforeResume=calls.size;
  // Resume the overdue watchdog before fetch callbacks. This also covers a wall
  // clock jump: neither may close healthy media before one bounded fresh check.
  assert.equal(await page.evaluate(()=>{fixture.clockOffset+=16000;fixture.controlTick();return fixture.peers[0].closed||fixture.tracks[0].stopped;}),false);
  await wait(async()=>heartbeatAttempts>=2,'stale in-flight heartbeat blocked recovery');
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','suspended timer recovery did not reconcile');
  await heldHeartbeat.abort().catch(()=>{});await page.unroute('**/api/v1/companion/calls/call-1/heartbeat');
  assert.equal(calls.size,callsBeforeResume);assert.equal(await page.evaluate(()=>fixture.peers[0].closed||fixture.tracks[0].stopped),false);
  assert(requests.some(r=>r.path.endsWith('/heartbeat')&&r.body?.diagnostics?.trigger==='timer_resume'));
  checks.push('an overdue watchdog reconciles the same call after a timer pause, replaces a stale heartbeat, and never redials or stops healthy media first');

  const mediaBeforePicker=await page.evaluate(()=>fixture.media.length);
  const chooserPromise=page.waitForEvent('filechooser');await page.locator('#choose-files').click();const chooser=await chooserPromise;
  await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,value:true});document.dispatchEvent(new Event('visibilitychange'));});
  assert(await page.locator('#call-screen').isVisible());
  await chooser.setFiles([]);
  await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,value:false});document.dispatchEvent(new Event('visibilitychange'));document.getElementById('share-files').dispatchEvent(new Event('cancel'));window.dispatchEvent(new Event('focus'));});
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','file picker return did not reconcile');
  assert.equal(calls.size,callsBeforeResume);assert.equal(await page.evaluate(()=>fixture.media.length),mediaBeforePicker);
  assert.equal(await page.evaluate(()=>fixture.tracks[0].stopped||fixture.peers[0].closed),false);
  for(const r of requests.filter(r=>r.body?.diagnostics)){
    assert.deepEqual(Object.keys(r.body.diagnostics).sort(),['trigger','visibility','peer_state','ice_state','channel_state','audio_state','wake_lock','control_age_ms','heartbeat_age_ms','timer_gap_ms'].sort());
    for(const key of ['control_age_ms','heartbeat_age_ms','timer_gap_ms'])assert(Number.isInteger(r.body.diagnostics[key])&&r.body.diagnostics[key]>=0&&r.body.diagnostics[key]<=3600000);
  }
  checks.push('file-picker and visibility transitions preserve voice and refresh the existing call; diagnostics contain only bounded lifecycle states and timings');

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
  await page.locator('#screen-awake').uncheck();await page.evaluate(()=>{fixture.delayWake=true;});await page.locator('#screen-awake').check();
  await wait(async()=>page.evaluate(()=>fixture.wakeGrants.length===1),'hangup pending screen lock fixture missing');
  holdNextClose=true;
  const voiceHangup=surfaceCommand('hangup',{action:'end_call'});
  await page.locator('#call-ended:not([hidden])').waitFor();
  await wait(async()=>voiceHangup.state==='executed','voice hangup did not acknowledge released media');
  await wait(async()=>heldClose!==null,'graceful close was not requested');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)));
  await page.evaluate(()=>{fixture.delayWake=false;fixture.wakeGrants.shift()();});
  await wait(async()=>page.evaluate(()=>fixture.wakeLocks.every(lock=>lock.released)),'late wake lock survived call end');
  assert(await page.evaluate(()=>fixture.peers.every(peer=>!peer.closed)&&fixture.channels.every(channel=>!channel.closed)&&fixture.audioContexts.every(context=>!context.closed)),'silent transport must remain until terminal close');
  heldClose(200);heldClose=null;
  heldSurfaceAck();heldSurfaceAck=null;await page.waitForTimeout(100);
  assert.ok(!requests.some(r=>r.path.endsWith(`/${lateView.operation_id}/ack`)&&r.body.state==='executed'));
  assert.match(await page.locator('#recovery-detail').innerText(),/Closing/);
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
  const mutationsBeforeUnknown=requests.filter(r=>r.method!=='GET').length,mediaBeforeUnknown=await page.evaluate(()=>fixture.media.length);
  await page.locator('#back-to-contacts').click();
  assert(await page.locator('#phonebook').isVisible(),'Contacts must open even when the creation response was lost');
  await wait(async()=>await page.locator('.contact-card').count()===3,'contacts were not rendered during unknown call recovery');
  await page.evaluate(()=>{window.dispatchEvent(new Event('focus'));window.dispatchEvent(new Event('online'));});
  assert(await page.locator('#call-recovery').isVisible());assert(await page.locator('#check-call-status').isHidden());
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isDisabled());
  assert.equal(await page.locator('#recover-call').innerText(),'Resolve unconfirmed call');
  assert.equal(requests.filter(r=>r.method!=='GET').length,mutationsBeforeUnknown);
  assert.equal(await page.evaluate(()=>fixture.media.length),mediaBeforeUnknown);
  await page.locator('#recover-call').click();await wait(async()=>await page.locator('#recover-call').isHidden(),'lost response recovery did not finish');
  const retriedRequest=requests.filter(request=>request.path==='/api/v1/companion/calls').at(-1).body;
  assert.deepEqual(lostRequest,retriedRequest);assert.equal(calls.size,2);assert.equal(calls.get('call-2').state,'closed');
  checks.push('Contacts stays accessible after a lost creation response; navigation and focus preserve the unknown request without writes or media, and explicit recovery uses the same UUID and SDP');

  await page.getByRole('button',{name:'Call Jarvis',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','Jarvis fixture did not connect');
  await wait(async()=>/cannot inspect/.test(await page.locator('#share-capability').innerText()),'effective capability missing before capture');
  await page.locator('#call-voice-settings').click();await page.locator('#voice-choice:not([disabled])').waitFor();assert.match(await page.locator('#voice-model-facts').innerText(),/Reasoning model.*Not yet reported/s);assert(await page.locator('#voice-advanced').isHidden());await page.locator('#close-voice-settings').click();
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
  assert.match(await page.locator('#recovery-detail').innerText(),/not yet confirmed/);
  const outageMutations=requests.filter(r=>r.method!=='GET').length,outageMedia=await page.evaluate(()=>fixture.media.length);
  catalogFails=true;await page.locator('#back-to-contacts').click();
  assert(await page.locator('#phonebook').isVisible(),'Contacts navigation was blocked by an unresolved call');
  assert(await page.locator('#call-ended').isHidden());assert(await page.locator('#call-recovery').isVisible());
  await wait(async()=>/contacts temporarily unavailable/.test(await page.locator('#notice-text').innerText()),'catalog failure not shown');
  await page.locator('#dismiss-notice').click();
  assert(await page.locator('#phonebook').isVisible());assert(await page.locator('#call-recovery').isVisible());
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isDisabled());
  await page.locator('#check-call-status').click();
  await wait(async()=>/could not be checked/.test(await page.locator('#recovery-detail').innerText()),'read failure did not retain recovery');
  assert.equal(requests.filter(r=>r.method!=='GET').length,outageMutations);
  assert.equal(await page.evaluate(()=>fixture.media.length),outageMedia);
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  if(process.env.COMPANION_SCREENSHOT)await page.screenshot({path:process.env.COMPANION_SCREENSHOT.replace('.png','-contacts-recovery.png'),fullPage:true});
  checks.push('an unresolved call and failed catalog never block Contacts; dismissing an error leaves recovery usable and Call disabled without writes or media');

  catalogFails=false;controlFails=false;
  await page.route('**/api/v1/companion/calls/call-4?after=0',route=>route.fulfill({status:200,json:{id:'other-call',state:'closed'}}));
  await page.locator('#check-call-status').click();
  await wait(async()=>/could not be checked/.test(await page.locator('#recovery-detail').innerText()),'wrong call identity was accepted');
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isDisabled());
  await page.unroute('**/api/v1/companion/calls/call-4?after=0');
  calls.get('call-4').state='closed';
  await page.evaluate(()=>window.dispatchEvent(new Event('online')));
  await wait(async()=>await page.locator('#call-recovery').isHidden(),'terminal same-call status did not clear stale recovery');
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled(),'existing contact Call button stayed disabled after terminal reconciliation');
  assert.equal(requests.filter(r=>r.method!=='GET').length,outageMutations);
  assert.equal(await page.evaluate(()=>fixture.media.length),outageMedia);
  assert.equal(calls.size,4);checks.push('only a terminal status with the exact call ID clears recovery and re-enables already-rendered Call buttons; online reconciliation never redials or closes again');

  await page.evaluate(()=>{fixture.delayMic=true;});
  await page.getByRole('button',{name:'Call Jarvis',exact:true}).click();await page.locator('#call-screen:not([hidden])').waitFor();
  await page.locator('#hangup').click();await page.evaluate(()=>fixture.resolveMic());await page.waitForTimeout(150);
  assert.equal(calls.size,4);assert(await page.evaluate(()=>fixture.tracks.at(-1).stopped));
  checks.push('late microphone permission after hangup is released without creating a call');

  calls.set('reloaded-call',{id:'reloaded-call',owner:authOwner,conversation_id:'earlier-room',state:'connecting',request:{agent_id:'coach'}});catalogAvailable=false;
  const beforeReload=requests.filter(request=>request.path==='/api/v1/companion/calls').length;
  await page.reload();await page.getByRole('button',{name:'End earlier call',exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>fixture.media.length),0);assert.equal(await page.evaluate(()=>fixture.peers.length),0);
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,beforeReload);
  assert(await page.locator('#phonebook').isVisible());assert(await page.locator('#call-recovery').isVisible());
  assert.match(await page.locator('#recovery-detail').innerText(),/already accepted can continue/);
  assert.equal(await page.locator('#recovery-work-link').getAttribute('href'),'/?conversation=earlier-room');
  await page.getByRole('button',{name:'End earlier call',exact:true}).click();
  await wait(async()=>await page.locator('#recover-call').isHidden(),'previous call did not close');
  assert.equal(calls.get('reloaded-call').state,'closed');assert.equal(await page.evaluate(()=>fixture.media.length),0);
  catalogAvailable=true;
  await page.reload();await page.getByRole('button',{name:'Call Rook',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','fresh call blocked after explicit prior closure');
  assert.equal(requests.filter(request=>request.path==='/api/v1/companion/calls').length,beforeReload+1);
  await page.locator('#hangup').click();await wait(async()=>await page.locator('#recover-call').isHidden(),'last fixture call did not close');
  checks.push('reload discovers the owner’s existing call without media or redial; explicit close permits a later fresh call');

  await page.locator('#back-to-contacts').click();events.length=0;
  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','logout fixture did not connect');
  emit('transcript',{role:'user',text:'Private prior caller text.',fragment:true,start_ms:0,end_ms:100});
  emit('card',{text:'Private prior caller result.',conversation_id:'room-coach'});
  emit('context_delivery',{event_id:'private-context',state:'queued',content:'Private prior caller context.'});
  await wait(async()=>await page.locator('.output-card').count()===1,'logout fixture result missing');
  await wait(async()=>await page.locator('.context-entry').count()===1,'logout fixture context missing');
  holdNextUpload=true;await page.locator('#share-files').setInputFiles({name:'late-private.txt',mimeType:'text/plain',buffer:Buffer.from('Private pending upload.')});
  await wait(async()=>heldUpload!==null,'late upload fixture did not reach storage');
  const availableBeforeLogout = requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available').length;
  let deliverOldStatus;
  await page.route('**/api/v1/companion/calls/call-6?after=0',async route=>{
    const response=await route.fetch();deliverOldStatus=()=>route.fulfill({response});
  });
  holdNextClose=true;await page.locator('#hangup').click();
  await wait(async()=>heldClose!==null,'close was not pending before sign-out');
  await page.evaluate(()=>window.dispatchEvent(new Event('focus')));
  await wait(async()=>deliverOldStatus!==undefined,'old recovery status was not pending');
  await page.locator('#sign-out').click();
  await page.locator('#login:not([hidden])').waitFor();await wait(async()=>heldClose!==null,'close was not pending at sign-out');
  assert.equal(await page.locator('.transcript-entry').count(),0);assert.equal(await page.locator('.output-card').count(),0);assert.equal(await page.locator('.context-entry').count(),0);
  assert.equal(await page.locator('#continue-work').getAttribute('href'),'/');assert.equal(await page.locator('#ended-work-link').getAttribute('href'),'/');
  assert(await page.evaluate(()=>fixture.tracks.every(track=>track.stopped)&&fixture.peers.every(peer=>peer.closed)));
  await page.locator('#username').fill('next-fixture-user');await page.locator('#password').fill('fixture-only');await page.locator('#sign-in').click();
  await page.locator('#app:not([hidden])').waitFor();heldClose();heldClose=null;heldUpload();heldUpload=null;await deliverOldStatus();await page.waitForTimeout(200);
  assert.equal(requests.filter(request=>/\/uploads$/.test(request.path)&&request.body.state==='available').length,availableBeforeLogout);
  assert.equal(await page.locator('#recover-call').isHidden(),true);assert.equal(await page.locator('#call-ended').isHidden(),true);
  assert(await page.locator('#call-recovery').isHidden());assert.equal(await page.locator('#recovery-detail').textContent(),'');assert.equal(await page.locator('#recovery-work-link').getAttribute('href'),'/');
  assert.equal(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled(),true);
  assert.doesNotMatch(await page.locator('body').textContent(),/Private prior caller/);
  assert.equal(await page.evaluate(()=>Object.keys(localStorage).length+Object.keys(sessionStorage).length),0);
  checks.push('sign-out clears private presentation and devices; delayed old status reads, upload receipts and close failures cannot contaminate the next owner’s login');

  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','provider interruption fixture did not connect');
  const interruptedCall=[...calls.values()].at(-1),closesBeforeProvider=requests.filter(r=>r.path.endsWith('/close')).length;
  interruptedCall.state='interrupted';interruptedCall.closure={confirmed:true,provider_reason:'connection_lost',reason:'The voice provider lost the audio connection. Your saved work remains in the conversation.'};
  await page.locator('#call-ended:not([hidden])').waitFor();
  assert.equal(await page.locator('#ended-title').innerText(),'Call interrupted');
  assert.equal(await page.locator('#ended-detail').innerText(),interruptedCall.closure.reason);
  assert.doesNotMatch(await page.locator('#ended-detail').innerText(),/not confirmed/);
  assert.equal(requests.filter(r=>r.path.endsWith('/close')).length,closesBeforeProvider);
  assert(await page.evaluate(()=>fixture.tracks.every(t=>t.stopped)&&fixture.peers.every(p=>p.closed)));
  checks.push('confirmed provider connection loss is shown as an interruption with its saved reason, releases devices, and does not issue a duplicate close');

  await page.locator('#back-to-contacts').click();await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','page exit fixture did not connect');
  const exitingCall=[...calls.values()].at(-1);
  let deliverExitTerminal;
  await page.route(`**/api/v1/companion/calls/${exitingCall.id}?after=0`,async route=>{
    const response=await route.fetch();
    if((await response.json()).state==='closed'){deliverExitTerminal=()=>route.fulfill({response});return;}
    await route.fulfill({response});
  });
  await page.evaluate(()=>window.dispatchEvent(new Event('pagehide')));
  assert(await page.evaluate(()=>fixture.tracks.every(t=>t.stopped)),'page exit must stop sensors before any close receipt');
  await wait(async()=>deliverExitTerminal!==undefined,'explicit page exit did not reach terminal close');
  assert.equal(exitingCall.state,'closed');
  // Server closure precedes delivery to confirmClose; the silent peer remains
  // until the browser receives that terminal response and releases transport.
  assert.equal(await page.evaluate(()=>fixture.peers.at(-1).closed),false);
  const exitRequest=requests.find(r=>r.path===`/api/v1/companion/calls/${exitingCall.id}/close`);
  assert.equal(exitRequest.body.reason,'page_exit');assert.equal(exitRequest.body.diagnostics.trigger,'page_exit');
  assert.equal(exitRequest.body.diagnostics.peer_state,'connected','closure must retain the pre-release transport state');
  await deliverExitTerminal();
  await wait(async()=>page.evaluate(()=>fixture.tracks.every(t=>t.stopped)&&fixture.peers.every(p=>p.closed)),'page exit terminal receipt did not release browser transport');
  checks.push('page exit still stops sensors immediately and records an explicit exit reason plus pre-release bounded transport diagnostics');
  await page.locator('#back-to-contacts').click();await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','late close fixture did not connect');
  const lateCloseCall=[...calls.values()].at(-1);holdNextClose=true;
  await page.locator('#hangup').click();await wait(async()=>heldClose!==null,'late close was not held');
  let deliverOldCatalog;
  await page.route('**/api/v1/companion',async route=>{
    const response=await route.fetch();deliverOldCatalog=()=>route.fulfill({response});
  });
  await page.locator('#back-to-contacts').click();
  await wait(async()=>deliverOldCatalog!==undefined,'old catalog was not held');
  await wait(async()=>await page.locator('#check-call-status').isEnabled(),'first nonterminal check did not settle');
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isDisabled());
  const writesBeforeTerminal=requests.filter(r=>r.method!=='GET').length;
  lateCloseCall.state='closed';await page.locator('#check-call-status').click();
  await wait(async()=>await page.locator('#call-recovery').isHidden(),'known terminal call remained blocked behind pending close');
  assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled());
  assert.equal(requests.filter(r=>r.method!=='GET').length,writesBeforeTerminal);
  assert(await page.evaluate(()=>fixture.peers.every(peer=>peer.closed)),'terminal observation must release any retained old transport');
  await page.getByRole('button',{name:'Call Coach',exact:true}).click();
  await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','new explicit call did not connect');
  // A newer active call legitimately reports fresh browser media state.
  const substantiveWrites=()=>requests.filter(r=>r.method!=='GET'&&!r.path.endsWith('/surface/presence')&&!r.path.endsWith('/heartbeat')).length;
  const latestCall=[...calls.values()].at(-1),writesBeforeLate=substantiveWrites();
  heldClose();heldClose=null;await deliverOldCatalog();await page.waitForTimeout(200);
  await page.unroute('**/api/v1/companion');
  assert(await page.locator('#call-recovery').isHidden());assert(await page.locator('#call-screen').isVisible());
  assert.equal(await page.locator('#call-status-text').innerText(),'Connected');
  assert.equal(substantiveWrites(),writesBeforeLate);
  assert.equal(await page.evaluate(()=>fixture.peers.at(-1).closed||fixture.tracks.at(-1).stopped),false);
  assert.equal([...calls.values()].at(-1).id,latestCall.id);
  await page.locator('#hangup').click();await wait(async()=>await page.locator('#call-recovery').isHidden(),'last call did not settle');
  checks.push('a terminal read settles a pending close; late close failure and stale catalog cannot restore old recovery or alter a later explicit call');
  for (const {lateOutcome,deliveryStage} of ['success','failure'].flatMap(lateOutcome=>['during recovery','before next call','after next call'].map(deliveryStage=>({lateOutcome,deliveryStage})))) {
    await page.locator('#back-to-contacts').click();
    let deliverOriginal,deliverRetry;
    await page.route('**/api/v1/companion/calls',async route=>{
      if(!deliverOriginal && route.request().method()==='POST') {
        const response=await route.fetch();deliverOriginal=()=>lateOutcome==='success'?route.fulfill({response}):route.abort('failed');return;
      }
      if(deliveryStage==='during recovery' && !deliverRetry && route.request().method()==='POST') {
        const response=await route.fetch();deliverRetry=()=>route.fulfill({response});return;
      }
      await route.continue();
    });
    await page.getByRole('button',{name:'Call Coach',exact:true}).click();
    await wait(async()=>deliverOriginal!==undefined,'original creation response was not held');
    const original=[...calls.values()].at(-1),createsBeforeResolution=requests.filter(r=>r.path==='/api/v1/companion/calls').length;
    await page.locator('#hangup').click();await page.locator('#back-to-contacts').click();
    assert(await page.locator('#phonebook').isVisible());assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isDisabled());
    assert(await page.locator('#check-call-status').isHidden());
    const closesBeforeResolution=requests.filter(r=>r.path.endsWith('/close')).length;
    await page.locator('#recover-call').click();
    if(deliveryStage==='during recovery') {
      await wait(async()=>deliverRetry!==undefined,'explicit resolution response was not held');
      await deliverOriginal();await page.waitForTimeout(100);
      assert.equal(requests.filter(r=>r.path.endsWith('/close')).length,closesBeforeResolution,'original response must leave close ownership with the explicit resolver');
      await deliverRetry();
    }
    await wait(async()=>await page.locator('#call-recovery').isHidden(),'explicit pending creation recovery did not settle');
    assert.equal(requests.filter(r=>r.path.endsWith('/close')).length,closesBeforeResolution+1);
    if(deliveryStage==='before next call') {
      await deliverOriginal();await page.waitForTimeout(100);
      assert(await page.locator('#call-recovery').isHidden());assert(await page.getByRole('button',{name:'Call Coach',exact:true}).isEnabled());
      assert.equal(requests.filter(r=>r.path.endsWith('/close')).length,closesBeforeResolution+1);
    }
    assert.equal(original.state,'closed');
    const creationRequests=requests.filter(r=>r.path==='/api/v1/companion/calls');
    assert.equal(creationRequests.length,createsBeforeResolution+1);assert.deepEqual(creationRequests.at(-1).body,original.request);
    await page.getByRole('button',{name:'Call Coach',exact:true}).click();
    await wait(async()=>await page.locator('#call-status-text').innerText()==='Connected','new call after pending creation did not connect');
    const closesBeforeOriginal=requests.filter(r=>r.path.endsWith('/close')).length;
    if(deliveryStage==='after next call') { await deliverOriginal();await page.waitForTimeout(200); }
    assert(await page.locator('#call-recovery').isHidden());assert(await page.locator('#call-screen').isVisible());
    assert.equal(await page.locator('#call-status-text').innerText(),'Connected');
    assert.equal(requests.filter(r=>r.path.endsWith('/close')).length,closesBeforeOriginal,'late original creation must not close again');
    assert.equal(await page.evaluate(()=>fixture.peers.at(-1).closed||fixture.tracks.at(-1).stopped),false);
    await page.unroute('**/api/v1/companion/calls');
    await page.locator('#hangup').click();await wait(async()=>await page.locator('#call-recovery').isHidden(),'pending creation successor did not settle');
    checks.push(`late original creation ${lateOutcome} ${deliveryStage} preserves explicit recovery ownership and never restores a settled call`);
  }
  assert.deepEqual(errors,[]);
  console.log(JSON.stringify({passed:checks.length,checks,scope:'real Chromium UI; fake API, media, and provider; no paid calls'},null,2));
} finally { await browser.close(); await new Promise(resolve=>server.close(resolve)); }
