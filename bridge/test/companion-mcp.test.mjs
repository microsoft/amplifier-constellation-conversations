import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {randomUUID} from 'node:crypto';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StreamableHTTPClientTransport} from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import {startCompanionMcp} from '../companion-mcp.mjs';

const token='fixture-scoped-token-never-public',callId=randomUUID();
async function fixture(t,handler) {
  const requests=[];
  const api=createServer(async(req,res)=>{
    let raw='';for await(const chunk of req)raw+=chunk;
    requests.push({path:req.url,method:req.method,authorization:req.headers.authorization,body:raw?JSON.parse(raw):null});
    if(handler)return handler(req,res,requests.at(-1));
    res.writeHead(200,{'Content-Type':'application/json'});
    res.end(JSON.stringify({observed:true,path:req.url,body:requests.at(-1).body}));
  });
  await new Promise(resolve=>api.listen(0,'127.0.0.1',resolve));
  const apiBase=`http://127.0.0.1:${api.address().port}`;
  const bridge=await startCompanionMcp({apiBase,callId,token});
  const url=new URL(`http://127.0.0.1:${bridge.port}/mcp`);
  const client=new Client({name:'companion-test',version:'1'});
  await client.connect(new StreamableHTTPClientTransport(url,{requestInit:{headers:{Authorization:`Bearer ${token}`}}}));
  t.after(async()=>{await client.close();await bridge.close();await new Promise(resolve=>{api.close(resolve);api.closeAllConnections();});});
  return {client,bridge,url,requests,apiBase};
}

test('official MCP client discovers only bounded tools and callbacks stay in this call',async t=>{
  const {client,requests}=await fixture(t);
  const {tools}=await client.listTools();
  assert.deepEqual(tools.map(v=>v.name).sort(),['companion_action_status','companion_connections','companion_request_action','companion_shared','companion_show']);
  const id=randomUUID();
  for(const [name,args] of [
    ['companion_connections',{}],
    ['companion_request_action',{connection_id:'minecraft:coach',delegation_id:'delegate-1',instruction:'Build the tower the human requested.\n',request_id:id}],
    ['companion_action_status',{request_id:id}],
    ['companion_show',{url:'https://example.com/video',title:'The demonstration',kind:'video'}],
    ['companion_shared',{}],
  ])assert.notEqual((await client.callTool({name,arguments:args})).isError,true);
  const root=`/api/v1/companion/calls/${callId}/tools/`;
  assert.deepEqual(requests.map(r=>[r.method,r.path]),[['GET',`${root}connections`],['POST',`${root}actions`],['GET',`${root}actions/${id}`],['POST',`${root}shared`],['GET',`${root}shared`]]);
  assert.equal(requests[1].body.request_id,id);
  assert.equal(requests[1].body.delegation_id,'delegate-1');
  assert.equal(requests[1].body.instruction,'Build the tower the human requested.\n');
  assert.equal(requests[3].body.url,'https://example.com/video');
  assert.ok(requests.every(r=>r.authorization===`Bearer ${token}`));
});

test('tool schemas cannot redirect callbacks, switch calls or expose credentials',async t=>{
  const {client,requests}=await fixture(t);
  for(const [name,args] of [
    ['companion_connections',{url:'https://attacker.invalid'}],
    ['companion_request_action',{connection_id:'game',delegation_id:'delegate-1',instruction:'Act',request_id:'../other'}],
    ['companion_request_action',{connection_id:'game',delegation_id:'delegate-1',instruction:'Act',request_id:randomUUID(),call_id:randomUUID()}],
    ['companion_request_action',{connection_id:'game',instruction:'Act',request_id:randomUUID()}],
    ['companion_request_action',{connection_id:'game',delegation_id:'',instruction:'Act',request_id:randomUUID()}],
    ['companion_request_action',{connection_id:'game',delegation_id:'x'.repeat(161),instruction:'Act',request_id:randomUUID()}],
    ['companion_request_action',{connection_id:'game',delegation_id:'delegate-1',instruction:'漢'.repeat(2667),request_id:randomUUID()}],
    ['companion_show',{url:'javascript:alert(1)',title:'Unsafe'}],
    ['companion_show',{url:'https://user:secret@example.com/',title:'Credential URL'}],
  ])assert.equal((await client.callTool({name,arguments:args})).isError,true);
  assert.equal(requests.length,0);
  const result=await client.callTool({name:'companion_show',arguments:{url:'https://example.com/image.png',title:'Image'}});
  assert.notEqual(result.isError,true);assert.equal(requests[0].body.kind,'auto');
});

test('listener rejects unauthenticated, browser-origin and oversized requests',async t=>{
  const {url,requests}=await fixture(t);
  assert.equal((await fetch(url,{method:'POST',body:'{}'})).status,401);
  assert.equal((await fetch(url,{method:'POST',headers:{Authorization:`Bearer ${token}`,Origin:'https://example.com'},body:'{}'})).status,403);
  assert.equal((await fetch(url,{method:'POST',headers:{Authorization:`Bearer ${token}`},body:'x'.repeat(65537)})).status,400);
  assert.equal((await fetch(new URL('/wrong',url),{method:'POST',headers:{Authorization:`Bearer ${token}`},body:'{}'})).status,404);
  assert.equal(requests.length,0);
});

test('uncertain effects are never retried and callback errors do not leak upstream content',async t=>{
  const {client,requests}=await fixture(t,(req,res)=>{res.destroy(new Error('upstream private credential'));});
  const result=await client.callTool({name:'companion_request_action',arguments:{connection_id:'game',delegation_id:'delegate-1',instruction:'An explicit human request',request_id:randomUUID()}});
  assert.equal(result.isError,true);assert.equal(result.structuredContent.outcome,'unknown');
  assert.equal(requests.length,1);
  assert.ok(!JSON.stringify(result).includes('private credential'));
  assert.ok(!JSON.stringify(result).includes(token));
});

test('redirects cannot carry the call token to another endpoint',async t=>{
  const {client,requests}=await fixture(t,(req,res)=>{res.writeHead(307,{Location:'/credential-sink'});res.end();});
  const result=await client.callTool({name:'companion_connections',arguments:{}});
  assert.equal(result.isError,true);assert.equal(requests.length,1);
});

test('host callback responses are bounded and shutdown closes the listener',async t=>{
  const {client,bridge,url}=await fixture(t,(req,res)=>{res.end(JSON.stringify({oversize:'x'.repeat(256*1024)}));});
  assert.equal((await client.callTool({name:'companion_shared',arguments:{}})).isError,true);
  await bridge.close();await bridge.close();
  await assert.rejects(fetch(url,{method:'POST',headers:{Authorization:`Bearer ${token}`},body:'{}'}));
});

test('configuration rejects unsafe origins, malformed call identity and weak/control tokens',async()=>{
  for(const patch of [{apiBase:'http://localhost:1234'},{apiBase:'https://user:secret@example.com'},{apiBase:'https://example.com/path'},{apiBase:'https://example.com/?next=secret'},{callId:'other/call'},{token:'short'},{token:'long-enough-token\nheader'},{port:-1}]) {
    await assert.rejects(startCompanionMcp({apiBase:'https://example.com',callId,token,...patch}));
  }
});
