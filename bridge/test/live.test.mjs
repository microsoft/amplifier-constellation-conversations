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
