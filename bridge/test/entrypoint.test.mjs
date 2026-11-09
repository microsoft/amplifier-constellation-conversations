import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,symlinkSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';

const bridge=fileURLToPath(new URL('../live.mjs',import.meta.url));
function isolated(args,home,expectedStatus=0){
  // Never inherit an API credential. Startup must stop at the local credential
  // check, before a provider, session, browser, or configuration request.
  const result=spawnSync(process.execPath,args,{env:{PATH:dirname(process.execPath),HOME:home},input:'',encoding:'utf8',timeout:5000});
  assert.equal(result.error,undefined);assert.equal(result.status,expectedStatus,result.stderr);return result.stdout.trim();
}

test('direct and versioned-directory symlink entrypoints both start the bridge without a provider call',()=>{
  assert(Number(process.versions.node.split('.')[0])>=22,'Use the supported Node22+ runtime');
  const home=mkdtempSync(join(tmpdir(),'companion-entry-'));
  try{
    const linked=join(home,'bridge');symlinkSync(dirname(bridge),linked,'dir');
    for(const args of [[bridge],[join(linked,'live.mjs')],['--preserve-symlinks-main',join(linked,'live.mjs')]]){
      const value=JSON.parse(isolated(args,home));
      assert.equal(value.type,'error');assert.equal(value.code,'credentials_missing');
    }
  }finally{rmSync(home,{recursive:true,force:true});}
});

test('import is side-effect-free even when the importing argv path is missing',()=>{
  const home=mkdtempSync(join(tmpdir(),'companion-import-'));
  try{
    for(const argv of ['',join(home,'not-an-entrypoint')]){
      const source=`process.argv[1]=${JSON.stringify(argv)};await import(${JSON.stringify(pathToFileURL(bridge).href)});process.stdout.write('imported');`;
      assert.equal(isolated(['--input-type=module','-e',source],home),'imported');
    }
  }finally{rmSync(home,{recursive:true,force:true});}
});

for(const [file,status]of [['awareness-decision.mjs',0],['minecraft-presence.mjs',1],['slack-observe.mjs',0],['slack-action.mjs',0]]){
  test(file+' recognizes linked CLI entry while imports remain inert',()=>{
    const home=mkdtempSync(join(tmpdir(),'companion-helper-entry-'));
    try{
      const linked=join(home,'bridge');symlinkSync(dirname(bridge),linked,'dir');
      const direct=join(dirname(bridge),file),link=join(linked,file);
      // Empty stdin and no configuration argument fail local validation. They
      // cannot reach any decision/provider, Minecraft, or Slack client call.
      const expected=JSON.parse(isolated([direct],home,status));
      assert(expected&&typeof expected==='object');
      for(const args of [[link],['--preserve-symlinks-main',link]]){
        const actual=JSON.parse(isolated(args,home,status)),baseline={...expected};
        if(file==='slack-observe.mjs'){
          // Separate invocations keep their own honest observation clocks.
          assert(Number.isFinite(Date.parse(baseline.observed_at)));
          assert(Number.isFinite(Date.parse(actual.observed_at)));
          delete baseline.observed_at;delete actual.observed_at;
        }
        assert.deepEqual(actual,baseline);
      }
      if(file==='awareness-decision.mjs')assert.equal(expected.model_requests,0);
      if(file==='minecraft-presence.mjs')assert.equal(expected.error,'presence_unavailable');
      const source=`process.argv[1]=${JSON.stringify(join(home,'missing'))};await import(${JSON.stringify(pathToFileURL(link).href)});process.stdout.write('imported');`;
      assert.equal(isolated(['--input-type=module','-e',source],home),'imported');
    }finally{rmSync(home,{recursive:true,force:true});}
  });
}
