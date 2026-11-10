import test from 'node:test';
import assert from 'node:assert/strict';
import {webcrypto,createHash,randomUUID} from 'node:crypto';
import {createHuddleApi} from '../ui/huddles-api.mjs';
if(!globalThis.crypto)globalThis.crypto=webcrypto;
const copy=value=>structuredClone(value);
const parent='11111111-1111-4111-8111-111111111111',room='22222222-2222-4222-8222-222222222222';
const hash=(text,targets=[])=>createHash('sha256').update(JSON.stringify({room,text,target_agent_ids:targets,attachment_ids:[]})).digest('hex');

function fixture() {
  const entries=new Map(),views=new Map(),calls=[],messages=[{id:randomUUID(),seq:5,role:'assistant',actor:'Coach',agent_id:'coach',text:'Earlier source.',status:'complete',created_at:1794312794000,target_agent_ids:[]}];
  const f={entries,views,calls,messages,context:{key:'call-a',owner:'brian',parentId:parent},mode:'good',failJournal:false,loseJournalAck:false,page404:false,readViewHook:null,postHook:null};
  const d=f.descriptor={version:1,id:room,conversation_id:room,title:'Entry review',purpose:'Review the entrance.',state:'open',revision:1,
    origin:{kind:'conversation',conversation_id:parent,actor:{id:'brian',kind:'human'}},inspect_url:'https://fixture.invalid/?conversation='+room,
    participants:[{id:'coach',label:'Coach',connection_id:'game-coach',delivery:'external'},{id:'rook',label:'Rook',connection_id:'game-rook',delivery:'external'}],
    members:[{username:'brian',role:'owner'}],preference:{mode:'quiet'},outcome_summary:{count:1,latest:{id:'old-result',created_at:1794300000000,kind:'progress'}}};
  const storage={getItem:key=>entries.has(key)?entries.get(key):null,setItem:(key,value)=>entries.set(key,value)};
  const api=async(path,method='GET',body,options={})=>{
    const user=f.context.owner,call={path,method,body:body===undefined?undefined:copy(body),owner:user};calls.push(call);
    const url=new URL(path,'https://fixture.invalid');
    if(url.pathname===`/api/v1/conversations/${parent}/huddles`){
      if(f.authorityStatus)throw Object.assign(new Error('access no longer available'),{status:f.authorityStatus});
      return {version:1,items:[copy(d)],current:null};
    }
    if(url.pathname===`/api/v1/conversations/${room}/huddles`)return {version:1,items:[],current:copy(d)};
    if(url.pathname===`/api/v1/huddles/${room}`)return copy(d);
    if(url.pathname===`/api/v1/huddles/${room}/preference`&&method==='PUT'){d.preference.mode=body.mode;return {mode:body.mode,membership_changed:false,work_authority_changed:false};}
    if(url.pathname===`/api/v1/conversations/${room}/messages`&&method==='GET'){
      const before=url.searchParams.get('before'),filtered=messages.filter(message=>before===null||message.seq<Number(before)),more=filtered.length>50;
      const items=filtered.slice(-50);return {items:copy(items),next_cursor:more?items[0].seq:null,search:''};
    }
    if(url.pathname.startsWith('/api/v1/views/')){
      const id=url.pathname.split('/').at(-1),key=user+':'+id;
      if(method==='GET'){
        if(f.readViewHook){const hook=f.readViewHook;f.readViewHook=null;await hook();}
        return copy(views.get(key)||{id,revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',search:''}});
      }
      const existing=views.get(key),revision=existing?.revision||0;
      if(f.failJournal||revision!==body.expected_revision)throw Object.assign(new Error('private CAS diagnostic'),{status:409});
      const value={id,revision:revision+1,state:copy(body.state)};views.set(key,value);
      if(f.loseJournalAck){f.loseJournalAck=false;throw new Error('lost journal ack');}
      return copy(value);
    }
    if(url.pathname===`/api/v1/conversations/${room}/messages`&&method==='POST'){
      const journal=[...views.values()].find(view=>view.state.draft_request_id===body.request_id);
      assert(journal,'draft must be saved before source POST');assert.equal(journal.state.draft,body.text);assert.equal(journal.state.draft_request_sha256,hash(body.text,body.target_agent_ids));
      call.viewBefore=copy(journal);call.storageBefore=[...entries.values()];
      if(f.mode==='404')throw Object.assign(new Error('source could be unavailable'),{status:404});
      messages.push({id:body.request_id,seq:Math.max(0,...messages.map(message=>message.seq))+1,role:'user',actor:user,agent_id:null,text:body.text,status:'complete',created_at:1794313000000,target_agent_ids:copy(body.target_agent_ids)});
      if(f.postHook)await f.postHook();
      if(f.mode==='throw')throw new Error('source ack lost');
      return {accepted:true,message_id:f.mode==='mismatch'?randomUUID():body.request_id};
    }
    if(url.pathname===`/api/v1/huddles/${room}/messages`){
      if(f.page404)throw Object.assign(new Error('not found is not proof of rejection'),{status:404});
      if(f.pageStatus)throw Object.assign(new Error('source access revoked'),{status:f.pageStatus});
      const after=Number(url.searchParams.get('after')),all=messages.filter(message=>message.seq>after),items=all.slice(0,100),next=items.at(-1)?.seq??after;
      if(f.pageHook)await f.pageHook();
      return {version:1,items:copy(items),next_cursor:next,has_more:all.length>100,coverage:{after,through:next,complete_through_cursor:true},state:d.state,revision:d.revision};
    }
    throw new Error('Unexpected fixture route: '+method+' '+path);
  };
  f.create=()=>createHuddleApi({api,storage,getContext:()=>f.context});f.adapter=f.create();
  f.args=()=>({context:copy(f.context),huddleId:room});
  f.send=async(text='Human request.',targets=['coach'])=>{
    const requestId=randomUUID(),args={...f.args(),requestId,text,targetAgentIds:targets};return {args,result:await f.adapter.send(args)};
  };
  f.posts=()=>calls.filter(call=>call.method==='POST');f.view=()=>[...views.values()][0];
  return f;
}

test('authorized discovery/read preserves source timestamps, attribution and compact summaries; inspection only reads',async()=>{
  const f=fixture(),list=await f.adapter.list(f.args());assert.equal(list.items[0].id,room);assert.equal(list.current,null);
  assert.equal(list.items[0].outcomeSummary.latest.created_at,1794300000000);assert.equal(list.items[0].outcomes,undefined);
  const read=await f.adapter.read(f.args());assert.equal(read.messages[0].createdAt,1794312794000);assert.equal(read.messages[0].actor,'Coach');assert.equal(read.canSend,true);
  assert.deepEqual(read.participants.map(person=>[person.id,person.kind]),[['coach','agent'],['rook','agent'],['brian','human']]);
  assert(f.calls.every(call=>call.method==='GET'));assert.equal(f.views.size,0);
  assert([...f.entries.values()].every(value=>/^[0-9a-f-]{36}$/.test(value)));assert.equal([...f.entries.values()].some(value=>value.includes('Earlier source')),false);
});

test('quiet/follow persists only preference; stopped is not completion and cannot send',async()=>{
  const f=fixture();assert.deepEqual(await f.adapter.setFollowing({...f.args(),following:true}),{following:true});
  assert.deepEqual(f.calls.filter(call=>call.method!=='GET').map(call=>[call.path,call.body]),[[`/api/v1/huddles/${room}/preference`,{mode:'follow'}]]);
  f.descriptor.state='stopped';const view=await f.adapter.read(f.args());assert.equal(view.canSend,false);assert.equal(view.status,'Discussion stopped');
  const submitted=await f.send();assert.equal(submitted.result.state,'not_recorded');assert.equal(f.posts().length,0);
});

test('human send journals exact request before the ordinary endpoint and clears only after matching saved source',async()=>{
  const f=fixture();await f.adapter.read(f.args());const {args,result}=await f.send('Review this privately.',['coach','rook']);assert.equal(result.state,'recorded');
  assert.equal(f.posts().length,1);const posted=f.posts()[0];assert.equal(posted.path,`/api/v1/conversations/${room}/messages`);
  assert.deepEqual(posted.body,{request_id:args.requestId,text:args.text,target_agent_ids:['coach','rook'],attachment_ids:[]});
  assert.equal(posted.viewBefore.state.history_before,6);assert(posted.storageBefore.every(value=>/^[0-9a-f-]{36}$/.test(value)));
  assert.equal(f.view().state.draft_request_id,'');assert.equal(f.view().state.draft,'');assert.equal(f.view().state.selected_conversation_id,room);
  assert(f.calls.every(call=>!call.path.includes('/posts')));
});

test('lost response rehydrates after adapter reload and exact read receipt reconciles without another POST',async()=>{
  const f=fixture();await f.adapter.read(f.args());f.mode='throw';const {args,result}=await f.send();assert.equal(result.state,'unknown');
  f.adapter=f.create();const restored=await f.adapter.read(f.args());assert.equal(restored.submission.requestId,args.requestId);assert.equal(restored.submission.text,args.text);
  assert.equal((await f.adapter.send(args)).state,'unknown');assert.equal(f.posts().length,1);
  assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'recorded');assert.equal(f.posts().length,1);assert.equal(f.view().state.draft_request_id,'');
});

test('unknown reconciliation reads only one page per explicit check and persists scan cursor across reload',async()=>{
  const f=fixture();await f.adapter.read(f.args());f.mode='404';const {args}=await f.send();
  for(let seq=6;seq<=210;seq++)f.messages.push({id:randomUUID(),seq,role:'assistant',actor:'Rook',agent_id:'rook',text:'Coordination',status:'complete',created_at:1794313000000,target_agent_ids:[]});
  f.messages.push({id:args.requestId,seq:211,role:'user',actor:'brian',agent_id:null,text:args.text,status:'complete',created_at:1794313000001,target_agent_ids:args.targetAgentIds});
  assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'unknown');assert.equal(f.view().state.history_before,106);
  f.adapter=f.create();assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'unknown');assert.equal(f.view().state.history_before,206);
  assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'recorded');
  assert.equal(f.calls.filter(call=>call.path.startsWith(`/api/v1/huddles/${room}/messages?`)).length,3);assert.equal(f.posts().length,1);
});

test('source 404, missing receipt and mismatched POST acknowledgment all stay unknown',async()=>{
  for(const mode of ['404','mismatch']){
    const f=fixture();await f.adapter.read(f.args());f.mode=mode;const {args,result}=await f.send();assert.equal(result.state,'unknown');assert.equal(f.view().state.draft_request_id,args.requestId);
    f.page404=true;assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'unknown');assert.equal(f.view().state.draft_request_id,args.requestId);
    f.page404=false;f.messages.splice(1);assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'unknown');assert.equal(f.posts().length,1);
  }
});

test('status check propagates lost parent/room authority for immediate widget revocation without clearing or resending the source',async()=>{
  for(const status of [401,403,404]){
    const f=fixture();await f.adapter.read(f.args());f.mode='throw';const {args}=await f.send();f.authorityStatus=status;
    await assert.rejects(f.adapter.findSubmission({...f.args(),requestId:args.requestId}),error=>error.status===status);
    assert.equal(f.view().state.draft_request_id,args.requestId);assert.equal(f.posts().length,1);
  }
});

test('source authorization failure and a context switch during a status read propagate revocation, while receipt404 remains unknown',async()=>{
  for(const status of [401,403]){
    const f=fixture();await f.adapter.read(f.args());f.mode='throw';const {args}=await f.send();f.pageStatus=status;
    await assert.rejects(f.adapter.findSubmission({...f.args(),requestId:args.requestId}),error=>error.status===status);
    assert.equal(f.view().state.draft_request_id,args.requestId);assert.equal(f.posts().length,1);
  }
  const f=fixture();await f.adapter.read(f.args());f.mode='throw';const {args}=await f.send();
  f.pageHook=()=>{f.context={key:'other-call',owner:'someone-else',parentId:parent};};
  await assert.rejects(f.adapter.findSubmission({...f.args(),requestId:args.requestId}),error=>error.status===403);
  assert.equal(f.view().state.draft_request_id,args.requestId);assert.equal(f.posts().length,1);
});

test('matching text alone is not receipt proof: exact ID, actor, role, agent null, status and targets are required',async()=>{
  for(const patch of [{actor:'someone-else'},{role:'assistant'},{agent_id:'coach'},{text:'Different content'},{status:'streaming'},{target_agent_ids:['rook']},{id:randomUUID()}]){
    const f=fixture();await f.adapter.read(f.args());f.mode='throw';const {args}=await f.send();Object.assign(f.messages.at(-1),patch);
    assert.equal((await f.adapter.findSubmission({...f.args(),requestId:args.requestId})).state,'unknown');assert.equal(f.view().state.draft_request_id,args.requestId);assert.equal(f.posts().length,1);
  }
});

test('wrong identity and cross-parent huddle are refused; no source message is sent',async()=>{
  const f=fixture(),args=f.args();f.context={...f.context,key:'call-b',owner:'other'};
  await assert.rejects(f.adapter.read(args),error=>error.status===403);assert.equal(f.calls.length,0);
  f.context=args.context;f.descriptor.origin.conversation_id='different-parent';await assert.rejects(f.adapter.read(args),error=>error.status===403);assert.equal(f.posts().length,0);
  f.descriptor.origin.conversation_id=parent;await assert.rejects(f.adapter.read({...args,huddleId:'another-room'}),error=>error.status===403);assert.equal(f.posts().length,0);
});

test('explicit current huddle metadata permits direct room inspection without guessing or replacing its origin',async()=>{
  const f=fixture();f.context={...f.context,key:'direct-huddle',parentId:room};f.descriptor.origin={kind:'external',service:'minecraft',conversation_id:'game-public'};
  const list=await f.adapter.list(f.args());assert.deepEqual(list.items,[]);assert.equal(list.current.id,room);
  const view=await f.adapter.read(f.args());assert.equal(view.id,room);assert.equal(view.canSend,true);assert.equal(f.posts().length,0);
});

test('UTF-8 oversize is definitive local rejection before HTTP; invalid targets and duplicate IDs also stop locally',async()=>{
  const f=fixture();for(const body of [{text:'😀'.repeat(1501),targetAgentIds:[]},{text:'ok',targetAgentIds:['coach','coach']},{text:' ',targetAgentIds:[]}]){
    assert.equal((await f.adapter.send({...f.args(),requestId:randomUUID(),...body})).state,'not_recorded');
  }
  assert.equal(f.calls.length,0);assert.equal(f.views.size,0);
});

test('failed durable view write blocks POST; persisted lost-ack view remains inspectable and never auto-replays',async()=>{
  const f=fixture();await f.adapter.read(f.args());f.failJournal=true;let sent=await f.send();assert.equal(sent.result.state,'not_recorded');assert.equal(f.posts().length,0);
  f.failJournal=false;f.loseJournalAck=true;sent=await f.send('Preserve this unconfirmed journal.');assert.equal(sent.result.state,'not_recorded');assert.equal(f.posts().length,0);
  f.adapter=f.create();const view=await f.adapter.read(f.args());assert.equal(view.submission.requestId,sent.args.requestId);assert.equal((await f.adapter.findSubmission({...f.args(),requestId:sent.args.requestId})).state,'unknown');assert.equal(f.posts().length,0);
});

test('journal hash mismatch prevents replacement; a stale identity response never clears the original pending source',async()=>{
  const f=fixture();await f.adapter.read(f.args());f.mode='throw';const first=await f.send();f.view().state.draft_request_sha256='0'.repeat(64);
  await assert.rejects(f.adapter.read(f.args()),/receipt needs review/);assert.equal(f.posts().length,1);
  f.view().state.draft_request_sha256=hash(first.args.text,first.args.targetAgentIds);assert.equal((await f.send('Another message.')).result.state,'not_recorded');assert.equal(f.posts().length,1);
  const fresh=fixture();await fresh.adapter.read(fresh.args());const old=fresh.args();fresh.postHook=()=>{fresh.context={...fresh.context,key:'caller-b',owner:'other'};};
  const submitted=await fresh.adapter.send({...old,requestId:randomUUID(),text:'Original caller.',targetAgentIds:[]});assert.equal(submitted.state,'unknown');assert.equal(fresh.view().state.draft,'Original caller.');
});

test('confirmed source cleanup preserves a concurrently changed journal rather than erasing another request',async()=>{
  const f=fixture();await f.adapter.read(f.args());const newer=randomUUID();
  f.postHook=()=>{f.readViewHook=()=>{const view=f.view();view.revision++;view.state={...view.state,draft:'Newer saved message',draft_request_id:newer,draft_request_sha256:hash('Newer saved message',['rook']),target_agent_ids:['rook']};};};
  const sent=await f.send();assert.equal(sent.result.state,'recorded');assert.equal(f.view().state.draft_request_id,newer);assert.equal(f.view().state.draft,'Newer saved message');assert.equal(f.posts().length,1);
});
