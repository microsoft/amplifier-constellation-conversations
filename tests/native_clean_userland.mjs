// Model-free native Linux package qualification. CI provides only readonly
// package/fixture mounts; this script runs with the package's private Node.
import {spawn, spawnSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import {readFile, writeFile, mkdir} from 'node:fs/promises';
import https from 'node:https';
import path from 'node:path';
import assert from 'node:assert/strict';

const checks=[];
const home='/home/fixture/Installation café';
const bundle='/bundle';
const workspace='/home/fixture/workspace';
const emptyPath='/home/fixture/no-tools';
const environment={HOME:'/home/fixture', USER:'fixture', LANG:'C.UTF-8',
  PATH:emptyPath, TMPDIR:'/tmp', TEMP:'/tmp', TMP:'/tmp'};
let server;
const run=(executable,args,timeout=30000,input)=>{
  const result=spawnSync(executable,args,{env:environment,cwd:workspace,
    encoding:'utf8',timeout,input,maxBuffer:2*1024*1024});
  assert.equal(result.status,0,`${path.basename(executable)} failed: ${result.error?.message??result.stderr?.slice(-1800)}`);
  return result.stdout;
};
const request=(url,ca)=>new Promise((resolve,reject)=>{
  const req=https.get(url,{ca,rejectUnauthorized:true,timeout:2000},response=>{
    let text=''; response.on('data',v=>text+=v);
    response.on('end',()=>resolve({status:response.statusCode,text}));
  });
  req.on('error',reject);
  req.on('timeout',()=>req.destroy(new Error('HTTPS timeout')));
});

try {
  const installationMount=(await readFile('/proc/mounts','utf8')).split('\n')
    .find(row=>row.split(' ')[1]==='/home/fixture');
  assert.ok(installationMount&&!installationMount.split(' ')[3].split(',').includes('noexec'),
    'The isolated installation tmpfs must allow installed executable launch');
  await mkdir(workspace,{recursive:true}); await mkdir(emptyPath,{recursive:true});
  const manifest=JSON.parse(await readFile(bundle+'/manifest.json','utf8'));
  assert.equal(process.platform,'linux');
  assert.ok(['linux-x64','linux-arm64'].includes(manifest.target));
  assert.equal(manifest.target,'linux-'+process.arch);
  assert.equal(process.versions.node,manifest.node_version);
  assert.equal(process.execPath,bundle+'/runtime/node');
  assert.equal(process.getuid(),1000);
  checks.push('actual private Node version/native architecture under non-root UID 1000');
  assert.equal(run(bundle+'/agent-conversations',['--version']).trim(),
    'agent-conversations '+manifest.version);
  const verified=JSON.parse(run(bundle+'/agent-conversations',['--home',home,'bundle','verify','--path',bundle]));
  assert.equal(verified.source_revision,manifest.source_revision);
  const installed=JSON.parse(run(bundle+'/agent-conversations',['--home',home,'bundle','install','--path',bundle],90000));
  const ep=installed.entrypoints;
  assert.ok(ep.service.startsWith(home+'/packages/'));
  assert.ok(ep.node.startsWith(home+'/packages/'));
  run(ep.service,['--home',home,'bundle','verify','--path',path.dirname(ep.service)]);
  checks.push('native package verified and installed under a relocated Unicode/spaced fresh home');
  run(ep.service,['--home',home,'init','--owner','fixture','--agent-command',ep.node,
    '--agent-arg','/fixture/fixture.mjs','--agent-cwd',workspace]);
  const configPath=home+'/config.json';
  const configBefore=await readFile(configPath,'utf8');
  const fixtureKey='fixture-native-key-not-a-real-provider-credential';
  const imported=run(ep.service,['--home',home,'credential','import-openai'],30000,fixtureKey+'\n');
  assert.deepEqual(JSON.parse(imported),{stored:true,provider:'openai'});
  assert.ok(!imported.includes(fixtureKey));
  assert.equal(await readFile(home+'/secrets/openai-api-key','utf8'),fixtureKey);
  assert.equal(await readFile(configPath,'utf8'),configBefore);
  checks.push('native private stdin credential import preserves configuration and does not expose its input');
  const config=JSON.parse(await readFile(configPath,'utf8'));
  config.agent.env.NATIVE_FIXTURE_LOG='/home/fixture/initialize.jsonl';
  await writeFile(configPath,JSON.stringify(config));
  const probe=JSON.parse(run(ep.service,['--home',home,'probe']));
  assert.equal(probe.ok,true);
  assert.equal(probe.agent_info.name,'native-package-fixture');
  const events=(await readFile('/home/fixture/initialize.jsonl','utf8')).trim().split('\n').map(v=>JSON.parse(v));
  assert.deepEqual(events.map(v=>v.method),['initialize']);
  assert.equal(events[0].node,ep.node);
  assert.equal(events[0].provider_key_present,false);
  checks.push('official ACP SDK initialize only; no session creation or model prompt');
  const require=createRequire(ep.bridge);
  for(const name of ['openai','ws','zod','@slack/web-api','@modelcontextprotocol/sdk/client/index.js',
    '@modelcontextprotocol/sdk/client/streamableHttp.js','@modelcontextprotocol/sdk/server/mcp.js',
    '@modelcontextprotocol/sdk/server/streamableHttp.js']) await import(pathToFileURL(require.resolve(name)));
  await import(pathToFileURL(ep.bridge)); await import(pathToFileURL(ep.mcp));
  checks.push('installed bridge and official SDKs import without system Node/npm or network');
  let serverLog='';
  server=spawn(ep.service,['--home',home,'serve'],{env:environment,cwd:workspace,stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',v=>serverLog+=v); server.stderr.on('data',v=>serverLog+=v);
  const ca=await readFile(home+'/tls/root-ca.crt');
  let runtime; let lastReadinessError;
  const deadline=Date.now()+30000;
  while(Date.now()<deadline){
    assert.equal(server.exitCode,null,`service exited: ${serverLog.slice(-1800)}`);
    try {
      runtime=JSON.parse(await readFile(home+'/runtime.json','utf8'));
      const health=await request(runtime.url+'/health',ca);
      if(health.status===200&&JSON.parse(health.text).ok) break;
      throw new Error(`health returned HTTP ${health.status}`);
    } catch(error){ lastReadinessError=error; runtime=null; }
    await new Promise(resolve=>setTimeout(resolve,100));
  }
  assert.ok(runtime,`HTTPS readiness failed: ${lastReadinessError}; ${serverLog.slice(-1800)}`);
  const anonymous=await request(runtime.url+'/api/v1/conversations',ca);
  assert.ok([401,403].includes(anonymous.status));
  assert.ok(JSON.parse(run(ep.service,['--home',home,'api','GET','/api/v1/conversations']))!==null);
  checks.push('installation-CA-verified HTTPS and owner-token API; anonymous private API denied');
  const stopped=new Promise(resolve=>server.once('exit',resolve));
  server.kill('SIGTERM');
  await Promise.race([stopped,new Promise((_,reject)=>setTimeout(()=>reject(new Error('service did not exit')),8000))]);
  assert.ok(server.exitCode!==null||server.signalCode!==null);
  checks.push('owned disposable service stopped');
  process.stdout.write(JSON.stringify({status:'passed',checks,source_revision:manifest.source_revision,
    target:manifest.target,node_version:process.versions.node,uid:process.getuid(),model_requests:0,
    installation_mount:installationMount,os_password_login:'not_tested',browser_media:'not_tested',
    signing:'not_tested'})+'\n');
} catch(error){
  process.stdout.write(JSON.stringify({status:'failed',checks,error:String(error),model_requests:0})+'\n');
  process.exitCode=1;
} finally {
  if(server&&server.exitCode===null&&server.signalCode===null) server.kill('SIGKILL');
}
