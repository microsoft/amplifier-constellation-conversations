import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {randomUUID} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {executeSlackAction,makeSlackClient} from '../slack-action.mjs';

const TOKEN='xoxb-fixture-secret-never-output';
async function fixture(t,reply) {
  const home=await mkdtemp(join(tmpdir(),'companion-slack-'));t.after(()=>rm(home,{recursive:true,force:true}));
  await mkdir(join(home,'secrets'));await writeFile(join(home,'secrets','slack.json'),JSON.stringify({app_token:'unused-private-app-token',bot_token:TOKEN}));
  const configPath=join(home,'action.json');
  const config={slack_home:home,channel_id:'C12345678',expected_bot_user_id:'U12345678'};
  await writeFile(configPath,JSON.stringify(config));
  const input={version:1,request_id:randomUUID(),instruction:'The exact human-authorized message.',source:{actor:'owner',original_text:'Private source context must stay in the host ledger.'}};
  const calls=[];
  const createClient=token=>makeSlackClient(token,async(url,init)=>{
    const call={url:String(url),init,fields:Object.fromEntries(new URLSearchParams(init.body))};calls.push(call);
    if(reply)return reply(call,calls.length);
    return Response.json(call.url.endsWith('auth.test')?{ok:true,user_id:config.expected_bot_user_id}:{ok:true,channel:config.channel_id,ts:'1794075000.123456',message:{user:config.expected_bot_user_id}});
  });
  return {home,config,configPath,input,calls,createClient};
}

test('official Slack SDK checks identity before exact fixed-channel delivery and returns only receipt',async t=>{
  const f=await fixture(t);const result=await executeSlackAction(f,f.createClient);
  assert.deepEqual(f.calls.map(c=>c.url),['https://slack.com/api/auth.test','https://slack.com/api/chat.postMessage']);
  const post=f.calls[1];assert.equal(post.fields.channel,f.config.channel_id);assert.equal(post.fields.text,f.input.instruction);
  assert.equal(post.fields.client_msg_id,f.input.request_id);assert.equal(post.fields.unfurl_links,'false');assert.equal(post.fields.unfurl_media,'false');
  assert.ok(f.calls.every(c=>c.init.redirect==='error'));
  assert.ok(!JSON.stringify(post.fields).includes('Private source context'));
  assert.deepEqual(result,{request_id:f.input.request_id,state:'completed',scope:'message_delivery',receipt:{channel:f.config.channel_id,ts:'1794075000.123456'}});
  assert.ok(!JSON.stringify(result).includes(TOKEN));
});

test('identity mismatch or preflight network failure never attempts a post',async t=>{
  for(const reply of [()=>Response.json({ok:true,user_id:'U87654321'}),()=>{throw new Error(`secret ${TOKEN}`);}]) {
    const f=await fixture(t,reply);const result=await executeSlackAction(f,f.createClient);
    assert.equal(result.state,'failed');assert.equal(f.calls.length,1);assert.ok(!JSON.stringify(result).includes(TOKEN));
  }
});

test('rate limits, lost acknowledgements and malformed receipts remain unknown without retry',async t=>{
  const replies=[
    ()=>new Response('',{status:429,headers:{'Retry-After':'120'}}),
    ()=>{throw new Error(`post possibly accepted: ${TOKEN}`);},
    ()=>Response.json({ok:true,channel:'C87654321',ts:'1794075000.123456'}),
    ()=>Response.json({ok:true,channel:'C12345678'}),
    ()=>Response.json({ok:false,error:'missing_scope'}),
  ];
  for(const postReply of replies) {
    const f=await fixture(t,(call)=>call.url.endsWith('auth.test')?Response.json({ok:true,user_id:'U12345678'}):postReply());
    const result=await executeSlackAction(f,f.createClient);
    assert.equal(result.state,'unknown');assert.equal(result.request_id,f.input.request_id);assert.equal(f.calls.length,2);
    assert.ok(!JSON.stringify(result).includes(TOKEN));
  }
});

test('stdin cannot choose a channel, token, method or API origin and UTF8 input is bounded',async t=>{
  const f=await fixture(t);
  for(const patch of [{channel:'C87654321'},{token:TOKEN},{method:'chat.delete'},{slackApiUrl:'https://attacker.invalid'},{request_id:'not-a-uuid'},{instruction:'漢'.repeat(3000)},{source:{actor:''}}]) {
    assert.equal((await executeSlackAction({...f,input:{...f.input,...patch}},f.createClient)).state,'failed');
  }
  assert.equal(f.calls.length,0);
});

test('owner config requires an absolute connector home and exact fixed destination fields',async t=>{
  const f=await fixture(t);
  for(const patch of [{slack_home:'relative'},{channel_id:'@someone'},{channel_id:'D12345678'},{expected_bot_user_id:'B12345678'},{api_url:'https://attacker.invalid'}]) {
    await writeFile(f.configPath,JSON.stringify({...f.config,...patch}));
    assert.equal((await executeSlackAction(f,f.createClient)).state,'failed');
  }
  assert.equal((await executeSlackAction({...f,configPath:'relative'},f.createClient)).state,'failed');assert.equal(f.calls.length,0);
});

test('instruction has an independent 8000 UTF8 byte limit inside a 65536 byte source envelope',async t=>{
  const f=await fixture(t);
  const instruction='漢'.repeat(2666)+'aa';
  assert.equal(Buffer.byteLength(instruction),8000);
  const input={...f.input,instruction,source:{...f.input.source,padding:''}};
  input.source.padding='x'.repeat(65536-Buffer.byteLength(JSON.stringify(input)));
  assert.equal(Buffer.byteLength(JSON.stringify(input)),65536);
  assert.equal((await executeSlackAction({...f,input},f.createClient)).state,'completed');
  assert.equal(f.calls[1].fields.text,instruction);
  assert.equal(f.calls.length,2);
  assert.equal((await executeSlackAction({...f,input:{...input,source:{...input.source,padding:input.source.padding+'x'}}},f.createClient)).state,'failed');
  assert.equal((await executeSlackAction({...f,input:{...f.input,instruction:instruction+'a'}},f.createClient)).state,'failed');
  assert.equal(f.calls.length,2);
});

test('CLI bounds the whole raw stdin envelope independently of instruction size',async t=>{
  const f=await fixture(t);const helper=fileURLToPath(new URL('../slack-action.mjs',import.meta.url));
  const input={...f.input,source:{...f.input.source,padding:''}};
  input.source.padding='x'.repeat(65536-Buffer.byteLength(JSON.stringify(input)));
  const options={encoding:'utf8',timeout:5000};
  const accepted=spawnSync(process.execPath,[helper,'--config',join(f.home,'missing.json')],{...options,input:JSON.stringify(input)});
  assert.equal(accepted.status,0);assert.equal(accepted.stderr,'');
  assert.equal(JSON.parse(accepted.stdout).error,'slack_preflight_failed');
  const rejected=spawnSync(process.execPath,[helper,'--config',join(f.home,'missing.json')],{...options,input:JSON.stringify(input)+' '});
  assert.equal(rejected.status,0);assert.equal(rejected.stderr,'');
  assert.deepEqual(JSON.parse(rejected.stdout),{request_id:null,state:'failed',error:'invalid_request'});
});

test('CLI invalid configuration emits one sanitized matching receipt and no SDK logs',async t=>{
  const f=await fixture(t);const helper=fileURLToPath(new URL('../slack-action.mjs',import.meta.url));
  const result=spawnSync(process.execPath,[helper,'--config',join(f.home,'missing.json')],{input:JSON.stringify(f.input),encoding:'utf8',timeout:5000});
  assert.equal(result.status,0);assert.equal(result.stderr,'');
  assert.deepEqual(JSON.parse(result.stdout),{request_id:f.input.request_id,state:'failed',error:'slack_preflight_failed'});
  assert.equal(result.stdout.trim().split('\n').length,1);
});
