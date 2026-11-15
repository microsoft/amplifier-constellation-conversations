// Execute the actual page controller against a small DOM and transport fixture.
// No native manager, package replacement, media or provider is started here.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {randomUUID} from 'node:crypto';
import vm from 'node:vm';
const source=await readFile(new URL('../ui/native-updates.mjs',import.meta.url),'utf8');
const html=await readFile(new URL('../ui/native-updates.html',import.meta.url),'utf8');
const root='/api/v1/companion/update',offer=randomUUID(),csrf='fixture-csrf';
function fixture(){
  const f={authenticated:true,owner:'standalone',mode:'normal',current:{version:'0.13.4'},update:null,control:null,operation:null,
    actions:{request:true,recover:false,rollback:false},posts:[],gets:[],fail:null,storage:new Map()};
  f.status=url=>({supported:true,owner:f.owner,mode:f.mode,current:f.current,actions:f.actions,operation:f.operation,
    ...(Object.hasOwn(f,'execution_pending')?{execution_pending:f.execution_pending}:{}),
    ...(!f.legacy?{update:f.update}:{}),offer:f.legacy?null:{id:offer,version:'0.13.5',current_version:f.current.version},
    ...(f.assistant?{assistant:true,companion_url:'http://127.0.0.1:51234/companion'}:{}),
    ...(url.searchParams.has('request_id')?{request:{id:url.searchParams.get('request_id'),state:f.update?.id===url.searchParams.get('request_id')?'recorded':'not_recorded'}}:{}),
    ...(url.searchParams.has('control_id')?{control:f.control||{id:url.searchParams.get('control_id'),action:'cancel',status:'not_recorded'}}:{})});
  f.fetch=async(path,opts)=>{
    const u=new URL(path,'http://fixture'),body=opts.body?JSON.parse(opts.body):null;
    assert.equal(opts.credentials,'same-origin');assert.equal(opts.cache,'no-store');
    const reply=(code,value)=>({ok:code<400,status:code,json:async()=>value});
    if(u.pathname==='/auth/session')return reply(200,{authenticated:f.authenticated,admin:f.authenticated,owner:'owner',auth_mode:'local_password',csrf});
    if(opts.method==='POST')f.posts.push({path:u.pathname,body,headers:opts.headers});else f.gets.push(u);
    if(u.pathname==='/auth/login'){f.authenticated=true;return reply(200,{authenticated:true,admin:true,owner:'owner',auth_mode:'local_password',csrf});}
    if(!f.authenticated)return reply(401,{error:'Sign in required'});
    if(opts.method==='POST')assert.equal(opts.headers['X-CSRF-Token'],csrf);
    if(u.pathname==='/auth/logout'){f.authenticated=false;return reply(200,{});}
    if(opts.method==='GET'&&u.pathname===root){if(f.fail==='offline')throw Error('offline');return reply(200,f.status(u));}
    if(u.pathname===root+'/assistant-close'){
      if(opts.method==='GET')return reply(200,{can_close:f.canClose,identity:{nonce:'a'.repeat(64),port:51235}});
      assert.deepEqual(body,{listener_nonce:'a'.repeat(64)});if(f.fail==='close-lost')throw Error('lost close reply');return reply(200,{closing:true,listener_nonce:body.listener_nonce});
    }
    if(u.pathname===root+'/request'){
      if(f.fail==='before-record'){f.fail=null;return reply(503,{error:'Reply unavailable'});}
      f.update={id:body.request_id,state:'accepted',phase:'queued',message:'The update is queued.',version:'0.13.5',revision:1,can_cancel:true,can_continue:false};
      if(f.fail==='after-record'){f.fail=null;throw Error('lost reply');}
      return reply(202,f.status(u));
    }
    if([root+'/cancel',root+'/continue'].includes(u.pathname)){
      assert.equal(body.request_id,f.update.id);assert.equal(typeof body.control_id,'string');
      if(f.fail==='control-lost'){f.fail=null;throw Error('lost control reply');}
      const action=u.pathname.split('/').at(-1);f.control={id:body.control_id,action,status:'recorded',outcome:{saved:true}};
      f.update={...f.update,state:action==='cancel'?'cancelled':'preparing',revision:f.update.revision+1,can_cancel:action!=='cancel',can_continue:false};
      return reply(202,{...f.status(u),control:f.control});
    }
    if(u.pathname===root+'/recover'){
      assert.equal(body.operation_id,f.operation.id);
      if(f.fail==='recovery-before-record'){f.fail=null;throw Error('lost recovery reply');}
      f.operation={id:randomUUID(),request_id:body.request_id,phase:f.recoveryPhase||'completed',outcome:body.action==='rollback'?'rolled_back':'applied'};
      return reply(202,f.status(u));
    }
    throw Error('Unexpected request '+opts.method+' '+u.pathname);
  };
  return f;
}
async function open(f,{url='http://fixture/updates?offer='+offer,storageThrows=false}={}){
  const nodes=new Map([...html.matchAll(/<[^>]+\bid="([^"]+)"[^>]*>/g)].map(m=>[m[1],{hidden:/\bhidden\b/.test(m[0]),disabled:false,textContent:'',value:'',open:false,focus(){this.focused=true;}}]));
  let serial=0;const timers=new Map(),listeners={};
  const document={hidden:false,getElementById:id=>{assert(nodes.has(id),id);return nodes.get(id);},addEventListener:(name,fn)=>{listeners[name]=fn;}};
  const storage={getItem:key=>{if(storageThrows)throw Error('unavailable');return f.storage.get(key)||null;},setItem:(key,value)=>{if(storageThrows)throw Error('unavailable');f.storage.set(key,value);},removeItem:key=>{if(storageThrows)throw Error('unavailable');f.storage.delete(key);}};
  const globals={document,location:{href:url,origin:new URL(url).origin,assign:destination=>{(f.navigations||=[]).push(destination);}},URL,URLSearchParams,sessionStorage:storage,crypto:{randomUUID},AbortController,
    fetch:f.fetch,setTimeout:(fn,ms)=>{const id=++serial;timers.set(id,{fn,ms});return id;},clearTimeout:id=>timers.delete(id),addEventListener:(name,fn)=>{listeners[name]=fn;}};
  await vm.runInNewContext('(async()=>{'+source+'\n})()',globals);
  return {nodes,async click(id){const el=nodes.get(id);assert(el&&!el.disabled,id+' disabled');return el.onclick?.();},
    async submit(id){return nodes.get(id).onsubmit({preventDefault(){}});},
    pollScheduled(){return [...timers.values()].some(t=>t.ms===3000);},
    async poll(){const timer=[...timers.values()].find(t=>t.ms===3000);assert(timer,'poll scheduled');return timer.fn();}};
}
test('one normal Update sends the offered ID, promptly renders progress and reloads by original UUID',async()=>{
  const f=fixture(),ui=await open(f);assert.equal(ui.nodes.get('update-offer').hidden,false);
  await ui.click('update-start');assert.equal(f.posts.length,1);assert.equal(f.posts[0].path,root+'/request');assert.equal(f.posts[0].body.offer_id,offer);
  const id=f.update.id;assert.equal(ui.nodes.get('update-progress-title').textContent,'Update requested');
  const reload=await open(f);assert.equal(f.posts.length,1);assert.equal(f.gets.at(-1).searchParams.get('request_id'),id);
  assert.equal(reload.nodes.get('update-reference-value').textContent,id);assert(!html.includes('id="update-check"'));assert(!html.includes('id="update-apply"'));
});
test('preparing, waiting and installing use saved progress and never invent a percentage',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');
  for(const [state,title]of [['preparing','Preparing the update'],['waiting','Waiting for current activity'],['installing','Installing the update']]){
    Object.assign(f.update,{state,phase:'unknown_phase',message:'Saved phase: '+state,revision:f.update.revision+1,can_cancel:state!=='installing'});await ui.click('update-refresh');
    assert.equal(ui.nodes.get('update-progress-title').textContent,title);assert.equal(ui.nodes.get('update-progress-detail').textContent,'Saved phase: '+state);
    assert(!ui.nodes.get('update-progress-detail').textContent.includes('%'));
  }
  assert.equal(ui.nodes.get('update-cancel').hidden,true);assert.equal(f.posts.length,1);
});
test('lost update acknowledgement after acceptance reconciles by GET without a second POST',async()=>{
  const f=fixture(),ui=await open(f);f.fail='after-record';await ui.click('update-start');const id=f.update.id;
  await open(f);assert.equal(f.posts.length,1);assert.equal(f.gets.at(-1).searchParams.get('request_id'),id);assert.equal(JSON.parse(f.storage.get('companion-native-update')).id,id);
});
test('not-recorded is an observation, not permission to replay or replace the original Update',async()=>{
  const f=fixture(),ui=await open(f);f.fail='before-record';await ui.click('update-start');const id=f.posts[0].body.request_id;
  const reload=await open(f);assert.equal(f.posts.length,1);assert.equal(reload.nodes.get('update-start').disabled,true);await reload.poll();assert.equal(f.posts.length,1);
  f.update={id,state:'preparing',phase:'verify',revision:1,can_cancel:true,can_continue:false};await reload.click('update-refresh');assert.equal(f.posts.length,1);
  assert.equal(reload.nodes.get('update-request-status').hidden,true);
});
test('Cancel uses a new control ID for the same Update and lost acknowledgement survives reload',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');f.fail='control-lost';await ui.click('update-cancel');
  const control=f.posts.at(-1).body;assert.equal(control.request_id,f.update.id);assert.notEqual(control.control_id,f.update.id);
  const reload=await open(f);assert.equal(f.posts.length,2);assert.equal(f.gets.at(-1).searchParams.get('control_id'),control.control_id);assert.equal(reload.nodes.get('update-cancel').disabled,true);
  f.control={id:randomUUID(),action:'cancel',status:'recorded',outcome:{saved:true}};await reload.click('update-refresh');assert.equal(reload.nodes.get('update-cancel').disabled,true);
  f.control={id:control.control_id,action:'cancel',status:'recorded',outcome:{cancelled:true}};f.update.state='cancelled';f.update.can_cancel=false;
  await reload.click('update-refresh');assert.equal(reload.nodes.get('update-request-status').hidden,true);assert.equal(reload.nodes.get('update-progress-title').textContent,'Update cancelled');assert.equal(f.posts.length,2);
});
test('Continue requires server permission and exact control receipt, not changing progress alone',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');Object.assign(f.update,{state:'needs_attention',message:'Resolve the pending work before continuing.',can_cancel:false,can_continue:true});await ui.click('update-refresh');
  assert.equal(ui.nodes.get('update-continue').hidden,false);f.fail='control-lost';await ui.click('update-continue');const control=f.posts.at(-1).body;
  f.update.state='preparing';f.update.can_continue=false;await ui.click('update-refresh');assert.equal(ui.nodes.get('update-request-status').hidden,false);
  f.control={id:control.control_id,action:'continue',status:'recorded',outcome:{continued:true}};await ui.click('update-refresh');assert.equal(ui.nodes.get('update-request-status').hidden,true);assert.equal(f.posts.length,2);
});
test('reconnection preserves progress and original request; unknown never enables another update',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');f.fail='offline';await ui.click('update-refresh');assert.equal(ui.nodes.get('update-connection').hidden,false);assert.equal(ui.nodes.get('update-progress-title').textContent,'Update requested');
  f.fail=null;Object.assign(f.update,{state:'unknown',message:'The worker result is not confirmed.',can_cancel:false,can_continue:false});await ui.poll();
  assert.equal(ui.nodes.get('update-connection').hidden,true);assert.equal(ui.nodes.get('update-progress-title').textContent,'Update result unconfirmed');assert.equal(ui.nodes.get('update-start').disabled,true);assert.equal(f.posts.length,1);
});
test('advanced folder update is one request and never stores its path or credentials',async()=>{
  const f=fixture(),ui=await open(f,{url:'http://fixture/updates'});ui.nodes.get('update-package-path').value='/private/extracted package';await ui.submit('update-package-form');
  assert.deepEqual(Object.keys(f.posts[0].body).sort(),['package_path','request_id']);assert.equal(f.posts[0].path,root+'/request');
  const stored=JSON.stringify([...f.storage]);assert(!stored.includes('/private/'));assert(!stored.includes(csrf));assert(!f.posts.some(p=>p.path.endsWith('/check')||p.path.endsWith('/apply')));
});
test('sign-out and reauthentication retain an unresolved identity and hide private presentation',async()=>{
  const f=fixture(),ui=await open(f);f.fail='before-record';await ui.click('update-start');const id=f.posts[0].body.request_id;
  await ui.click('update-sign-out');assert.equal(ui.nodes.get('updates-content').hidden,true);assert.equal(ui.nodes.get('update-login').hidden,false);
  ui.nodes.get('update-password').value='fixture-password';await ui.submit('update-login');assert.equal(ui.nodes.get('update-password').value,'');
  assert.equal(f.gets.at(-1).searchParams.get('request_id'),id);assert.equal(f.posts.filter(p=>p.path===root+'/request').length,1);
});
test('legacy recovery stays server-authorized, confirmed and separate from normal Update',async()=>{
  const f=fixture();f.mode='recovery';f.operation={id:randomUUID(),phase:'unknown'};f.actions={request:false,recover:true,rollback:true};const ui=await open(f);
  assert.equal(ui.nodes.get('return-to-companion').hidden,true);assert.equal(ui.nodes.get('update-package-form').hidden,true);assert.equal(ui.nodes.get('update-advanced').open,true);
  await ui.click('update-rollback');assert.equal(f.posts.length,0);await ui.click('update-cancel-rollback');assert.equal(f.posts.length,0);
  await ui.click('update-rollback');await ui.submit('update-rollback-confirmation');assert.equal(f.posts[0].path,root+'/recover');assert.equal(f.posts[0].body.action,'rollback');assert.match(ui.nodes.get('update-recovery-status').textContent,/Previous version restored/);
});
test('enrolled owner and unavailable browser storage cannot start a competing or untraceable request',async()=>{
  const f=fixture();f.owner='enrolled';const ui=await open(f);assert.equal(ui.nodes.get('update-offer').hidden,true);assert.equal(ui.nodes.get('update-advanced').hidden,true);assert.equal(f.posts.length,0);
  const g=fixture(),disabled=await open(g,{storageThrows:true});await disabled.click('update-start');assert.equal(g.posts.length,0);assert.match(disabled.nodes.get('update-error').textContent,/save the update reference/);
});
test('cached UI recognizes only the exact previously received native child after old-version rollback',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const parent=f.update.id,child=randomUUID();f.update.operation_id=child;await ui.click('update-refresh');
  assert.deepEqual(JSON.parse(f.storage.get('companion-native-update')),{id:parent,operation_id:child});
  f.legacy=true;f.operation={id:child,phase:'completed',outcome:'rolled_back'};f.current.version='0.13.4';
  const cached=await open(f);assert.equal(cached.nodes.get('update-progress-title').textContent,'Previous version restored');assert.equal(cached.nodes.get('update-package-form').hidden,true);assert.equal(f.posts.length,1);
  f.operation={id:randomUUID(),phase:'completed',outcome:'applied'};await cached.click('update-refresh');assert.equal(cached.nodes.get('update-progress-title').textContent,'Update result unconfirmed');assert.equal(f.posts.length,1);
});
test('old-schema native settlement never fabricates a lost Cancel control receipt',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const child=randomUUID();f.update.operation_id=child;await ui.click('update-refresh');
  f.fail='control-lost';await ui.click('update-cancel');f.legacy=true;f.operation={id:child,phase:'completed',outcome:'unchanged'};
  const cached=await open(f);assert.equal(cached.nodes.get('update-request-status').hidden,false);assert.match(cached.nodes.get('update-request-status').textContent,/Cancel reply is unconfirmed/);assert.equal(f.posts.length,2);
});
test('explicit rollback stays foreground across reload without reassigning the completed parent',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const parent=f.update.id,original=randomUUID();
  Object.assign(f.update,{state:'completed',operation_id:original,can_cancel:false});f.operation={id:original,phase:'completed',outcome:'applied'};f.actions.rollback=true;await ui.click('update-refresh');
  f.recoveryPhase='switching';await ui.click('update-rollback');await ui.submit('update-rollback-confirmation');const request=f.posts.at(-1).body,newChild=f.operation.id;
  assert.notEqual(newChild,original);assert.equal(request.operation_id,original);assert.equal(ui.nodes.get('update-progress-title').textContent,'Installing the package');
  assert.deepEqual(JSON.parse(f.storage.get('companion-native-update')),{id:parent,operation_id:original});
  const reloaded=await open(f);assert.equal(f.gets.at(-1).searchParams.get('request_id'),request.request_id);assert.equal(reloaded.nodes.get('update-progress-title').textContent,'Installing the package');
  f.operation.phase='restored';await reloaded.click('update-refresh');assert.equal(reloaded.nodes.get('update-progress-title').textContent,'Previous version restored');assert.equal(f.posts.length,2);
  f.legacy=true;await reloaded.click('update-refresh');assert.equal(reloaded.nodes.get('update-progress-title').textContent,'Previous version restored');assert.equal(f.posts.length,2);
});
test('completed result keeps observing execution settlement through reload without replaying Update',async()=>{
  const f=fixture(),ui=await open(f);f.fail='after-record';await ui.click('update-start');const parent=f.update.id,child=randomUUID();
  Object.assign(f.update,{state:'completed',phase:'completed',operation_id:child,can_cancel:false,can_continue:false});
  f.operation={id:child,phase:'completed',outcome:'applied'};f.execution_pending=true;
  f.actions={request:false,check:false,apply:false,recover:false,rollback:false};await ui.click('update-refresh');
  assert.equal(ui.nodes.get('update-progress-title').textContent,'Update complete');
  assert.equal(ui.nodes.get('update-rollback').hidden,true);assert.equal(ui.pollScheduled(),true);
  await ui.submit('update-rollback-confirmation');assert.equal(f.posts.length,1);
  const reload=await open(f);assert.equal(reload.pollScheduled(),true);
  assert.equal(f.gets.at(-1).searchParams.get('request_id'),parent);
  assert.deepEqual(JSON.parse(f.storage.get('companion-native-update')),{id:parent,operation_id:child});
  f.execution_pending=false;f.actions={request:true,check:true,apply:false,recover:false,rollback:true};
  const reads=f.gets.length;await reload.poll();assert.equal(f.gets.length,reads+1);
  assert.equal(reload.nodes.get('update-rollback').hidden,false);assert.equal(reload.pollScheduled(),false);
  assert.deepEqual(JSON.parse(f.storage.get('companion-native-update')),{id:parent,operation_id:child});
  assert.equal(f.posts.length,1);assert.equal(f.posts[0].path,root+'/request');
});
test('historical selected completion observes newer pending execution without gaining rollback authority',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const historical=f.update.id,child=randomUUID();
  Object.assign(f.update,{state:'completed',phase:'completed',operation_id:child,can_cancel:false,can_continue:false});
  f.operation={id:child,phase:'completed',outcome:'applied'};
  // Request-specific history remains selected while global execution belongs to
  // a newer operation. The advisory flag must never grant an old action.
  f.execution_pending=true;f.actions={request:false,check:false,apply:false,recover:false,rollback:false};
  await ui.click('update-refresh');assert.equal(ui.pollScheduled(),true);await ui.poll();
  assert.equal(ui.nodes.get('update-reference-value').textContent,historical);
  assert.equal(f.gets.at(-1).searchParams.get('request_id'),historical);
  assert.equal(ui.nodes.get('update-rollback').hidden,true);await ui.submit('update-rollback-confirmation');assert.equal(f.posts.length,1);
  f.execution_pending=false;await ui.poll();assert.equal(ui.pollScheduled(),false);
  assert.equal(ui.nodes.get('update-rollback').hidden,true);await ui.submit('update-rollback-confirmation');assert.equal(f.posts.length,1);
  assert.deepEqual(JSON.parse(f.storage.get('companion-native-update')),{id:historical,operation_id:child});
});
test('lost rollback acknowledgement cannot display original parent completion or an unrelated native result',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const original=randomUUID();Object.assign(f.update,{state:'completed',operation_id:original,can_cancel:false});f.operation={id:original,phase:'completed',outcome:'applied'};f.actions.rollback=true;await ui.click('update-refresh');
  f.fail='recovery-before-record';await ui.click('update-rollback');await ui.submit('update-rollback-confirmation');const request=f.posts.at(-1).body;
  const reloaded=await open(f);assert.equal(reloaded.nodes.get('update-progress-title').textContent,'Checking your update request');assert.equal(f.posts.length,2);
  f.operation={id:randomUUID(),request_id:randomUUID(),phase:'restored',outcome:'rolled_back'};await reloaded.click('update-refresh');assert.equal(reloaded.nodes.get('update-request-status').hidden,false);assert.equal(f.posts.length,2);
  f.operation={id:randomUUID(),request_id:request.request_id,phase:'restored',outcome:'rolled_back'};await reloaded.click('update-refresh');assert.equal(reloaded.nodes.get('update-progress-title').textContent,'Previous version restored');assert.equal(reloaded.nodes.get('update-request-status').hidden,true);assert.equal(f.posts.length,2);
});
test('a previously acknowledged parent missing from a later read stays unknown and cannot authorize a new Update',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const original=f.update;
  f.update={...original,id:randomUUID(),state:'completed'};f.operation={id:randomUUID(),phase:'completed',outcome:'applied'};await ui.click('update-refresh');
  assert.equal(ui.nodes.get('update-progress-title').textContent,'Update result unconfirmed');assert.equal(ui.nodes.get('update-start').disabled,true);assert.equal(f.posts.length,1);
  f.update=original;await ui.poll();assert.equal(ui.nodes.get('update-progress-title').textContent,'Update requested');assert.equal(f.posts.length,1);
});
test('failed update permits another offer only with explicit server admission and keeps Continue independent',async()=>{
  const f=fixture(),ui=await open(f);await ui.click('update-start');const first=f.update.id;
  Object.assign(f.update,{state:'failed',can_cancel:false,can_continue:true});
  for(const permission of [undefined,false]) {
    f.actions.request=permission;await ui.click('update-refresh');assert.equal(ui.nodes.get('update-offer').hidden,true);assert.equal(ui.nodes.get('update-start').disabled,true);assert.equal(ui.nodes.get('update-continue').hidden,false);
  }
  f.actions.request=true;
  for(const state of ['unknown','needs_attention','interrupted']) {
    f.update.state=state;await ui.click('update-refresh');assert.equal(ui.nodes.get('update-offer').hidden,true);assert.equal(ui.nodes.get('update-start').disabled,true);
  }
  f.update.state='failed';await ui.click('update-refresh');assert.equal(ui.nodes.get('update-offer').hidden,false);assert.equal(ui.nodes.get('update-start').disabled,false);assert.equal(ui.nodes.get('update-continue').hidden,false);
  await ui.click('update-start');assert.notEqual(f.update.id,first);assert.equal(f.posts.length,2);assert.equal(f.posts.at(-1).path,root+'/request');
});
test('restricted candidate assistant disables advanced package submission without hiding the fixed offered Update',async()=>{
  const f=fixture();f.assistant=true;f.actions.advanced_package=false;const fetch=f.fetch;f.fetch=async(path,options)=>{const response=await fetch(path,options);if(path===root+'/request'){const value=await response.json();delete value.assistant;delete value.companion_url;value.actions={...value.actions};delete value.actions.advanced_package;return {...response,json:async()=>value};}return response;};const ui=await open(f);
  assert.equal(ui.nodes.get('update-package-form').hidden,true);assert.equal(ui.nodes.get('update-start').disabled,false);
  ui.nodes.get('update-package-path').value='/another/package';await ui.submit('update-package-form');assert.equal(f.posts.length,0);
  await ui.click('update-start');assert.equal(f.posts.length,1);assert.equal(f.posts[0].body.offer_id,offer);assert.equal(f.posts[0].body.package_path,undefined);assert.equal(ui.nodes.get('update-package-form').hidden,true);
});

test('assistant return closes only a settled exact listener once; a lost close reply never permits replay',async()=>{
  const f=fixture();f.assistant=true;f.canClose=true;const ui=await open(f,{url:'http://127.0.0.1:51235/updates'});
  f.fail='close-lost';await ui.click('return-to-companion');assert.equal(f.posts.length,1);assert.equal(f.posts[0].path,root+'/assistant-close');assert.equal(ui.nodes.get('update-start').disabled,true);assert.match(ui.nodes.get('update-error').textContent,/unconfirmed/);
  const reload=await open(f,{url:'http://127.0.0.1:51235/updates'});await reload.click('return-to-companion');assert.equal(f.posts.length,1);assert.deepEqual(f.navigations,['http://127.0.0.1:51234/companion']);
  const g=fixture();g.assistant=true;g.canClose=false;const active=await open(g,{url:'http://127.0.0.1:51235/updates'});await active.click('return-to-companion');assert.equal(g.posts.length,0);assert.deepEqual(g.navigations,['http://127.0.0.1:51234/companion']);
});
