import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';

// Run the actual browser helper with a controlled revisioned service. The
// service fixture owns its state; it does not implement the client merge rules.
const source=await readFile(new URL('../ui/app.js',import.meta.url),'utf8');
const start=source.includes('function viewOrigin(')?source.indexOf('function viewOrigin('):source.indexOf('function saveView(');
const helpers=source.slice(start,source.indexOf('\nfunction applyView(){',start));
assert(start>=0&&helpers.length>0,'production view helper is present');
const clone=value=>structuredClone(value);
const initial=()=>({id:'view-a',revision:1,state:{selected_conversation_id:'room-a',panel:'conversation',dialog:null,draft:'original',search:'',action:null}});
const deferred=()=>{let resolve;const promise=new Promise(r=>{resolve=r;});return {promise,resolve};};
function fixture(){
  const ui={view:initial(),saveChain:Promise.resolve(),viewIntents:new Set(),viewEdits:new Map(),viewIntentSequence:0,savingViews:0};
  let stored=initial();const calls=[],rendered=[],timers=new Map();let timerId=0;
  const remote=patch=>{stored={...stored,revision:stored.revision+1,state:{...stored.state,...clone(patch)}};};
  let beforePut=async()=>{};
  const api=async(path,method='GET',body)=>{
    assert.equal(path,'/api/v1/views/view-a');
    assert(['GET','PUT'].includes(method),'view changes must not publish messages');
    calls.push({method,body:clone(body)});
    if(method==='GET')return clone(stored);
    await beforePut(clone(body));
    if(body.expected_revision!==stored.revision){const error=new Error('Conflict');error.status=409;throw error;}
    stored={...stored,revision:stored.revision+1,state:clone(body.state)};
    return clone(stored);
  };
  const applyView=()=>rendered.push(clone(ui.view));
  const setTimer=callback=>{const id=++timerId;timers.set(id,callback);return id;};
  const clearTimer=id=>timers.delete(id);
  const errors=[];
  const control=new Function('ui','api','viewId','applyView','structuredClone','setTimeout','clearTimeout','error',`${helpers}\nreturn {saveView,...(typeof queueViewEdit==='function'?{queueViewEdit,cancelViewEdit}:{}),...(typeof pendingViewField==='function'?{pendingViewField}:{})};`)(ui,api,'view-a',applyView,structuredClone,setTimer,clearTimer,e=>errors.push(e));
  return {...control,ui,calls,rendered,errors,remote,timers,stored:()=>clone(stored),beforePut:fn=>{beforePut=fn;}};
}

test('a conflict keeps the newer draft and never resends the stale patch',async()=>{
  const f=fixture();let first=true;
  f.beforePut(async()=>{if(first){first=false;f.remote({draft:'newer human draft'});}});
  await assert.rejects(f.saveView({draft:'older controller draft'}),e=>e.status===409);
  assert.equal(f.stored().state.draft,'newer human draft');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
  assert.equal(f.ui.view.state.draft,'newer human draft');
});

test('one disjoint rebase preserves the independently changed field',async()=>{
  const f=fixture();let first=true;
  f.beforePut(async()=>{if(first){first=false;f.remote({search:'new human filter'});}});
  await f.saveView({draft:'prepared draft'});
  assert.equal(f.stored().state.search,'new human filter');
  assert.equal(f.stored().state.draft,'prepared draft');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,2);
});

test('queued same-browser draft edits retain their order',async()=>{
  const f=fixture(),entered=deferred(),release=deferred();let first=true;
  f.beforePut(async()=>{if(first){first=false;entered.resolve();await release.promise;}});
  const earlier=f.saveView({draft:'first local edit'});await entered.promise;
  const later=f.saveView({draft:'second local edit'});
  release.resolve();await Promise.all([earlier,later]);
  assert.equal(f.stored().state.draft,'second local edit');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,2);
});

test('a queued old-room draft cannot follow a preceding navigation',async()=>{
  const f=fixture(),entered=deferred(),release=deferred();
  f.beforePut(async()=>{entered.resolve();await release.promise;});
  const navigation=f.saveView({selected_conversation_id:'room-b',draft:''});await entered.promise;
  const stale=f.saveView({draft:'private room-a text'});
  const rejected=assert.rejects(stale,e=>e.status===409);
  release.resolve();await navigation;await rejected;
  assert.equal(f.stored().state.selected_conversation_id,'room-b');
  assert.equal(f.stored().state.draft,'');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
});

test('a newer same-field refresh before dispatch refuses the old patch without a write',async()=>{
  const f=fixture(),release=deferred();f.ui.saveChain=release.promise;
  const stale=f.saveView({draft:'old intent'});
  f.remote({draft:'new human intent'});f.ui.view=f.stored();
  const rejected=assert.rejects(stale,e=>e.status===409);
  release.resolve();await rejected;
  assert.equal(f.calls.length,0);
  assert.equal(f.stored().state.draft,'new human intent');
});

test('a delayed focus action does not overwrite newer focus',async()=>{
  const f=fixture();let first=true;
  f.beforePut(async()=>{if(first){first=false;f.remote({action:{id:'human-focus',kind:'focus',target:'search'}});}});
  await assert.rejects(f.saveView({action:{id:'old-focus',kind:'focus',target:'composer'}}),e=>e.status===409);
  assert.equal(f.stored().state.action.id,'human-focus');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
});

test('focus cannot be rebased into a different panel',async()=>{
  const f=fixture();let first=true;
  f.beforePut(async()=>{if(first){first=false;f.remote({panel:'activity'});}});
  await assert.rejects(f.saveView({action:{id:'old-focus',kind:'focus',target:'composer'}}),e=>e.status===409);
  assert.equal(f.stored().state.panel,'activity');
  assert.equal(f.stored().state.action,null);
});

test('a second disjoint conflict is returned without an unbounded retry',async()=>{
  const f=fixture();let n=0;
  f.beforePut(async()=>f.remote({search:`new filter ${++n}`}));
  await assert.rejects(f.saveView({draft:'old intent'}),e=>e.status===409);
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,2);
  assert.equal(f.stored().state.draft,'original');
});

test('transport failure remains uncertain and is never resubmitted',async()=>{
  const f=fixture();f.beforePut(async()=>{throw new Error('Lost reply');});
  await assert.rejects(f.saveView({draft:'pending intent'}),/Lost reply/);
  assert.deepEqual(f.calls.map(c=>c.method),['PUT']);
});

test('mutating the caller patch after queueing cannot change its intent',async()=>{
  const f=fixture(),release=deferred();f.ui.saveChain=release.promise;
  const patch={draft:'original request',action:{id:'once',kind:'focus',target:'composer'}};
  const save=f.saveView(patch);patch.draft='changed later';patch.action.target='search';release.resolve();await save;
  assert.equal(f.stored().state.draft,'original request');
  assert.equal(f.stored().state.action.target,'composer');
});

test('debounced text keeps the resource at the time of typing',async()=>{
  const f=fixture();f.queueViewEdit('draftTimer',450,{draft:'private room-a text'});
  const fire=f.timers.get(f.ui.draftTimer);
  await f.saveView({selected_conversation_id:'room-b',draft:''});
  fire();await assert.rejects(f.ui.saveChain,e=>e.status===409);
  assert.equal(f.stored().state.selected_conversation_id,'room-b');
  assert.equal(f.stored().state.draft,'');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
});

test('pending later typing stays protected while an earlier edit is acknowledged',async()=>{
  const f=fixture(),entered=deferred(),release=deferred();let first=true;
  f.beforePut(async()=>{if(first){first=false;entered.resolve();await release.promise;}});
  const earlier=f.saveView({draft:'first local edit'});await entered.promise;
  f.queueViewEdit('draftTimer',450,{draft:'newer local typing'});
  assert.equal(f.pendingViewField('draft'),true);
  release.resolve();await earlier;
  assert.equal(f.pendingViewField('draft'),true);
  f.timers.get(f.ui.draftTimer)();await f.ui.saveChain;
  assert.equal(f.stored().state.draft,'newer local typing');
  assert.equal(f.pendingViewField('draft'),false);
});

test('replaced and canceled debounce work releases only its own pending intent',async()=>{
  const f=fixture();f.queueViewEdit('draftTimer',450,{draft:'first key'});
  f.queueViewEdit('draftTimer',450,{draft:'later key'});
  f.queueViewEdit('searchTimer',350,{search:'separate filter'});
  assert.equal(f.ui.viewIntents.size,2);assert.equal(f.timers.size,2);
  f.cancelViewEdit('draftTimer');
  assert.equal(f.pendingViewField('draft'),false);
  assert.equal(f.pendingViewField('search'),true);
  assert.equal(f.ui.viewIntents.size,1);assert.equal(f.timers.size,1);
  f.timers.get(f.ui.searchTimer)();await f.ui.saveChain;
  assert.equal(f.stored().state.search,'separate filter');
  assert.equal(f.stored().state.draft,'original');
});

// Timers must not give an older keystroke priority over a newer control action.
test('earlier debounced typing cannot follow a newer programmatic draft',async()=>{
  const f=fixture();f.queueViewEdit('draftTimer',450,{draft:'earlier local typing'});
  const fire=f.timers.get(f.ui.draftTimer);
  await f.saveView({draft:'newer explicit draft'});
  assert.equal(f.pendingViewField('draft'),false);
  fire();await assert.rejects(f.ui.saveChain,e=>e.status===409);
  assert.equal(f.stored().state.draft,'newer explicit draft');
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
  assert.equal(f.ui.savingViews,0);
});
