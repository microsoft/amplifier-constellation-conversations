import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {randomUUID} from 'node:crypto';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StreamableHTTPClientTransport} from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import {startCompanionMcp} from '../companion-mcp.mjs';

test('catalog read arguments remain executable and schema identifies the correct namespace and view',async t=>{
  const callId=randomUUID(),token='private-catalog-fixture-token',connection_id='world:Workshop / east';
  const reads={
    saved:{tool:'companion_observations',arguments:{connection_id}},
    current:{tool:'companion_connection_status',arguments:{connection_id,view:'current'}},
    reports:{tool:'companion_connection_status',arguments:{connection_id,view:'reports'}},
  };
  const paths=[];
  const api=createServer((req,res)=>{
    paths.push(req.url);
    assert.equal(req.headers.authorization,`Bearer ${token}`);
    res.writeHead(200,{'Content-Type':'application/json'});
    res.end(JSON.stringify(req.url.endsWith('/connections')?{
      connections:[],observation_sources:[{id:connection_id,connection_id,label:'Workshop',read_only:true,reads}],
    }:{status:'fixture_read'}));
  });
  await new Promise(resolve=>api.listen(0,'127.0.0.1',resolve));
  const bridge=await startCompanionMcp({apiBase:`http://127.0.0.1:${api.address().port}`,callId,token});
  const client=new Client({name:'catalog-test',version:'1'});
  t.after(async()=>{await client.close();await bridge.close();await new Promise(resolve=>{api.close(resolve);api.closeAllConnections();});});
  await client.connect(new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${bridge.port}/mcp`),{requestInit:{headers:{Authorization:`Bearer ${token}`}}}));
  const {tools}=await client.listTools();
  for(const name of ['companion_observations','companion_connection_status']){
    const tool=tools.find(tool=>tool.name===name);
    assert.match(tool.inputSchema.properties.connection_id.description,/Exact Companion connection_id/);
    assert.match(tool.inputSchema.properties.connection_id.description,/provider_id and conversation_id are different/);
  }
  const status=tools.find(tool=>tool.name==='companion_connection_status');
  assert.deepEqual(status.inputSchema.properties.view.enum,['reports','current']);
  assert.match(status.inputSchema.properties.view.description,/current selects typed current snapshots/);
  assert.match(status.inputSchema.properties.view.description,/Omission retains the reports view/);
  const discovered=await client.callTool({name:'companion_connections',arguments:{}});
  for(const read of Object.values(discovered.structuredContent.observation_sources[0].reads)){
    assert.notEqual((await client.callTool({name:read.tool,arguments:read.arguments})).isError,true);
  }
  assert.notEqual((await client.callTool({name:'companion_connection_status',arguments:{connection_id}})).isError,true);
  const root=`/api/v1/companion/calls/${callId}/tools/`;
  const encoded=encodeURIComponent(connection_id);
  assert.deepEqual(paths,[
    `${root}connections`,`${root}observations?connection_id=${encoded}`,
    `${root}connection-status?connection_id=${encoded}&view=current`,
    `${root}connection-status?connection_id=${encoded}&view=reports`,
    `${root}connection-status?connection_id=${encoded}`,
  ]);
  for(const args of [{connection_id,view:null},{connection_id,view:'latest'},{connection_id,view:'current',provider_id:'a-context-provider'}]){
    assert.equal((await client.callTool({name:'companion_connection_status',arguments:args})).isError,true);
  }
  assert.equal(paths.length,5,'invalid arguments make no host request');
});
