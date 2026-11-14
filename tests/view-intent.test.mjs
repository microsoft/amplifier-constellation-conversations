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
  const ui={items:[],view:initial(),saveChain:Promise.resolve(),viewIntents:new Set(),viewEdits:new Map(),viewIntentSequence:0,savingViews:0,viewEpoch:0};
  let stored=initial();const calls=[],rendered=[],cleared=[],timers=new Map();let timerId=0;
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
  const control=new Function('ui','api','viewId','applyView','structuredClone','setTimeout','clearTimeout','error','clearConversation',`${helpers}\nreturn {saveView,...(typeof queueViewEdit==='function'?{queueViewEdit,cancelViewEdit}:{}),...(typeof pendingViewField==='function'?{pendingViewField}:{}),...(typeof receiveView==='function'?{receiveView,clearUnavailableConversation}:{})};`)(ui,api,'view-a',applyView,structuredClone,setTimer,clearTimer,e=>errors.push(e),()=>cleared.push(clone(ui.view)));
  return {...control,ui,calls,rendered,cleared,errors,remote,timers,replace:value=>{stored=clone(value);},stored:()=>clone(stored),beforePut:fn=>{beforePut=fn;}};
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

test('a 409 reset adopts the empty view and rejects the old room draft',async()=>{
  const f=fixture();let first=true;
  f.beforePut(async()=>{if(first){first=false;f.replace({id:'view-a',revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}});}});
  await assert.rejects(f.saveView({draft:'removed-room private draft'}),e=>e.status===409);
  assert.equal(f.ui.view.revision,0);assert.equal(f.ui.view.state.draft,'');
  assert.equal(f.ui.view.state.selected_conversation_id,null);
  assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
  assert.equal(f.cleared.length,1);
  await f.saveView({draft:'new user intent after reset'});
  assert.equal(f.stored().state.draft,'new user intent after reset');
});

test('reset invalidates old queued intent even when its original fields match',async()=>{
  const f=fixture(),release=deferred();
  f.replace({...initial(),state:{...initial().state,selected_conversation_id:null,draft:''}});f.ui.view=f.stored();
  f.ui.saveChain=release.promise;const old=f.saveView({draft:'prepared before reset'});
  const oldOutcome=assert.rejects(old,e=>e.status===409);
  f.replace({...f.stored(),revision:0});
  f.receiveView(f.stored(),f.ui.view);release.resolve();await oldOutcome;
  assert.equal(f.calls.length,0);assert.equal(f.ui.view.state.draft,'');
});

test('reset cancels old debounce and clears protected content before rendering',()=>{
  const f=fixture();f.queueViewEdit('draftTimer',450,{draft:'removed-room typing'});
  f.receiveView({id:'view-a',revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}},f.ui.view);
  assert.equal(f.timers.size,0);assert.equal(f.pendingViewField('draft'),false);
  assert.equal(f.cleared.length,1);assert.equal(f.cleared[0].state.selected_conversation_id,null);
});

test('a delayed reset snapshot cannot replace a newer adopted view',()=>{
  const f=fixture(),requestedFrom=f.ui.view;
  f.remote({draft:'newer human intent'});f.ui.view=f.stored();
  f.receiveView({id:'view-a',revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}},requestedFrom);
  assert.equal(f.ui.view.state.draft,'newer human intent');assert.equal(f.cleared.length,0);
});

test('an unavailable room clears private state synchronously before recovery writes',()=>{
  const f=fixture();f.ui.view.state={...f.ui.view.state,title_draft:'private title',dialog:'rename',attachment_ids:['private-file'],elicitation_drafts:{form:{secret:'private answer'}}};
  f.queueViewEdit('titleTimer',300,{title_draft:'older private title'});
  assert.equal(f.clearUnavailableConversation('room-a'),true);
  assert.equal(f.calls.length,0);assert.equal(f.cleared.length,1);
  const current=f.cleared[0].state;assert.equal(current.selected_conversation_id,null);
  assert.equal(current.draft,'');assert.equal(current.title_draft,'');assert.equal(current.dialog,null);
  assert.deepEqual(current.attachment_ids,[]);assert.deepEqual(current.elicitation_drafts,{});
  assert.equal(f.timers.size,0);
});

test('an older in-flight GET cannot revive a deleted view after reset',()=>{
  const f=fixture(),requestedFrom=f.ui.view,epoch=f.ui.viewEpoch;
  const staleReply={...initial(),revision:2,state:{...initial().state,draft:'revoked room draft'}};
  f.receiveView({id:'view-a',revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}},f.ui.view,epoch);
  f.receiveView(staleReply,requestedFrom,epoch);
  assert.equal(f.ui.view.revision,0);assert.equal(f.ui.view.state.selected_conversation_id,null);
  assert.equal(f.ui.view.state.draft,'');assert.equal(f.cleared.length,1);
});

// Exercise the real refresh coroutine, holding actual read boundaries rather
// than testing only the generation predicate in isolation.
const refreshSource=source.slice(source.indexOf('async function refresh(){'),source.indexOf('\nfunction viewOrigin('));
for(const phase of ['list','conversation','history','attachments']){
  test(`reset discards an already started ${phase} response before assigning or rendering`,async()=>{
    const ui={items:[{id:'room-a',title:'Original room'}],agents:[],people:[],conversation:{id:'room-a',messages:[{text:'old room text'}]},files:[{id:'old-file'}],view:initial(),viewEpoch:0,viewIntents:new Set(),viewEdits:new Map(),refreshing:false,refreshAgain:false,viewIntentSequence:0,savingViews:0,saveChain:Promise.resolve()};
    const entered=deferred(),release=deferred(),events=[],calls=[];
    const paths={list:'/api/v1/conversations?search=',conversation:'/api/v1/conversations/room-a',history:'/api/v1/conversations/room-a/messages?limit=100&search=',attachments:'/api/v1/conversations/room-a/attachments'};
    const responses={
      [paths.list]:{items:[{id:'delayed-list',title:'Delayed private title'}]},
      '/api/v1/views/view-a':initial(),'/api/v1/agents':{items:[]},'/api/v1/people':{items:[]},
      [paths.conversation]:{id:'room-a',messages:[{text:'delayed room body'}]},
      [paths.history]:{items:[{text:'delayed history'}],next_cursor:null},[paths.attachments]:{items:[{id:'delayed-file'}]}
    };
    const api=async(path,method='GET')=>{
      assert.equal(method,'GET','refresh must not deliver input');calls.push(path);
      assert(Object.hasOwn(responses,path),path);
      if(path===paths[phase]){entered.resolve();await release.promise;}
      return clone(responses[path]);
    };
    const clearConversation=()=>{ui.conversation=null;ui.files=[];ui.historyCursor=null;events.push('clear');};
    const render=()=>events.push('render'),queue=()=>events.push('queue');
    const control=new Function('ui','api','viewId','$','applyView','structuredClone','clearTimeout','clearConversation','renderList','renderChat','renderParticipants','refreshLinkedHuddles','renderActivity','queueRefresh',`${helpers}\n${refreshSource}\nreturn {refresh,receiveView};`)(ui,api,'view-a',()=>({hidden:false}),render,structuredClone,()=>{},clearConversation,render,render,render,render,render,queue);
    const running=control.refresh();await entered.promise;
    control.receiveView({id:'view-a',revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}},ui.view);
    const keptItems=clone(ui.items),startedCalls=[...calls];release.resolve();await running;
    assert.equal(ui.conversation,null);assert.deepEqual(ui.files,[]);
    assert.deepEqual(ui.items,keptItems,'an older list cannot replace the reset cache');
    assert.deepEqual(events,['clear','queue'],'no stale render or linked-room lookup follows the reset');
    assert.deepEqual(calls,startedCalls,'no later room read follows stale completion');
  });
}

test('an in-flight activity read cannot refill cleared details after reset',async()=>{
  const start=source.indexOf('async function renderActivity(){');
  const activitySource=source.slice(start,source.indexOf('\n',start));
  const response=deferred(),ui={viewEpoch:0},rendered=[];
  const activity=new Function('ui','api','$','el',`${activitySource}\nreturn renderActivity;`)(ui,async path=>{assert.equal(path,'/api/v1/activity');return response.promise;},()=>({replaceChildren:()=>rendered.push('replace'),append:()=>rendered.push('append')}),()=>({}));
  const running=activity();ui.viewEpoch++;response.resolve({items:[]});await running;
  assert.deepEqual(rendered,[]);
});
