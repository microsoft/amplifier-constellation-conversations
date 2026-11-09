import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {LiveBridge,sessionOptions,spokenExcerpt,safeError,publicEvent} from '../live.mjs';
const create={op:'create',sdp:'v=0\r\n',voice:'marin',instructions:'You are Coach.'};
class FakeSideband extends EventEmitter {
 constructor(){super();this.socket=new EventEmitter();this.socket.readyState=1;this.sent=[];}
 send(e){this.sent.push(e);}
 close(){this.emit('close');}
}
function fixture(options={}) {
 const events=[],sideband=new FakeSideband(),requests=[];
 const client={live:{create:async(body,opts)=>{requests.push({body,opts});return {session:{id:'live_one'},transport:{type:'webrtc',sdp:'v=0 answer'}};}}};
 const bridge=new LiveBridge({emit:e=>events.push(e),client,connect:()=>sideband,finalizationMs:10,...options});
 return {bridge,events,sideband,requests};
}
test('client delegation, server authority and recording policy are fixed at startup',()=>{
 const options=sessionOptions(create);
 assert.equal(options.session.model,'gpt-live-1');assert.equal(options.session.delegation.type,'client');
 assert.deepEqual(options.session.client.data_channel.allowed_client_events,[]);assert.equal(options.session.store,false);
 assert.ok(options.session.client.data_channel.allowed_server_events.every(e=>typeof e.type==='string')); 
 assert.equal(sessionOptions({...create,history:'The user asked about a project.'}).session.input[0].role,'user');
 assert.throws(()=>sessionOptions({...create,voice:'invented'}));
});
test('append rejects oversized content instead of cutting a sentence or qualifier',()=>{
 assert.throws(()=>spokenExcerpt('🚀漢字'.repeat(500)), /invalid_content/);
 const fact='The local test videos run on ARM64; they are not YouTube videos.';
 assert.equal(spokenExcerpt(fact),fact);
 assert.throws(()=>spokenExcerpt('x'.repeat(470)+' videos on ARM64.'), /invalid_content/);
 assert.equal(spokenExcerpt('Ready.'),'Ready.');
});
test('provider errors and session snapshots cannot expose credentials/instructions',()=>{
 const secret='sk-private-KEY';assert.ok(!JSON.stringify(safeError({status:401,message:secret,headers:{authorization:secret}})).includes(secret));
 assert.deepEqual(publicEvent({type:'session.started',event_id:'s',session:{id:'live_one',instructions:secret}}),{type:'session.started',event_id:'s',session:{id:'live_one'}});
 assert.equal(publicEvent({type:'private.reasoning',text:secret}),null);
});
test('provider error diagnostics retain bounded protocol fields and nested command identity only',()=>{
 const source={type:'error',event_id:'provider-error',error:{type:'invalid_request_error',code:'unknown_parameter',param:'delegation_id',client_event_id:'append-1',message:'sk-private-KEY',headers:{authorization:'sk-private-KEY'},session:{instructions:'private'}}};
 assert.deepEqual(publicEvent(source,new Set(['append-1'])),{type:'error',event_id:'provider-error',error:{type:'invalid_request_error',code:'unknown_parameter',param:'delegation_id',client_event_id:'append-1',message:'The voice service reported an error.'}});
 assert.equal(publicEvent(source).error.client_event_id,undefined);
 const conflicting=publicEvent({...source,client_event_id:'append-2'},new Set(['append-1','append-2']));
 assert.equal(conflicting.client_event_id,undefined);assert.equal(conflicting.error.client_event_id,undefined);
 for(const bad of ['sk-private-KEY','x'.repeat(161),'private text','https://private.invalid']) {
  const result=publicEvent({...source,client_event_id:bad,error:{...source.error,code:bad,type:bad,param:bad,client_event_id:bad}});
  assert.deepEqual(result,{type:'error',event_id:'provider-error',error:{code:'voice_service_error',message:'The voice service reported an error.'}});
 }
});
test('SDK provider-error event and error callback preserve diagnostics without replaying the append',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);
 await bridge.command({op:'append',channel:'commentary',event_id:'append-1',delegation_id:'delegate-1',content:'The saved result is available.'});
 const provider={type:'error',event_id:'provider-error',error:{type:'invalid_request_error',code:'invalid_value',param:'delegation_id',client_event_id:'append-1',message:'private context'}};
 sideband.emit('event',provider);
 const sdkError=new Error('private context');Object.defineProperty(sdkError,'error',{value:provider,enumerable:false});
 sideband.emit('error',sdkError);assert.equal(bridge.state,'active');
 assert.equal(events.find(e=>e.event?.type==='error').event.error.client_event_id,'append-1');
 assert.equal(sideband.sent.filter(e=>e.type==='session.commentary.append').length,1);
 assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,0);
 assert.equal(JSON.stringify(events).includes('private context'),false);
 sideband.emit('event',{type:'session.output_transcript.delta',event_id:'later',delta:'I can keep listening.',start_ms:10,end_ms:20});
 assert.ok(events.some(e=>e.event?.event_id==='later'));
 await bridge.close();
});
test('duplicate append IDs are not resent; changed ID payload rejected',async()=>{
 const {bridge,sideband,events,requests}=fixture();await bridge.command(create);
 assert.equal(events[0].type,'ready');assert.equal(requests[0].opts.maxRetries,0);
 const append={op:'append',channel:'commentary',event_id:'one',delegation_id:'d1',content:'The check passed.'};
 await bridge.command(append);await bridge.command(append);assert.equal(sideband.sent.length,1);
 await assert.rejects(bridge.command({...append,content:'Something else'}));
 await bridge.close();assert.equal(events.at(-1).confirmed,false);
});
test('only matching terminal settlement confirms close',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);const closing=bridge.close();
 sideband.emit('event',{type:'session.closed',session:{id:'different'}});assert.equal(bridge.finished,false);
 assert.equal(events.some(e=>e.event?.session?.id==='different'),false);
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},reason:'close_requested',usage:{seconds:1}});
 await closing;assert.equal(events.at(-1).confirmed,true);assert.equal(sideband.sent.at(-1).type,'session.close');
});
test('default bridge has no local duration cutoff and honors provider expiry',async t=>{
 t.mock.timers.enable({apis:['setTimeout']});
 const {bridge,sideband,events}=fixture();await bridge.command(create);
 t.mock.timers.tick(3_600_000);
 assert.equal(bridge.state,'active');assert.equal(bridge.expiry,undefined);
 assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,0);
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},reason:'expired',usage:{seconds:3600}});
 await bridge.done;
 assert.equal(bridge.state,'closed');assert.equal(events.at(-1).confirmed,true);
 assert.equal(events.find(e=>e.event?.type==='session.closed').event.reason,'expired');
});
test('an explicit fixture duration still requests close and awaits terminal settlement',async t=>{
 t.mock.timers.enable({apis:['setTimeout']});
 const {bridge,sideband,events}=fixture({maxCallMs:100});await bridge.command(create);
 t.mock.timers.tick(99);assert.equal(bridge.state,'active');
 t.mock.timers.tick(1);assert.equal(bridge.state,'closing');assert.equal(bridge.finished,false);
 assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,1);
 assert.equal(events.some(e=>e.type==='closed'),false);
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},reason:'close_requested',usage:{seconds:0.1}});
 await bridge.done;assert.equal(events.at(-1).confirmed,true);
});
test('socket disappearance is unknown finalization, never fabricated completion',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);sideband.emit('close');await bridge.done;
 assert.equal(events.at(-1).confirmed,false);
});
test('failed create does not retry or emit a ready call',async()=>{
 let attempts=0;const {bridge,events}=fixture({client:{live:{create:async()=>{attempts++;throw {status:429,message:'private'};}}}});
 await bridge.command(create);assert.equal(attempts,1);assert.equal(events[0].code,'provider_capacity');
 assert.equal(events.some(e=>e.type==='ready'),false);assert.equal(events.at(-1).confirmed,false);
});
test('close during creation waits for the allocated session and closes it without becoming ready',async()=>{
 let resolveCreate;
 const pending=new Promise(resolve=>{resolveCreate=resolve;});
 const {bridge,sideband,events}=fixture({client:{live:{create:()=>pending}}});
 const creating=bridge.command(create);
 const closed=bridge.close();
 assert.equal(bridge.finished,false);
 resolveCreate({session:{id:'live_one'},transport:{type:'webrtc',sdp:'v=0 answer'}});
 await creating;
 assert.equal(events.some(e=>e.type==='ready'),false);
 assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,1);
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},reason:'close_requested',usage:{seconds:0}});
 await closed;
 assert.equal(events.at(-1).confirmed,true);
});
test('a failed pending create settles shutdown without retry or leaked readiness',async()=>{
 let rejectCreate;
 const pending=new Promise((_,reject)=>{rejectCreate=reject;});
 const {bridge,events}=fixture({client:{live:{create:()=>pending}}});
 const creating=bridge.command(create),closed=bridge.close();
 rejectCreate(new Error('private credentials'));
 await creating;await closed;
 assert.equal(events.filter(e=>e.type==='closed').length,1);
 assert.equal(events.some(e=>e.type==='ready'),false);
 assert.equal(JSON.stringify(events).includes('private credentials'),false);
});
test('closing while sideband connects sends one close and never exposes a ready call',async()=>{
 const {bridge,sideband,events}=fixture();sideband.socket.readyState=0;
 const creating=bridge.command(create);
 await new Promise(resolve=>setImmediate(resolve));
 const closing=bridge.close();bridge.close();
 sideband.socket.readyState=1;sideband.socket.emit('open');await creating;
 assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,1);
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},reason:'close_requested',usage:{seconds:0}});
 await closing;assert.equal(events.some(e=>e.type==='ready'),false);
});
test('malformed terminal events cannot fabricate finalization',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);const closing=bridge.close();
 sideband.emit('event',{type:'session.closed',session:{id:'live_one'},usage:{private:'secret'}});
 assert.equal(bridge.finished,false);await closing;
 assert.equal(events.at(-1).confirmed,false);
});
test('public event projections omit extra fields even on allowed event types',()=>{
 const extra={credentials:'private',instructions:'private',unknown:{secret:'private'}};
 const transcript=publicEvent({type:'session.input_transcript.delta',event_id:'e1',delta:'Hello',start_ms:1,end_ms:2,...extra});
 assert.deepEqual(transcript,{type:'session.input_transcript.delta',event_id:'e1',delta:'Hello',start_ms:1,end_ms:2});
 const usage=publicEvent({type:'session.usage.updated',event_id:'e2',usage:{seconds:1,...extra},context_window:{usage_ratio:0.2,...extra},...extra});
 assert.deepEqual(usage,{type:'session.usage.updated',event_id:'e2',usage:{seconds:1},context_window:{usage_ratio:0.2}});
 const delegation=publicEvent({type:'session.delegation.created',event_id:'e3',delegation:{id:'d1',target:'client',...extra},offset_ms:2,...extra});
 assert.deepEqual(delegation,{type:'session.delegation.created',event_id:'e3',delegation:{id:'d1',target:'client'},offset_ms:2});
 assert.equal(publicEvent({type:'session.delegation.created',delegation:{id:'d1',target:'responses'},offset_ms:2}),null);
});

test('per-call MCP listener is private to the host and closes with bridge lifecycle',async()=>{
 const {bridge,events,requests}=fixture();
 const token='fixture-private-call-grant-0123456789';
 await bridge.command({...create,mcp:{api_base:'http://127.0.0.1:49123',call_id:'e0f36409-a5a6-4c1a-9355-0cd5ed10ce10',token}});
 const ready=events.find(e=>e.type==='ready');
 assert.ok(Number.isInteger(ready.mcp_port));
 assert.ok(!JSON.stringify(requests).includes(token));
 assert.ok(!JSON.stringify(events).includes(token));
 const endpoint=`http://127.0.0.1:${ready.mcp_port}/mcp`;
 assert.equal((await fetch(endpoint,{method:'POST',body:'{}'})).status,401);
 await bridge.close();
 await assert.rejects(fetch(endpoint,{method:'POST',body:'{}'}));
});

test('host voice whitelist exactly matches the SDK-checked bridge whitelist',async()=>{
 const {readFile}=await import('node:fs/promises');
 const host=await readFile(new URL('../../src/companion.rs',import.meta.url),'utf8');
 const names=host.match(/const VOICES:[\s\S]*?= &\[([\s\S]*?)\];/)[1].match(/"([a-z]+)"/g).map(v=>JSON.parse(v));
 for (const voice of names) assert.equal(sessionOptions({...create,voice}).session.audio.output.voice,voice);
 assert.equal(names.length,23);
});

test('uncorrelated provider errors do not imply closure but transport failures do',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);
 const provider={type:'error',event_id:'speech-error',error:{type:'server_error',code:null,message:'private diagnostic'}};
 sideband.emit('event',provider);
 const sdkError=new Error('private diagnostic');Object.defineProperty(sdkError,'error',{value:provider});
 sideband.emit('error',sdkError);assert.equal(bridge.state,'active');
 assert.equal(sideband.sent.length,0);assert.equal(events.filter(e=>e.type==='closed').length,0);
 sideband.emit('error',new Error('secret socket failure'));await bridge.done;
 assert.equal(events.at(-1).confirmed,false);assert.equal(sideband.sent.filter(e=>e.type==='session.close').length,1);
 assert.equal(JSON.stringify(events).includes('private diagnostic'),false);
 assert.equal(JSON.stringify(events).includes('secret socket failure'),false);
});

const inputEvent=(id,text='Please end this call.',start=100,end=200)=>({type:'session.input_transcript.delta',event_id:id,delta:text,start_ms:start,end_ms:end});
const endDecision=(source,probability=.999,patch={})=>({type:'decision.event',event_id:`decision-${source}`,delegation_id:null,trigger_id:`trigger-${source}`,source_event_type:'session.input_transcript.delta',source_event_id:source,event:{type:'decision.completed',decision:{answers:[{name:'end_current_call',type:'predicate',probability}],model:'native-fixture',usage:{}}},...patch});
const waitControl=()=>new Promise(resolve=>setTimeout(resolve,30));

test('native call control is input-only and independent of client reasoning delegation',()=>{
 const options=sessionOptions({...create,native_call_control:true});
 assert.equal(options.session.delegation.type,'client');
 assert.equal(options.session.triggers.length,1);
 const trigger=options.session.triggers[0];
 assert.equal(trigger.event_type,'session.input_transcript.delta');assert.equal(trigger.type,'decisions');
 assert.equal(trigger.decisions.questions.length,1);assert.equal(trigger.decisions.questions[0].name,'end_current_call');
 const instructions=trigger.decisions.questions[0].instructions;
 for(const term of ['COMPLETE','Quoted','hypothetical','negative','revokes','Stop working on Minecraft = false','Please end = false','assistant speech','indirect polite request','can we end here?','How do I hang up? = false'])assert.ok(instructions.toLowerCase().includes(term.toLowerCase()));
 assert.equal(options.session.client.data_channel.allowed_server_events.some(v=>v.type==='decision.event'),false);
});

test('a matching current native end-call signal closes exactly once even without backend work',async()=>{
 const {bridge,sideband,events}=fixture({callControlGraceMs:10});await bridge.command({...create,native_call_control:true});
 sideband.emit('event',inputEvent('end-now'));sideband.emit('event',endDecision('end-now'));
 await waitControl();
 assert.equal(sideband.sent.filter(v=>v.type==='session.close').length,1);
 const control=events.find(v=>v.type==='call_control');assert.equal(control.action,'end_call');assert.equal(control.decision,'requested');assert.equal(control.session_id,'live_one');
 assert.equal(control.source_event_id,'end-now');assert.equal(control.source_end_ms,200);
 sideband.emit('event',endDecision('end-now'));await waitControl();
 assert.equal(sideband.sent.filter(v=>v.type==='session.close').length,1);
 assert.equal(events.filter(v=>v.type==='call_control').length,1);
 const diagnostics=events.filter(v=>v.event?.type==='companion.native_call_control.diagnostic').map(v=>v.event.reason);
 assert.ok(diagnostics.includes('close_requested'));
 assert.equal(diagnostics.includes('pending_cancelled'),false,'A fired close decision is no longer a pending cancellation');
 assert.equal(events.at(-1).confirmed,false); // Socket/timeout is still not provider closure.
});

test('a spoken hangup promise, negative decision, refusal and malformed output never close the call',async()=>{
 const {bridge,sideband,events}=fixture({callControlGraceMs:10});await bridge.command({...create,native_call_control:true});
 sideband.emit('event',{type:'session.output_transcript.delta',event_id:'promise',delta:'Hanging up now.',start_ms:10,end_ms:20});
 const cases=[
  ['negative',endDecision('negative',.001)],
  ['uncertain',endDecision('uncertain',.97)],
  ['refusal',endDecision('refusal',.999,{event:{type:'decision.completed',decision:{answers:[{type:'refusal',name:'end_current_call'}]}}})],
  ['wrong-question',endDecision('wrong-question',.999,{event:{type:'decision.completed',decision:{answers:[{type:'predicate',name:'other',probability:1}]}}})],
  ['delegated',endDecision('delegated',1,{delegation_id:'backend-delegation'})],
  ['output',endDecision('output',1,{source_event_type:'session.output_transcript.delta'})],
  ['bad-probability',endDecision('bad-probability',NaN)],
 ];
 let time=100;
 for(const [id,decision] of cases){sideband.emit('event',inputEvent(id,'This fixture provides a semantic model answer.',time,time+10));sideband.emit('event',decision);time+=20;}
 await waitControl();assert.equal(events.some(v=>v.type==='call_control'),false);assert.equal(sideband.sent.length,0);
 await bridge.close();
});

test('new human speech revokes a pending signal and older decisions cannot end a continued conversation',async()=>{
 const {bridge,sideband,events}=fixture({callControlGraceMs:10});await bridge.command({...create,native_call_control:true});
 sideband.emit('event',inputEvent('old'));sideband.emit('event',endDecision('old'));
 sideband.emit('event',inputEvent('correction','Actually, stay on the call.',250,300));
 sideband.emit('event',endDecision('correction',.001));
 sideband.emit('event',endDecision('old',1,{event_id:'old-later',trigger_id:'old-later'}));
 await waitControl();assert.equal(events.some(v=>v.type==='call_control'),false);assert.equal(sideband.sent.length,0);
 await bridge.close();
});

test('out-of-order delivery binds the trigger to its exact source while stale and foreign sessions are ignored',async()=>{
 const {bridge,sideband,events}=fixture({callControlGraceMs:10});await bridge.command({...create,native_call_control:true});
 sideband.emit('event',{...inputEvent('foreign'),session_id:'live_other'});
 sideband.emit('event',{...endDecision('foreign'),session_id:'live_other'});
 sideband.emit('event',endDecision('now'));
 sideband.emit('event',inputEvent('now','Please hang up.',500,600));
 sideband.emit('event',inputEvent('earlier','Old fragment.',200,300));
 await waitControl();assert.equal(events.filter(v=>v.type==='call_control').length,1);
 assert.equal(events.find(v=>v.type==='call_control').source_event_id,'now');
 assert.equal(sideband.sent.filter(v=>v.type==='session.close').length,1);
});

test('old trigger results and changed duplicate source identity never authorize call control',async()=>{
 const first=fixture({callControlGraceMs:10,callControlFreshMs:10});await first.bridge.command({...create,native_call_control:true});
 first.sideband.emit('event',inputEvent('expired'));await waitControl();first.sideband.emit('event',endDecision('expired'));await waitControl();
 assert.equal(first.events.some(v=>v.type==='call_control'),false);await first.bridge.close();
 const second=fixture({callControlGraceMs:10});await second.bridge.command({...create,native_call_control:true});
 second.sideband.emit('event',inputEvent('changed'));second.sideband.emit('event',endDecision('changed'));
 second.sideband.emit('event',inputEvent('changed','Do not hang up.'));
 await waitControl();assert.equal(second.events.some(v=>v.type==='call_control'),false);await second.bridge.close();
});


test('native Decisions call control is an explicit optional capability, off by default',async()=>{
 assert.equal(sessionOptions(create).session.triggers,undefined);
 assert.equal(sessionOptions({...create,native_call_control:false}).session.triggers,undefined);
 for(const bad of [null,1,'true',{},[]])assert.throws(()=>sessionOptions({...create,native_call_control:bad}),/invalid_native_call_control/);
 const {bridge,sideband,events}=fixture({callControlGraceMs:10});await bridge.command(create);
 sideband.emit('event',inputEvent('disabled'));sideband.emit('event',endDecision('disabled'));await waitControl();
 assert.equal(events.some(e=>e.type==='call_control'),false);assert.equal(sideband.sent.length,0);
 await bridge.close();
});

test('empty thinking suppresses automatic acknowledgement without accepting empty speech',async()=>{
 const {bridge,sideband}=fixture();await bridge.command(create);
 const quiet={op:'append',channel:'thinking',event_id:'silent-1',delegation_id:'d1',content:''};
 await bridge.command(quiet);await bridge.command(quiet);
 assert.deepEqual(sideband.sent,[{type:'session.thinking.append',event_id:'silent-1',delegation_id:'d1',content:''}]);
 await assert.rejects(bridge.command({...quiet,event_id:'not-speech',channel:'commentary'}),/invalid_content/);
 await assert.rejects(bridge.command({...quiet,event_id:'not-blank',content:' '}),/invalid_content/);
 await bridge.close();
});

test('provider media closes while original official MCP listener serves saved receipts until disposal',async t=>{
 const {createServer}=await import('node:http');
 const {Client}=await import('@modelcontextprotocol/sdk/client/index.js');
 const {StreamableHTTPClientTransport}=await import('@modelcontextprotocol/sdk/client/streamableHttp.js');
 const seen=[];
 const api=createServer((req,res)=>{
  seen.push({method:req.method,url:req.url});
  res.writeHead(req.method==='GET'?200:403,{'Content-Type':'application/json'});
  res.end(JSON.stringify(req.method==='GET'?{kind:'call',state:'closed',provider_finalization:'confirmed'}:{error:'call_inactive'}));
 });
 await new Promise(resolve=>api.listen(0,'127.0.0.1',resolve));
 t.after(()=>new Promise(resolve=>{api.close(resolve);api.closeAllConnections();}));
 const token='fixture-private-call-grant-0123456789';
 const {bridge,events,sideband,requests}=fixture();
 t.after(()=>bridge.dispose());
 await bridge.command({...create,retain_receipts:true,mcp:{api_base:`http://127.0.0.1:${api.address().port}`,call_id:'e0f36409-a5a6-4c1a-9355-0cd5ed10ce10',token}});
 const endpoint=new URL(`http://127.0.0.1:${events.find(e=>e.type==='ready').mcp_port}/mcp`);
 const client=new Client({name:'retained-receipt-fixture',version:'1.0.0'});
 await client.connect(new StreamableHTTPClientTransport(endpoint,{requestInit:{headers:{Authorization:`Bearer ${token}`}}}));
 const closing=bridge.close();
 sideband.emit('event',{type:'session.closed',event_id:'closed-original',session:{id:'live_one'},reason:'close_requested',usage:{seconds:1}});
 await closing;
 assert.equal(bridge.state,'closed');assert.equal(bridge.disposed,false);
 assert.equal(events.at(-1).confirmed,true);
 const result=await client.callTool({name:'companion_surface_status',arguments:{}});
 assert.equal(result.structuredContent.provider_finalization,'confirmed');
 assert.deepEqual(seen,[{method:'GET',url:'/api/v1/companion/calls/e0f36409-a5a6-4c1a-9355-0cd5ed10ce10/tools/surface/current'}]);
 assert.equal(requests.length,1);
 await assert.rejects(bridge.command({op:'append',channel:'commentary',event_id:'late',delegation_id:null,content:'Live again'}),/call_not_active/);
 await client.close();await bridge.command({op:'dispose'});await bridge.transportDone;
 assert.equal(bridge.disposed,true);
 await assert.rejects(fetch(endpoint,{method:'POST',body:'{}'}));
});

test('native control diagnostics distinguish no-close decisions and stale sources without recording reasoning',async()=>{
 const {bridge,events,sideband}=fixture({callControlFreshMs:5});
 await bridge.command({...create,native_call_control:true});
 assert.ok(events.some(e=>e.event?.reason==='configured_enabled'));
 sideband.emit('event',inputEvent('negative'));
 const negative=endDecision('negative',.2);
 negative.event.decision.reasoning='private rationale';negative.event.decision.headers={authorization:'secret'};
 sideband.emit('event',negative);
 assert.ok(events.some(e=>e.event?.reason==='no_close_decision'&&e.event.probability===.2));
 sideband.emit('event',inputEvent('stale', 'Secret utterance', 200,300));
 await new Promise(resolve=>setTimeout(resolve,15));
 sideband.emit('event',endDecision('stale'));
 assert.ok(events.some(e=>e.event?.reason==='stale_source'));
 sideband.emit('event',endDecision('missing'));
 assert.ok(events.some(e=>e.event?.reason==='awaiting_source'));
 const diagnostics=events.filter(e=>e.event?.type==='companion.native_call_control.diagnostic');
 const saved=JSON.stringify(diagnostics);
 for(const value of ['private rationale','authorization','Secret utterance'])assert.equal(saved.includes(value),false);
 assert.equal(sideband.sent.length,0);
 for(let i=0;i<1100;i++)bridge.diagnostic('no_close_decision');
 assert.equal(events.filter(e=>e.event?.type==='companion.native_call_control.diagnostic').length,1025);
 assert.equal(events.at(-1).event.reason,'diagnostic_limit');
 await bridge.close();
});

test('session-wide interruption instructions use the supported SDK event and correlated receipt',async()=>{
 const {bridge,sideband,events}=fixture();await bridge.command(create);
 const append={op:'append',channel:'instructions',event_id:'floor-123-suppressed',delegation_id:null,content:'The caller has taken the floor. Stop speaking and listen. Accepted work is unchanged.'};
 await bridge.command(append);await bridge.command(append);
 assert.deepEqual(sideband.sent.filter(e=>e.type==='session.instructions.append'),[{type:'session.instructions.append',event_id:append.event_id,delegation_id:null,content:append.content}]);
 await assert.rejects(bridge.command({...append,event_id:'wrong-delegation',delegation_id:'d1'}),/invalid_append/);
 await assert.rejects(bridge.command({...append,content:'Changed instruction'}),/changed_command/);
 await assert.rejects(bridge.command({...append,event_id:'empty',content:''}),/invalid_content/);
 sideband.emit('event',{type:'session.instructions.appended',event_id:'ack-1',client_event_id:append.event_id,start_ms:100,end_ms:250});
 assert.deepEqual(events.find(e=>e.event?.type==='session.instructions.appended').event,{type:'session.instructions.appended',event_id:'ack-1',client_event_id:append.event_id,start_ms:100,end_ms:250});
 assert.equal(sideband.sent.some(e=>e.type==='response.cancel'||e.type==='session.input_audio.mute'),false);
 await bridge.close();
});
