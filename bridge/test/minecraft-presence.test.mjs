import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {McpServer} from '@modelcontextprotocol/sdk/server/mcp.js';
import {StreamableHTTPServerTransport} from '@modelcontextprotocol/sdk/server/streamableHttp.js';
import {z} from 'zod';
import {observePresence} from '../minecraft-presence.mjs';

const token = 'fixture-private-presence-token';
async function fixture(t, mode='normal') {
  const calls = [];
  const http = createServer(async(req,res) => {
    if (mode === 'redirect') {
      res.writeHead(302, {Location:'http://127.0.0.1:1/stolen'}); res.end(); return;
    }
    assert.equal(req.url, '/presence/mcp');
    assert.equal(req.headers.authorization, `Bearer ${token}`);
    if (req.method !== 'POST') {res.writeHead(405); res.end(); return;}
    let raw=''; for await (const part of req) raw+=part;
    const request = JSON.parse(raw);
    const server = new McpServer({name:'minecraft-presence-fixture',version:'1'});
    const reply = value => ({content:[{type:'text',text:JSON.stringify(value)}]});
    server.registerTool('minecraft_status',{inputSchema:{},annotations:{readOnlyHint:true}}, async () => {
      calls.push('minecraft_status');
      if (mode === 'stall') await new Promise(resolve => setTimeout(resolve, 200));
      if (mode === 'error') return {isError:true,content:[{type:'text',text:token}]};
      return reply({username:mode === 'wrong_identity' ? 'Other' : 'Coach',observed_at:'2026-11-08T21:00:00Z',position:{x:1,y:64,z:2}});
    });
    server.registerTool('minecraft_recent_activity',{inputSchema:{limit:z.number().int().min(1).max(20)},annotations:{readOnlyHint:true}}, async ({limit}) => {
      calls.push('minecraft_recent_activity');
      assert.equal(limit,20);
      return reply({username:'Coach',messages:[{id:'own-1',actor:'Coach',at:'2026-11-08T21:00:00Z',text:'The mechanism still needs testing.'}]});
    });
    const transport=new StreamableHTTPServerTransport({sessionIdGenerator:undefined,enableJsonResponse:true});
    res.on('close',()=>{transport.close();server.close();});
    await server.connect(transport);
    await transport.handleRequest(req,res,request);
  });
  await new Promise(resolve=>http.listen(0,'127.0.0.1',resolve));
  t.after(()=>new Promise(resolve=>{http.close(resolve);http.closeAllConnections();}));
  return {calls,input:{port:http.address().port,token,username:'Coach',timeout_ms:2000}};
}

test('official SDK reads only fixed status and own-plus-other public activity tools',async t=>{
  const {calls,input}=await fixture(t);
  const result=await observePresence(input);
  assert.deepEqual(calls.sort(),['minecraft_recent_activity','minecraft_status']);
  assert.equal(result.status.username,'Coach');
  assert.equal(result.activity.messages[0].actor,'Coach');
  assert.equal(result.activity.messages[0].id,'own-1');
  assert.ok(!JSON.stringify(result).includes(token));
});

test('fixed client rejects changed identities and MCP tool errors',async t=>{
  for (const mode of ['wrong_identity','error']) {
    const {input}=await fixture(t,mode);
    await assert.rejects(()=>observePresence(input),error=>!error.message.includes(token));
  }
});

test('client forbids redirect and bounds stalled operations without retrying',async t=>{
  const redirected=await fixture(t,'redirect');
  await assert.rejects(()=>observePresence(redirected.input));
  const stalled=await fixture(t,'stall');
  await assert.rejects(()=>observePresence({...stalled.input,timeout_ms:100}));
  assert.equal(stalled.calls.filter(name=>name==='minecraft_status').length,1);
});

test('input cannot choose arbitrary host, tools or unsafe credentials',async()=>{
  const input={port:12345,token,username:'Coach',timeout_ms:2000};
  for(const invalid of [{...input,url:'https://other.invalid'},{...input,tool:'chat'}, {...input,port:0},
    {...input,timeout_ms:8001},{...input,token:'secret\nheader'}]) await assert.rejects(()=>observePresence(invalid));
});
