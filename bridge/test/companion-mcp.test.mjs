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
  assert.deepEqual(tools.map(v=>v.name).sort(),['companion_action_status','companion_capture','companion_connection_status','companion_connections','companion_feedback','companion_hangup','companion_huddles','companion_huddle_ensure','companion_huddle_read','companion_huddle_post','companion_huddle_status','companion_observations','companion_request_action','companion_shared','companion_show','companion_surface_status','companion_view'].sort());
  const id=randomUUID();
  for(const [name,args] of [
    ['companion_connections',{}],
    ['companion_request_action',{connection_id:'minecraft:coach',delegation_id:'delegate-1',instruction:'Build the tower the human requested.\n',request_id:id}],
    ['companion_action_status',{request_id:id}],
    ['companion_show',{url:'https://example.com/video',title:'The demonstration',kind:'video'}],
    ['companion_shared',{}],
    ['companion_view',{operation_id:id,delegation_id:'delegate-1',action:'focus_card',card_id:'123'}],
    ['companion_capture',{operation_id:id,delegation_id:'delegate-1'}],
    ['companion_surface_status',{operation_id:id}],
  ])assert.notEqual((await client.callTool({name,arguments:args})).isError,true);
  const root=`/api/v1/companion/calls/${callId}/tools/`;
  assert.deepEqual(requests.map(r=>[r.method,r.path]),[['GET',`${root}connections`],['POST',`${root}actions`],['GET',`${root}actions/${id}`],['POST',`${root}shared`],['GET',`${root}shared`],['POST',`${root}view`],['POST',`${root}capture`],['GET',`${root}surface/${id}`]]);
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
    ['companion_view',{operation_id:randomUUID(),delegation_id:'delegate-1',action:'open_url',url:'https://example.com'}],
    ['companion_view',{operation_id:randomUUID(),delegation_id:'delegate-1',action:'focus_card',card_id:'#arbitrary-selector'}],
    ['companion_view',{operation_id:randomUUID(),delegation_id:'delegate-1',action:'focus_card'}],
    ['companion_view',{operation_id:randomUUID(),delegation_id:'delegate-1',action:'scroll_up',card_id:'1'}],
    ['companion_capture',{operation_id:randomUUID(),delegation_id:'delegate-1',enable_camera:true}],
    ['companion_capture',{operation_id:randomUUID(),delegation_id:'delegate-1',count:2}],
    ['companion_capture',{operation_id:randomUUID()}],
    ['companion_surface_status',{operation_id:'../other'}],
    ['companion_shared',{awareness_id:'../other-call'}],
    ['companion_shared',{awareness_id:'a'.repeat(63)}],
    ['companion_shared',{awareness_id:'a'.repeat(65)}],
    ['companion_shared',{awareness_id:'g'.repeat(64)}],
    ['companion_shared',{awareness_id:null}],
    ['companion_shared',{awareness_id:'a'.repeat(64),call_id:randomUUID()}],
    ['companion_shared',{awareness_id:'a'.repeat(64),url:'https://attacker.invalid'}],
  ])assert.equal((await client.callTool({name,arguments:args})).isError,true);
  assert.equal(requests.length,0);
  const result=await client.callTool({name:'companion_show',arguments:{url:'https://example.com/image.png',title:'Image'}});
  assert.notEqual(result.isError,true);assert.equal(requests[0].body.kind,'auto');
});

test('official MCP client retrieves one exact older awareness card through a fixed scoped query',async t=>{
  const awareness_id='1a'.repeat(32);
  const {client,requests}=await fixture(t,(_req,res)=>{
    res.writeHead(200,{'Content-Type':'application/json'});
    res.end(JSON.stringify({items:[],shown:[{awareness_id,card_id:'1',text:'The final check failed; keep this qualifier.'}]}));
  });
  const response=await client.callTool({name:'companion_shared',arguments:{awareness_id}});
  assert.notEqual(response.isError,true);
  assert.equal(requests.length,1);
  assert.equal(requests[0].path,`/api/v1/companion/calls/${callId}/tools/shared?awareness_id=${awareness_id}`);
  assert.equal(requests[0].method,'GET');assert.equal(requests[0].body,null);
  assert.equal(requests[0].authorization,`Bearer ${token}`);
  assert.equal(response.structuredContent.shown[0].awareness_id,awareness_id);
  assert.equal(response.structuredContent.shown[0].text,'The final check failed; keep this qualifier.');
});

test('missing exact awareness card stays missing without substituting recent cards or retrying',async t=>{
  const {client,requests}=await fixture(t,(_req,res)=>{res.writeHead(404,{'Content-Type':'application/json'});res.end(JSON.stringify({error:'Not found'}));});
  const response=await client.callTool({name:'companion_shared',arguments:{awareness_id:'b'.repeat(64)}});
  assert.equal(response.isError,true);assert.equal(requests.length,1);
  assert.equal(response.structuredContent.status,404);
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

test('saved camera evidence uses official MCP image content without duplicating bytes into text',async t=>{
  const png='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3XcAAAAASUVORK5CYII=';
  const operation=randomUUID(),artifact=randomUUID();
  const {client,requests}=await fixture(t,(req,res)=>res.end(JSON.stringify({operation_id:operation,kind:'capture',state:'uploaded',artifact_ids:[artifact],inspection:false,image_input:true,image:{mimeType:'image/png',data:png}})));
  const result=await client.callTool({name:'companion_surface_status',arguments:{operation_id:operation}});
  assert.notEqual(result.isError,true);assert.equal(result.content[1].type,'image');assert.equal(result.content[1].data,png);assert.equal(result.content[1].mimeType,'image/png');
  assert.equal(result.structuredContent.state,'uploaded');assert.equal(result.structuredContent.inspection,false);assert.equal(result.structuredContent.image,undefined);
  assert.ok(!result.content[0].text.includes(png));assert.equal(requests.length,1);
});

test('capture transport uncertainty never repeats the request and directs same-operation status lookup',async t=>{
  const {client,requests}=await fixture(t,(req,res)=>res.destroy());
  const result=await client.callTool({name:'companion_capture',arguments:{operation_id:randomUUID(),delegation_id:'delegate-1'}});
  assert.equal(result.isError,true);assert.equal(result.structuredContent.outcome,'unknown');assert.match(result.structuredContent.guidance,/companion_surface_status/);assert.equal(requests.length,1);
});

test('camera prerequisites are explicit no-effect results and native Allow is never presented as browser consent',async t=>{
  const operation=randomUUID();let reason='capture_consent_required';
  const {client,requests}=await fixture(t,(_req,res)=>res.end(JSON.stringify({operation_id:operation,delegation_id:'delegate-1',kind:'capture',state:'blocked',reason,effect_attempted:false,artifact_ids:[],inspection:false,private_detail:'hidden helper credentials',guidance:'arbitrary upstream instruction'})));
  for(const expected of ['capture_consent_required','camera_disabled','browser_unavailable']) {
    reason=expected;
    const result=await client.callTool({name:'companion_capture',arguments:{operation_id:operation,delegation_id:'delegate-1'}});
    assert.notEqual(result.isError,true);assert.equal(result.structuredContent.state,'blocked');
    assert.equal(result.structuredContent.reason,expected);assert.equal(result.structuredContent.effect_attempted,false);
    assert.deepEqual(result.structuredContent.artifact_ids,[]);assert.equal(result.structuredContent.inspection,false);
    assert.ok(!JSON.stringify(result).includes('hidden helper credentials'));assert.ok(!JSON.stringify(result).includes('arbitrary upstream instruction'));
    if(expected==='capture_consent_required')assert.match(result.structuredContent.guidance,/Allow choice cannot enable/);
  }
  assert.equal(requests.length,3);assert.ok(requests.every(r=>r.body.operation_id===operation));
});

test('fresh connection status has one encoded fixed read-only route and rejects target or command substitutions',async t=>{
  const {client,requests}=await fixture(t);
  const result=await client.callTool({name:'companion_connection_status',arguments:{connection_id:'workshop:coach / status'}});
  assert.notEqual(result.isError,true);assert.equal(requests.length,1);assert.equal(requests[0].method,'GET');
  assert.equal(requests[0].path,`/api/v1/companion/calls/${callId}/tools/connection-status?connection_id=workshop%3Acoach%20%2F%20status`);
  for(const args of [{connection_id:'x',command:'host-command'},{connection_id:'x',url:'https://other.test'},{connection_id:''},{connection_id:'漢'.repeat(41)},{connection_id:'a\nb'}]) {
    assert.equal((await client.callTool({name:'companion_connection_status',arguments:args})).isError,true);
  }
  assert.equal(requests.length,1);
});

test('unsaved or malformed capture media is never presented as image evidence',async t=>{
  const {client}=await fixture(t,(req,res)=>res.end(JSON.stringify({kind:'capture',state:'executed',artifact_ids:[],inspection:false,image:{mimeType:'image/png',data:'not-image'}})));
  const result=await client.callTool({name:'companion_surface_status',arguments:{operation_id:randomUUID()}});
  assert.equal(result.isError,true);assert.ok(!result.content.some(v=>v.type==='image'));
});

test('unsupported ACP image input retains the saved artifact without image bytes or fallback',async t=>{
  const artifact=randomUUID(),receipt={kind:'capture',state:'uploaded',artifact_ids:[artifact],inspection:false,image_input:false,image_delivery:'unsupported'};
  const {client,requests}=await fixture(t,(req,res)=>res.end(JSON.stringify(receipt)));
  const result=await client.callTool({name:'companion_surface_status',arguments:{operation_id:randomUUID()}});
  assert.notEqual(result.isError,true);assert.deepEqual(result.structuredContent.artifact_ids,[artifact]);assert.equal(result.structuredContent.image_delivery,'unsupported');
  assert.ok(result.content.every(v=>v.type==='text'));assert.equal(requests.length,1);
  receipt.image={mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3XcAAAAASUVORK5CYII='};
  const malformed=await client.callTool({name:'companion_surface_status',arguments:{operation_id:randomUUID()}});
  assert.equal(malformed.isError,true);assert.ok(malformed.content.every(v=>v.type==='text'));assert.equal(requests.length,2);
});

test('observations hangup and feedback use fixed own-call routes without arbitrary targets',async t=>{
 const {client,requests}=await fixture(t);const id=randomUUID();
 for(const [name,args] of [
  ['companion_observations',{}],
  ['companion_hangup',{operation_id:id,delegation_id:'delegate-1'}],
  ['companion_feedback',{operation_id:id,delegation_id:'delegate-1',text:'Please play a shutter sound.'}],
 ])assert.notEqual((await client.callTool({name,arguments:args})).isError,true);
 const root=`/api/v1/companion/calls/${callId}/tools/`;
 assert.deepEqual(requests.map(r=>[r.method,r.path]),[['GET',root+'observations'],['POST',root+'hangup'],['POST',root+'feedback']]);
 const count=requests.length;
 for(const [name,args] of [
  ['companion_observations',{agent_id:'other'}],
  ['companion_hangup',{operation_id:id,delegation_id:'delegate-1',call_id:randomUUID()}],
  ['companion_hangup',{operation_id:id,delegation_id:'delegate-1',cancel_work:true}],
  ['companion_feedback',{operation_id:id,delegation_id:'delegate-1',text:'hello',recipient:'external'}],
  ['companion_feedback',{operation_id:id,delegation_id:'delegate-1',text:'漢'.repeat(1334)}],
 ])assert.equal((await client.callTool({name,arguments:args})).isError,true);
 assert.equal(requests.length,count);
});

test('surface state and existing shared-image lookup reuse one scoped read tool',async t=>{
 const {client,requests}=await fixture(t);const artifact=randomUUID();
 assert.notEqual((await client.callTool({name:'companion_surface_status',arguments:{}})).isError,true);
 assert.notEqual((await client.callTool({name:'companion_surface_status',arguments:{artifact_id:artifact}})).isError,true);
 assert.equal(requests[0].path,`/api/v1/companion/calls/${callId}/tools/surface/current`);
 assert.equal(requests[1].path,`/api/v1/companion/calls/${callId}/tools/surface/attachment:${artifact}`);
 for(const args of [{artifact_id:'../private'},{artifact_id:artifact,operation_id:randomUUID()},{artifact_id:artifact,call_id:randomUUID()}]) {
  assert.equal((await client.callTool({name:'companion_surface_status',arguments:args})).isError,true);
 }
 assert.equal(requests.length,2);assert.ok(requests.every(r=>r.method==='GET'));
});

test('whiteboard operations are limited to three exact saved IDs and cannot change other surfaces',async t=>{
 const {client,requests}=await fixture(t);const upload=`upload:${randomUUID()}`;
 for(const extra of [{action:'replace_board',card_ids:['1',upload]},{action:'add_card',card_id:'2'},{action:'remove_card',card_id:upload},{action:'clear_board'}]) {
  assert.notEqual((await client.callTool({name:'companion_view',arguments:{operation_id:randomUUID(),delegation_id:'delegate-1',...extra}})).isError,true);
 }
 for(const extra of [{action:'replace_board',card_ids:[]},{action:'replace_board',card_ids:['1','1']},{action:'replace_board',card_ids:['1','2','3','4']},{action:'replace_board',card_ids:['#selector']},{action:'add_card',card_ids:['1']},{action:'clear_board',card_id:'1'},{action:'remove_card',card_id:'upload:../../secret'}]) {
  assert.equal((await client.callTool({name:'companion_view',arguments:{operation_id:randomUUID(),delegation_id:'delegate-1',...extra}})).isError,true);
 }
 assert.equal(requests.length,4);assert.ok(requests.every(r=>r.path===`/api/v1/companion/calls/${callId}/tools/view`));
});

test('saved manually shared images are actual MCP image content without a camera operation',async t=>{
 const png='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3XcAAAAASUVORK5CYII=';
 const artifact=randomUUID();
 const {client,requests}=await fixture(t,(_req,res)=>res.end(JSON.stringify({kind:'attachment',state:'available',artifact_ids:[artifact],inspection:false,image_input:true,image:{mimeType:'image/png',data:png}})));
 const result=await client.callTool({name:'companion_surface_status',arguments:{artifact_id:artifact}});
 assert.notEqual(result.isError,true);assert.equal(result.content[1].type,'image');assert.equal(result.content[1].data,png);
 assert.equal(result.structuredContent.image,undefined);assert.equal(result.structuredContent.inspection,false);
 assert.equal(requests.length,1);assert.equal(requests[0].method,'GET');
});

test('one existing show tool can author a short plain note without URL or HTML authority',async t=>{
 const {client,requests}=await fixture(t);const text='One useful comparison.\n<script>literal text only</script>';
 assert.notEqual((await client.callTool({name:'companion_show',arguments:{title:'Compare',text}})).isError,true);
 assert.equal(requests[0].body.kind,'note');assert.equal(requests[0].body.text,text);assert.equal(requests[0].body.url,undefined);
 for(const args of [{title:'None'},{title:'Both',text:'note',url:'https://example.test'},{title:'Long',text:'漢'.repeat(401)},{title:'Blank',text:' '},{title:'Wrong kind',text:'Note',kind:'image'},{title:'Wrong kind',url:'https://example.test',kind:'note'},{title:'HTML',text:'plain',html:'<script/>'}])assert.equal((await client.callTool({name:'companion_show',arguments:args})).isError,true);
 assert.equal(requests.length,1);
});

test('saved result images use a bounded 2 MiB transport without increasing phone capture limits',async t=>{
 const artifact=randomUUID(),bytes=Buffer.alloc(2*1024*1024);Buffer.from([137,80,78,71,13,10,26,10]).copy(bytes);
 const receipt={kind:'attachment',state:'available',source:'agent_result',message_id:randomUUID(),artifact_ids:[artifact],inspection:false,image_input:true,image:{mimeType:'image/png',data:bytes.toString('base64')}};
 const {client,requests}=await fixture(t,(_req,res)=>res.end(JSON.stringify(receipt)));
 const read=()=>client.callTool({name:'companion_surface_status',arguments:{artifact_id:artifact}});
 let result=await read();assert.notEqual(result.isError,true);assert.equal(result.content[1].data,bytes.toString('base64'));assert.equal(result.structuredContent.source,'agent_result');assert.equal(result.structuredContent.inspection,false);assert.equal(result.structuredContent.image,undefined);
 receipt.image.data=Buffer.concat([bytes,Buffer.alloc(1)]).toString('base64');
 result=await read();assert.equal(result.isError,true);assert.ok(!result.content.some(v=>v.type==='image'));
 receipt.image.data=bytes.toString('base64').slice(0,-1);result=await read();assert.equal(result.isError,true);
 receipt.image.data=bytes.toString('base64');receipt.image.mimeType='text/plain';result=await read();assert.equal(result.isError,true);
 receipt.image.mimeType='image/png';receipt.kind='capture';receipt.state='uploaded';result=await read();assert.equal(result.isError,true,'camera output still has its own 512 KiB boundary');
 assert.equal(requests.length,5,'read failures never recapture or retry');
});

test('scoped observation and current status schemas keep exact connection IDs without broadening',async t=>{
  const {client,requests}=await fixture(t);
  const connection_id='world:Constellation / east';
  for(const [name,args] of [
    ['companion_observations',{connection_id}],
    ['companion_connection_status',{connection_id,view:'current'}],
    ['companion_connection_status',{connection_id,view:'reports'}],
  ])assert.notEqual((await client.callTool({name,arguments:args})).isError,true);
  const root=`/api/v1/companion/calls/${callId}/tools/`;
  assert.deepEqual(requests.map(r=>[r.method,r.path]),[
    ['GET',`${root}observations?connection_id=world%3AConstellation%20%2F%20east`],
    ['GET',`${root}connection-status?connection_id=world%3AConstellation%20%2F%20east&view=current`],
    ['GET',`${root}connection-status?connection_id=world%3AConstellation%20%2F%20east&view=reports`],
  ]);
  for(const [name,args] of [
    ['companion_observations',{connection_id:null}],
    ['companion_observations',{connection_id:''}],
    ['companion_observations',{connection_id:'a\nb'}],
    ['companion_observations',{connection_id:'漢'.repeat(41)}],
    ['companion_observations',{connection_id:['a','b']}],
    ['companion_observations',{connection_id,owner:'someone-else'}],
    ['companion_connection_status',{connection_id,view:null}],
    ['companion_connection_status',{connection_id,view:'latest'}],
    ['companion_connection_status',{connection_id,view:'current',command:'arbitrary'}],
  ])assert.equal((await client.callTool({name,arguments:args})).isError,true);
  assert.equal(requests.length,3);
  const {tools}=await client.listTools();
  assert.match(tools.find(x=>x.name==='companion_observations').description,/exact connection_id/);
  assert.match(tools.find(x=>x.name==='companion_connection_status').description,/view current/);
});

test('automatic huddle tools preserve exact installation/source selectors and fixed call callbacks',async t=>{
  const {client,requests}=await fixture(t);
  const installation_id='local:agent.v1-qa',huddle_id=randomUUID(),request_id=randomUUID(),delegation_id='delegate-current';
  const ensure={installation_id,request_id,delegation_id,title:'Compare two routes',purpose:'Check the caller’s actual request.',peer_installation_ids:['peer:one','peer:two']};
  const post={installation_id,huddle_id,request_id:randomUUID(),delegation_id,text:'The path is blocked; no completion is claimed.',target_agent_ids:['peer:one'],kind:'message'};
  const outcome={...post,request_id:randomUUID(),kind:'outcome',outcome:{kind:'blocked',evidence:[{label:'Original observation',reference:'fixture:observation-1'}]}};
  for(const [name,arguments_]of[
    ['companion_huddles',{}],['companion_huddles',{installation_id}],['companion_huddles',{installation_id,after:0,limit:50}],
    ['companion_huddle_ensure',ensure],['companion_huddle_read',{installation_id,huddle_id,after:4,limit:100}],
    ['companion_huddle_post',post],['companion_huddle_post',outcome],
    ['companion_huddle_status',{installation_id,request_id,operation:'ensure'}],
    ['companion_huddle_status',{installation_id,request_id:post.request_id,operation:'post',huddle_id}],
  ])assert.notEqual((await client.callTool({name,arguments:arguments_})).isError,true,name);
  const base=`/api/v1/companion/calls/${callId}/tools/`,query='installation_id=local%3Aagent.v1-qa';
  assert.deepEqual(requests.map(r=>[r.method,r.path]),[
    ['GET',base+'huddles'],['GET',base+'huddles?'+query],['GET',base+'huddles?'+query+'&after=0&limit=50'],
    ['POST',base+'huddles/ensure'],['GET',base+`huddles/${huddle_id}/messages?`+query+'&after=4&limit=100'],
    ['POST',base+`huddles/${huddle_id}/posts`],['POST',base+`huddles/${huddle_id}/posts`],
    ['GET',base+`huddles/ensure/${request_id}?`+query],['GET',base+`huddles/${huddle_id}/posts/${post.request_id}?`+query],
  ]);
  assert.deepEqual(requests[3].body,ensure);
  const {huddle_id:unused,...expectedPost}=post,{huddle_id:unusedOutcome,...expectedOutcome}=outcome;
  assert.deepEqual(requests[5].body,expectedPost);assert.deepEqual(requests[6].body,expectedOutcome);
  assert(requests.every(r=>r.authorization===`Bearer ${token}`));
  assert(requests.filter(r=>r.method==='GET').every(r=>r.body===null));
  const {tools}=await client.listTools();
  for(const name of ['companion_huddles','companion_huddle_read','companion_huddle_status'])assert.equal(tools.find(v=>v.name===name).annotations.readOnlyHint,true);
  for(const name of ['companion_huddle_ensure','companion_huddle_post'])assert.equal(tools.find(v=>v.name===name).annotations.readOnlyHint,false);
  assert.match(tools.find(v=>v.name==='companion_huddle_status').description,/Ensure status needs no room ID/);
  assert.match(tools.find(v=>v.name==='companion_huddle_ensure').description,/does not start a second agent session/);
});

test('automatic huddle schemas reject ambiguous discovery, unsafe IDs and model-authored authority before HTTP',async t=>{
  const {client,requests}=await fixture(t),installation_id='local',huddle_id=randomUUID(),request_id=randomUUID();
  const ensure={installation_id,request_id,delegation_id:'delegate-current',title:'Plan',purpose:'Compare the two plans',peer_installation_ids:['peer']};
  const post={installation_id,huddle_id,request_id,delegation_id:'delegate-current',text:'Routine detail',target_agent_ids:[],kind:'message'};
  const invalid=[
    ...[{after:0},{limit:1},{installation_id:null},{installation_id:''},{installation_id:'local/other'},{installation_id:'local?owner=other'},{installation_id:'a\nb'},{installation_id:'café'},{installation_id:'a'.repeat(201)},{installation_id,after:-1},{installation_id,after:.5},{installation_id,after:Number.MAX_SAFE_INTEGER+1},{installation_id,after:null},{installation_id,limit:0},{installation_id,limit:51},{installation_id,limit:null},{installation_id,token:'secret'}].map(v=>['companion_huddles',v]),
    ...[{peer_installation_ids:['peer','peer']},{peer_installation_ids:Array.from({length:8},(_,i)=>'peer'+i)},{peer_installation_ids:['bad/name']},{title:' '},{title:'a'.repeat(121)},{title:'😀'.repeat(121)},{purpose:'漢'.repeat(667)},{purpose:'hello\0world'},{source_message_id:randomUUID()},{origin:{actor:'human'}},{actor:'Brian'},{url:'https://attacker.invalid'},{callback:'https://attacker.invalid'},{call_id:randomUUID()},{token:'forged'},{owner:'someone-else'},{delegation_id:null},{delegation_id:'line\nbreak'},{delegation_id:'é'.repeat(81)},{request_id:'not-a-uuid'}].map(v=>['companion_huddle_ensure',{...ensure,...v}]),
    ...[{huddle_id:'../other'},{after:-1},{limit:101},{limit:null},{source:{}},{actor:'human'}].map(v=>['companion_huddle_read',{installation_id,huddle_id,...v}]),
    ...[{target_agent_ids:['peer','peer']},{target_agent_ids:Array.from({length:9},(_,i)=>'peer'+i)},{target_agent_ids:['x/y']},{text:'漢'.repeat(2001)},{text:' '},{text:'a\0b'},{kind:'outcome'},{outcome:{kind:'result',evidence:[]}},{kind:'outcome',outcome:null},{kind:'outcome',outcome:{kind:'complete',evidence:[]}},{kind:'outcome',outcome:{kind:'result',evidence:Array.from({length:9},()=>({label:'Source',reference:'fixture:1'}))}},{kind:'outcome',outcome:{kind:'result',evidence:[{label:'漢'.repeat(67),reference:'fixture:1'}]}},{kind:'outcome',outcome:{kind:'result',evidence:[{label:'Source',reference:'漢'.repeat(334)}]}},{kind:'outcome',outcome:{kind:'result',evidence:[{label:'Source',reference:'fixture:1',actor:'human'}]}},{kind:'outcome',outcome:{kind:'result',evidence:[],verified:true}},{source:{id:'forged'}},{actor:'Brian'},{origin:{}},{token:'forged'},{url:'https://attacker.invalid'}].map(v=>['companion_huddle_post',{...post,...v}]),
    ['companion_huddle_status',{installation_id,request_id,operation:'ensure',huddle_id}],
    ['companion_huddle_status',{installation_id,request_id,operation:'post'}],
    ['companion_huddle_status',{installation_id,request_id,operation:'post',huddle_id:null}],
    ['companion_huddle_status',{installation_id,request_id,operation:'other'}],
    ['companion_huddle_status',{installation_id,request_id,operation:'ensure',source:{}}],
  ];
  for(const[name,args]of invalid)assert.equal((await client.callTool({name,arguments:args})).isError,true,JSON.stringify({name,args}));
  assert.equal(requests.length,0,'Rejected schemas cannot make any host callback');
});

test('huddle UTF-8 boundaries preserve authored content and count title characters rather than UTF-16 units',async t=>{
  const {client,requests}=await fixture(t),installation_id='i'.repeat(200),huddle_id=randomUUID(),delegation_id='d';
  const title='😀'.repeat(120),purpose='é'.repeat(1000),text='漢'.repeat(2000);
  const ensure={installation_id,request_id:randomUUID(),delegation_id,title,purpose,peer_installation_ids:[]};
  assert.notEqual((await client.callTool({name:'companion_huddle_ensure',arguments:ensure})).isError,true);
  assert.deepEqual(requests[0].body,ensure);assert.equal(Buffer.byteLength(requests[0].body.title),480);
  assert.notEqual((await client.callTool({name:'companion_huddle_post',arguments:{installation_id,huddle_id,delegation_id,request_id:randomUUID(),text,target_agent_ids:[],kind:'outcome',outcome:{kind:'progress',evidence:[]}}})).isError,true);
  assert.equal(requests[1].body.text,text);assert.equal(Buffer.byteLength(requests[1].body.text),6000);
  assert.equal(requests.length,2);
});

test('huddle metadata cap counts serialized UTF-8 bytes while keeping the exact boundary',async t=>{
  const {client,requests}=await fixture(t);
  const outcome={kind:'result',evidence:Array.from({length:8},()=>({label:'é'.repeat(100),reference:'r'.repeat(750)}))};
  outcome.evidence[0].reference+='r'.repeat(8000-Buffer.byteLength(JSON.stringify(outcome)));
  assert.equal(Buffer.byteLength(JSON.stringify(outcome)),8000);
  const post={installation_id:'local',huddle_id:randomUUID(),request_id:randomUUID(),delegation_id:'é'.repeat(80),text:'Result',target_agent_ids:[],kind:'outcome',outcome};
  assert.notEqual((await client.callTool({name:'companion_huddle_post',arguments:post})).isError,true);
  assert.deepEqual(requests[0].body.outcome,outcome);
  outcome.evidence[1].reference+='r';
  assert.equal(Buffer.byteLength(JSON.stringify(outcome)),8001);
  assert.equal((await client.callTool({name:'companion_huddle_post',arguments:{...post,request_id:randomUUID()}})).isError,true);
  assert.equal((await client.callTool({name:'companion_huddle_post',arguments:{...post,request_id:randomUUID(),delegation_id:'bad\u0085id',outcome:{kind:'result',evidence:[]}}})).isError,true);
  assert.equal(requests.length,1,'Oversized metadata and control-bearing delegation do not reach the callback');
});

test('unknown ensure returns exact status arguments without a room and never retries the mutation',async t=>{
  const {client,requests}=await fixture(t,(req,res)=>{
    if(req.method==='POST')res.destroy();
    else{res.writeHead(404);res.end(JSON.stringify({error:'No saved receipt is available'}));}
  });
  const installation_id='local:one',request_id=randomUUID();
  const result=await client.callTool({name:'companion_huddle_ensure',arguments:{installation_id,request_id,delegation_id:'current',title:'Review',purpose:'Compare',peer_installation_ids:['peer']}});
  assert.equal(result.isError,true);assert.equal(result.structuredContent.outcome,'unknown');
  assert.deepEqual(result.structuredContent.recovery,{tool:'companion_huddle_status',arguments:{installation_id,request_id,operation:'ensure'}});
  assert.match(result.structuredContent.guidance,/Do not use a new request ID/);
  assert.equal(requests.length,1);assert.equal(requests[0].method,'POST');
  const status=await client.callTool({name:result.structuredContent.recovery.tool,arguments:result.structuredContent.recovery.arguments});
  assert.equal(status.isError,true);assert.equal(status.structuredContent.status,404);
  assert.equal(requests.length,2);assert.equal(requests[1].method,'GET');
  assert.equal(requests[1].path,`/api/v1/companion/calls/${callId}/tools/huddles/ensure/${request_id}?installation_id=local%3Aone`);
  assert(!JSON.stringify(result).includes(token));
});

test('post rejection and uncertain response carry original operation identities without repeating a callback',async t=>{
  let mode='rejected';
  const {client,requests}=await fixture(t,(_req,res)=>{
    if(mode==='rejected'){res.writeHead(403);res.end('upstream private token must never be surfaced');}
    else if(mode==='malformed'){res.end('{not valid json');}
    else if(mode==='oversize'){res.end(JSON.stringify({value:'x'.repeat(256*1024)}));}
    else{res.writeHead(307,{Location:'/credential-sink'});res.end();}
  });
  const installation_id='local',huddle_id=randomUUID();
  for(const next of ['rejected','malformed','oversize','redirect']){
    mode=next;const request_id=randomUUID();
    const result=await client.callTool({name:'companion_huddle_post',arguments:{installation_id,huddle_id,request_id,delegation_id:'d',text:'The check remains incomplete.',target_agent_ids:[],kind:'outcome',outcome:{kind:'blocked',evidence:[]}}});
    assert.equal(result.isError,true);
    assert.deepEqual(result.structuredContent.recovery,{tool:'companion_huddle_status',arguments:{installation_id,request_id,operation:'post',huddle_id}});
    assert.match(result.structuredContent.guidance,/same|exact original/);
    if(next==='rejected')assert.equal(result.structuredContent.status,403);else assert.equal(result.structuredContent.outcome,'unknown');
    assert(!JSON.stringify(result).includes('upstream private'));assert(!JSON.stringify(result).includes(token));
  }
  assert.equal(requests.length,4,'One callback per independent test intent; no retry/redirect');
  assert(requests.every(r=>r.path===`/api/v1/companion/calls/${callId}/tools/huddles/${huddle_id}/posts`));
});
