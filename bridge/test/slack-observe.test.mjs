import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {observeSlack,makeSlackObserverClient} from '../slack-observe.mjs';

const TOKEN='xoxb-fixture-observer-never-output',CHANNEL='C12345678',BOT='U12345678',TS='1794075000.123456';
async function fixture(t,reply){
  const home=await mkdtemp(join(tmpdir(),'companion-slack-observe-'));t.after(()=>rm(home,{recursive:true,force:true}));
  await mkdir(join(home,'secrets'));await writeFile(join(home,'secrets','slack.json'),JSON.stringify({bot_token:TOKEN}));
  const configPath=join(home,'observer.json');await writeFile(configPath,JSON.stringify({slack_home:home,connection_id:'slack:coach',channel_id:CHANNEL,expected_bot_user_id:BOT}));
  const request_id=randomUUID();const input={version:1,cursor:null,actions:[{request_id,receipt:{request_id,state:'completed',scope:'message_delivery',receipt:{channel:CHANNEL,ts:TS}}}]};
  const messages=[{type:'message',user:BOT,ts:TS,text:'Delivered message',client_msg_id:request_id}];
  const calls=[];
  const createClient=(token,deadline)=>makeSlackObserverClient(token,async(url,init)=>{
    const call={url:String(url),init,fields:Object.fromEntries(new URLSearchParams(init.body))};calls.push(call);
    if(reply)return reply(call,calls.length);
    return Response.json(call.url.endsWith('auth.test')?{ok:true,user_id:BOT}:{ok:true,messages,has_more:false});
  },deadline);
  return {home,configPath,input,messages,calls,createClient};
}

test('official Slack SDK reads only the fixed channel and confirms delivery, never work completion',async t=>{
  const f=await fixture(t);const result=await observeSlack(f,f.createClient);
  assert.deepEqual(f.calls.map(c=>c.url),['https://slack.com/api/auth.test','https://slack.com/api/conversations.history']);
  assert.equal(f.calls[1].fields.channel,CHANNEL);assert.equal(f.calls[1].fields.limit,'32');
  assert.ok(f.calls.every(c=>c.init.redirect==='error'));
  assert.equal(result.availability,'available');assert.equal(result.actions[0].state,'completed');
  assert.equal(result.actions[0].scope,'message_delivery');assert.equal(result.actions[0].work_outcome,'unknown');
  assert.equal(result.events[0].audience.channel_id,CHANNEL);assert.equal(result.events[0].provenance.authority,false);
  assert.ok(!JSON.stringify(result).includes(TOKEN));
});

test('old receipt outside the bounded channel window stays unknown without per-action reads',async t=>{
  const request_id=randomUUID();
  const f=await fixture(t,call=>Response.json(call.url.endsWith('auth.test')?{ok:true,user_id:BOT}:
    call.fields.oldest?{ok:true,messages:[{type:'message',user:BOT,ts:TS,text:'Old delivery',client_msg_id:request_id}]}:{ok:true,messages:[],has_more:true}));
  f.input.actions[0].request_id=request_id;f.input.actions[0].receipt.request_id=request_id;
  const result=await observeSlack(f,f.createClient);
  assert.equal(result.actions[0].state,'unknown');assert.equal(f.calls.length,2);
  assert.equal(result.coverage.older_messages_omitted,true);
});

test('identity mismatch prevents history reads and no retry follows a network or rate-limit error',async t=>{
  for(const reply of [()=>Response.json({ok:true,user_id:'U87654321'}),()=>{throw Error(TOKEN);},()=>new Response('',{status:429,headers:{'Retry-After':'60'}})]){
    const f=await fixture(t,reply),result=await observeSlack(f,f.createClient);
    assert.equal(result.availability,'unavailable');assert.equal(f.calls.length,1);assert.equal(result.actions[0].state,'unknown');
    assert.ok(!JSON.stringify(result).includes(TOKEN));
  }
});

test('another author, missing message, mismatched client ID and changed channel never confirm the action',async t=>{
  for(const patch of [{user:'U87654321'},{client_msg_id:randomUUID()},{type:'tombstone'}]){
    const f=await fixture(t);Object.assign(f.messages[0],patch);
    assert.equal((await observeSlack(f,f.createClient)).actions[0].state,'unknown');
  }
  const f=await fixture(t);f.input.actions[0].receipt.receipt.channel='C87654321';
  assert.equal((await observeSlack(f,f.createClient)).actions[0].state,'unknown');assert.equal(f.calls.length,2);
  const absent=await fixture(t);absent.messages.length=0;
  assert.equal((await observeSlack(absent,absent.createClient)).actions[0].state,'unknown');assert.equal(absent.calls.length,2);
});

test('cursor deduplicates unchanged messages and detects edits without inventing new authority',async t=>{
  const f=await fixture(t),first=await observeSlack(f,f.createClient);
  f.input.cursor=first.cursor;
  assert.equal((await observeSlack(f,f.createClient)).events.length,0);
  f.messages[0].text='Edited message';f.messages[0].edited={ts:'1794075100.123456',user:BOT};
  const edited=await observeSlack(f,f.createClient);assert.equal(edited.events.length,1);
  assert.notEqual(edited.events[0].revision,first.events[0].revision);assert.equal(edited.events[0].provenance.authority,false);
});

test('stdin cannot select destination or credentials and response/output/text limits are enforced',async t=>{
  const f=await fixture(t);
  for(const patch of [{channel_id:'C87654321'},{token:TOKEN},{api_url:'https://attacker.invalid'},{actions:Array(5).fill(f.input.actions[0])}]){
    assert.equal((await observeSlack({...f,input:{...f.input,...patch}},f.createClient)).availability,'unavailable');
  }
  assert.equal(f.calls.length,0);
  f.messages.length=0;
  for(let i=0;i<32;i++)f.messages.push({type:'message',user:BOT,ts:`1794075${String(i).padStart(3,'0')}.123456`,text:'漢'.repeat(1500)});
  const result=await observeSlack(f,f.createClient);
  assert.ok(Buffer.byteLength(JSON.stringify(result))<=131072);assert.ok(result.events.every(e=>Buffer.byteLength(e.text)<=4000));
  assert.equal(result.coverage.gap,true);
  assert.equal(result.coverage.omitted_events,32);assert.equal(result.events.length,0,'Oversize source messages must be omitted whole, never clipped');
  const huge=await fixture(t,()=>new Response('x'.repeat(524289)));
  assert.equal((await observeSlack(huge,huge.createClient)).availability,'unavailable');assert.equal(huge.calls.length,1);
});

test('unavailable reads preserve the previous cursor and omit upstream error text',async t=>{
  const f=await fixture(t,()=>{throw Error(TOKEN);});
  f.input.cursor={seen:[{id:'slack:previous',revision:'a'.repeat(64)}]};
  const result=await observeSlack(f,f.createClient);
  assert.deepEqual(result.cursor,f.input.cursor);assert.ok(!JSON.stringify(result).includes(TOKEN));
});

test('action semantic revisions omit observation clock and SDK requests share one deadline',async t=>{
  const f=await fixture(t);
  const first=await observeSlack(f,f.createClient,'2026-11-07T20:00:00.000Z');
  const second=await observeSlack(f,f.createClient,'2026-11-07T20:01:00.000Z');
  assert.notEqual(first.actions[0].observed_at,second.actions[0].observed_at);
  assert.equal(first.actions[0].revision,second.actions[0].revision);
  assert.ok(f.calls.every(c=>c.init.signal instanceof AbortSignal));
  const stop=new AbortController();stop.abort();
  let called=false;
  const client=makeSlackObserverClient(TOKEN,async(_url,init)=>{called=true;init.signal.throwIfAborted();return Response.json({ok:true,user_id:BOT});},stop.signal);
  await assert.rejects(client.auth.test());assert.equal(called,true);
});
