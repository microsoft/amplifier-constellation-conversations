// Human-browser adapter for huddles.mjs. `api` carries the host's cookie/CSRF
// authentication; this module has no participant credential or admin endpoint.
// getContext() returns the CURRENT {key,owner,parentId} from authenticated UI
// state. Each widget callback carries that same context. A changed call/person
// invalidates old operations before and after each request.
//
// Pending human sends reuse the existing server-side /views/{uuid} draft and
// revision-CAS fields. sessionStorage contains only an opaque view UUID, keyed
// by owner + original parent + huddle; it never holds transcript/draft text.
// A dedicated view avoids changing the main Conversations view. Its ordinary
// history_before cursor is the inspected/scanned high-water + 1. A later status
// check reads at most one 100-message page and persists the next cursor. No
// source POST is retried; absent/malformed/mismatched receipts remain unknown.

const encoder = new TextEncoder();
const uuid = value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
const string = value => typeof value === 'string' && value.length > 0;
const sequence = value => Number.isSafeInteger(value) && value >= 0;
const targetsValid = value => Array.isArray(value) && value.length <= 8 && value.every(string) && new Set(value).size === value.length;
const equal = (left,right) => JSON.stringify(left) === JSON.stringify(right);
const fail = (message = 'The huddle could not be verified.', status = 409) => Object.assign(new Error(message),{status});
const result = (requestId,state) => ({requestId,state});
const sourceBody = (room,text,targets) => ({room,text,target_agent_ids:targets,attachment_ids:[]});
async function sourceHash(room,text,targets) {
  const bytes=await crypto.subtle.digest('SHA-256',encoder.encode(JSON.stringify(sourceBody(room,text,targets))));
  return [...new Uint8Array(bytes)].map(byte=>byte.toString(16).padStart(2,'0')).join('');
}

export function createHuddleApi({api,getContext,storage=globalThis.sessionStorage}) {
  if(typeof api!=='function'||typeof getContext!=='function')throw new TypeError('Huddle API requires authenticated host callbacks.');
  const highWater=new Map();
  function identity(context,signal) {
    const actual=getContext();
    if(signal?.aborted||!context||!actual||![context.key,context.owner,context.parentId].every(string)
      ||context.key!==actual.key||context.owner!==actual.owner||context.parentId!==actual.parentId)
      throw fail('The original conversation is no longer selected.',403);
    return Object.freeze({key:context.key,owner:context.owner,parentId:context.parentId});
  }
  function scope(context,room) {return JSON.stringify([context.owner,context.parentId,room]);}
  async function request(context,signal,path,method='GET',body) {
    identity(context,signal);const value=await api(path,method,body,{signal});identity(context,signal);return value;
  }
  function descriptor(value,context,id=null,current=false) {
    if(value?.version!==1||!string(value.id)||value.conversation_id!==value.id||(id!==null&&value.id!==id)
      ||!(current&&value.id===context.parentId)&&!(value.origin?.kind==='conversation'&&value.origin.conversation_id===context.parentId)
      ||!Array.isArray(value.members)||!value.members.some(member=>member.username===context.owner)
      ||!Array.isArray(value.participants)||value.participants.some(agent=>!string(agent.id)||agent.delivery!=='external')
      ||!['open','stopped'].includes(value.state)||!['quiet','follow'].includes(value.preference?.mode))
      throw fail('This huddle is not available in the original conversation.',403);
    return value;
  }
  function normalized(value) {
    return {id:value.id,title:value.title,purpose:value.purpose,status:value.state==='open'?'Discussion open':'Discussion stopped',
      participants:[...value.participants.map(agent=>({id:agent.id,label:agent.label,kind:'agent'})),
        ...value.members.map(member=>({id:member.username,label:member.username,kind:'human'}))],
      following:value.preference.mode==='follow',canSend:value.state==='open',inspectUrl:value.inspect_url,outcomeSummary:value.outcome_summary};
  }
  async function parentList(context,signal) {
    const value=await request(context,signal,`/api/v1/conversations/${encodeURIComponent(context.parentId)}/huddles`);
    if(value?.version!==1||!Array.isArray(value.items))throw fail();
    return {items:value.items.map(item=>descriptor(item,context)),current:value.current?descriptor(value.current,context,context.parentId,true):null};
  }
  async function authorized(context,signal,id) {
    if(!string(id))throw fail('Choose a huddle in the original conversation.',403);
    const listed=await parentList(context,signal),current=listed.current?.id===id;
    if(!current&&!listed.items.some(item=>item.id===id))throw fail('This huddle is no longer available.',403);
    return descriptor(await request(context,signal,`/api/v1/huddles/${encodeURIComponent(id)}`),context,id,current);
  }
  function viewId(context,room) {
    const key='constellation.huddle.view.v1:'+scope(context,room);
    try {
      let id=storage.getItem(key);
      if(id!==null&&!uuid(id))throw fail('The saved message view needs review.');
      if(id===null){id=crypto.randomUUID();storage.setItem(key,id);if(storage.getItem(key)!==id)throw new Error('view not retained');}
      return id;
    } catch {throw fail('This browser could not preserve a message receipt.');}
  }
  async function readView(context,signal,room) {
    const id=viewId(context,room),value=await request(context,signal,`/api/v1/views/${encodeURIComponent(id)}`);
    if(value?.id!==id||!sequence(value.revision)||!value.state||typeof value.state!=='object'
      ||(value.state.selected_conversation_id!==null&&value.state.selected_conversation_id!==room))throw fail('The saved message view needs review.');
    return value;
  }
  async function pending(view,room) {
    const saved=view.state;
    if(!saved.draft_request_id&&!saved.draft_request_sha256)return null;
    if(saved.selected_conversation_id!==room||!uuid(saved.draft_request_id)||typeof saved.draft!=='string'
      ||!saved.draft.trim()||encoder.encode(saved.draft).length>6000||!targetsValid(saved.target_agent_ids)
      ||!Array.isArray(saved.attachment_ids)||saved.attachment_ids.length
      ||!sequence(saved.history_before)||saved.history_before<1
      ||saved.draft_request_sha256!==await sourceHash(room,saved.draft,saved.target_agent_ids))
      throw fail('The saved message receipt needs review before another message is sent.');
    return {requestId:saved.draft_request_id,text:saved.draft,targetAgentIds:[...saved.target_agent_ids],state:'unknown',
      hash:saved.draft_request_sha256,after:saved.history_before-1};
  }
  async function writeView(context,signal,view,state) {
    const value=await request(context,signal,`/api/v1/views/${encodeURIComponent(view.id)}`,'PUT',{expected_revision:view.revision,state});
    if(value?.id!==view.id||!sequence(value.revision)||value.revision<=view.revision||!value.state)throw fail('The saved message receipt could not be confirmed.');
    return value;
  }
  async function updatePending(context,signal,room,expected,patch) {
    const view=await readView(context,signal,room),current=await pending(view,room);
    if(!current||current.requestId!==expected.requestId||current.hash!==expected.hash||current.text!==expected.text
      ||!equal(current.targetAgentIds,expected.targetAgentIds))return false;
    await writeView(context,signal,view,{...view.state,...patch});return true;
  }
  async function clearPending(context,signal,room,saved) {
    try {await updatePending(context,signal,room,saved,{draft:'',draft_request_id:'',draft_request_sha256:'',target_agent_ids:[],attachment_ids:[],history_before:null});}
    catch { /* A confirmed source remains confirmed; keep an uncleared journal for later review. */ }
  }
  return {
    async list({context,signal}) {
      context=identity(context,signal);const value=await parentList(context,signal);
      return {items:value.items.map(normalized),current:value.current?normalized(value.current):null};
    },
    async read({context,signal,huddleId,before=null}) {
      context=identity(context,signal);
      if(before!==null&&(!sequence(before)||before<1))throw fail('The history page could not be verified.');
      const huddle=await authorized(context,signal,huddleId),query=new URLSearchParams({limit:'50'});
      if(before!==null)query.set('before',String(before));
      const page=await request(context,signal,`/api/v1/conversations/${encodeURIComponent(huddleId)}/messages?${query}`);
      if(!Array.isArray(page?.items)||page.items.length>50||page.items.some(message=>!string(message.id)||!sequence(message.seq)||typeof message.text!=='string')
        ||(page.next_cursor!==null&&(!sequence(page.next_cursor)||page.next_cursor<1)))throw fail('The history page could not be verified.');
      const key=scope(context,huddleId),highest=Math.max(highWater.get(key)||0,...page.items.map(message=>message.seq));highWater.set(key,highest);
      const saved=await pending(await readView(context,signal,huddleId),huddleId);
      identity(context,signal);
      return {...normalized(huddle),messages:page.items.map(message=>({id:message.id,actor:message.actor,text:message.text,createdAt:message.created_at})),
        before:page.next_cursor,submission:saved?{requestId:saved.requestId,text:saved.text,targetAgentIds:saved.targetAgentIds,state:'unknown'}:undefined};
    },
    async setFollowing({context,signal,huddleId,following}) {
      context=identity(context,signal);if(typeof following!=='boolean')throw fail('Choose quiet updates or follow details.');
      await authorized(context,signal,huddleId);
      const mode=following?'follow':'quiet',value=await request(context,signal,`/api/v1/huddles/${encodeURIComponent(huddleId)}/preference`,'PUT',{mode});
      if(value?.mode!==mode||value.membership_changed!==false||value.work_authority_changed!==false)throw fail('The update preference could not be confirmed.');
      return {following};
    },
    async send({context,signal,huddleId,requestId,text,targetAgentIds}) {
      context=identity(context,signal);
      // These checks precede any HTTP and establish only that THIS attempt was
      // not transmitted. A server/network failure below never establishes that.
      if(!uuid(requestId)||typeof text!=='string'||!text.trim()||encoder.encode(text).length>6000||!targetsValid(targetAgentIds))return result(requestId,'not_recorded');
      const huddle=await authorized(context,signal,huddleId);
      if(huddle.state!=='open'||targetAgentIds.some(target=>!huddle.participants.some(agent=>agent.id===target)))return result(requestId,'not_recorded');
      const view=await readView(context,signal,huddleId),old=await pending(view,huddleId);
      if(old)return result(requestId,old.requestId===requestId?'unknown':'not_recorded');
      const hash=await sourceHash(huddleId,text,targetAgentIds),saved={requestId,text,targetAgentIds:[...targetAgentIds],hash,after:highWater.get(scope(context,huddleId))||0};
      try {
        const stored=await writeView(context,signal,view,{...view.state,selected_conversation_id:huddleId,panel:'conversation',
          draft:text,draft_request_id:requestId,draft_request_sha256:hash,target_agent_ids:[...targetAgentIds],attachment_ids:[],
          history_search:'',history_before:saved.after+1});
        const confirmed=await pending(stored,huddleId);
        if(!confirmed||confirmed.requestId!==requestId||confirmed.hash!==hash||confirmed.after!==saved.after)return result(requestId,'not_recorded');
      } catch {return result(requestId,'not_recorded');} // No source POST has run.
      try {
        const response=await request(context,signal,`/api/v1/conversations/${encodeURIComponent(huddleId)}/messages`,'POST',
          {request_id:requestId,text,target_agent_ids:[...targetAgentIds],attachment_ids:[]});
        if(response?.accepted!==true||response.message_id!==requestId)return result(requestId,'unknown');
        await clearPending(context,signal,huddleId,saved);return result(requestId,'recorded');
      } catch {return result(requestId,'unknown');}
    },
    async findSubmission({context,signal,huddleId,requestId}) {
      context=identity(context,signal);
      if(!uuid(requestId))return result(requestId,'unknown');
      try {
        await authorized(context,signal,huddleId);
        const saved=await pending(await readView(context,signal,huddleId),huddleId);
        if(!saved||saved.requestId!==requestId)return result(requestId,'unknown');
        const page=await request(context,signal,`/api/v1/huddles/${encodeURIComponent(huddleId)}/messages?after=${saved.after}&limit=100`);
        if(page?.version!==1||!Array.isArray(page.items)||page.items.length>100||!sequence(page.next_cursor)
          ||page.next_cursor<saved.after||typeof page.has_more!=='boolean'||page.coverage?.after!==saved.after
          ||page.coverage.through!==page.next_cursor||page.coverage.complete_through_cursor!==true)return result(requestId,'unknown');
        let previous=saved.after;
        for(const message of page.items){if(!sequence(message.seq)||message.seq<=previous)return result(requestId,'unknown');previous=message.seq;}
        if(previous!==page.next_cursor)return result(requestId,'unknown');
        const match=page.items.find(message=>message.id===requestId);
        if(match) {
          if(match.role!=='user'||match.actor!==context.owner||match.agent_id!==null||match.text!==saved.text
            ||match.status!=='complete'||!equal(match.target_agent_ids,saved.targetAgentIds))return result(requestId,'unknown');
          await clearPending(context,signal,huddleId,saved);return result(requestId,'recorded');
        }
        if(page.next_cursor>saved.after)await updatePending(context,signal,huddleId,saved,{history_before:page.next_cursor+1});
        return result(requestId,'unknown');
      } catch {return result(requestId,'unknown');}
    }
  };
}
