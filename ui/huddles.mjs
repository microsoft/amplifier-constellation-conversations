// Presentation only: the host supplies authorized data and durable operations.
// No HTTP routes, polling, voice commands, grants, Stop or task authority here.
//
// createHuddles({root, list, read, setFollowing, send, findSubmission})
// Every callback receives {context, signal, ...}; context.key must change when
// the signed-in person or original conversation/call changes. The host must
// enforce that identity itself. Do not put credentials in context.
// list -> {items:[{id,title,purpose,status,participants:[{id,label,kind}],following}]}
// read({huddleId,before,limit:50}) -> the same summary plus
//   {messages:[{id,actor,text,createdAt}], before:null|opaqueCursor, canSend:bool,
//    submission?:{requestId,text,targetAgentIds,state}}
// setFollowing({huddleId,following}) -> {following:bool} (persisted preference).
// send({huddleId,requestId,text,targetAgentIds}) and
// findSubmission({huddleId,requestId}) -> {requestId,state,messageId?}.
// Submission state is 'recorded', 'not_recorded', or 'unknown'. 'recorded'
// means the human message was saved, not that agents acted or anyone heard it.
// Only a definitive source rejection may map to 'not_recorded'; transport
// failures, missing lookup rows and malformed receipts must remain 'unknown'.
// The adapter owns durable request storage and rehydrates an unresolved saved
// submission through read(). The widget never retries a send. Reconciliation
// is read-only. A new context/revocation clears all private presentation.

const node = (tag, className, text) => {
  const result = document.createElement(tag);
  if (className) result.className = className;
  if (text !== undefined) result.textContent = text;
  return result;
};
const button = (text, action, className = '') => {
  const result = node('button', className, text); result.type = 'button'; result.onclick = action; return result;
};
const denied = error => [401,403,404].includes(error?.status) || error?.code === 'access_revoked';
const stateOf = value => ['recorded','not_recorded','unknown'].includes(value) ? value : 'unknown';

export function createHuddles({root, list, read, setFollowing, send, findSubmission, requestId = () => crypto.randomUUID()}) {
  if (!root || ![list,read,setFollowing,send].every(fn => typeof fn === 'function')) throw new TypeError('Huddle view requires its host callbacks.');
  let context = null, generation = 0, listRequest = 0, readRequest = 0, abort = new AbortController();
  let summaries = [], selected = null, room = null, cursor = null, destroyed = false;
  const drafts = new Map();
  const frame = node('section','huddles'); frame.setAttribute('aria-label','Work huddles');
  const heading = node('div','huddles-heading'); heading.append(node('h2',null,'Work huddles'));
  const refreshButton = button('Refresh', () => refresh()); heading.append(refreshButton);
  const status = node('p','huddles-status'); status.setAttribute('role','status');
  const directory = node('div','huddles-list');
  const inspector = node('section','huddles-inspector'); inspector.hidden = true;
  frame.append(heading,status,directory,inspector); root.replaceChildren(frame);
  const valid = token => !destroyed && context && generation === token;
  const input = extra => ({context,signal:abort.signal,...extra});
  const draftFor = id => {
    if (!drafts.has(id)) drafts.set(id,{text:'',targets:[],joining:false,pending:null,checking:false,following:false});
    return drafts.get(id);
  };
  function clear(message = '') {
    generation++; listRequest++; readRequest++; abort.abort(); abort = new AbortController();
    context = null; summaries = []; selected = room = cursor = null; drafts.clear();
    directory.replaceChildren(); inspector.replaceChildren(); inspector.hidden = true;
    directory.hidden = false; status.textContent = message; refreshButton.disabled = true;
  }
  function revoke() { clear('This huddle view is no longer available.'); }
  function renderDirectory() {
    directory.replaceChildren(...summaries.map(summary => {
      const card = node('article','huddles-summary'); card.dataset.huddleId = summary.id;
      card.append(node('h3',null,summary.title || 'Work huddle'),node('p',null,summary.purpose || 'Purpose not provided.'));
      card.append(node('p','huddles-meta',(summary.participants || []).map(person => person.label).filter(Boolean).join(', ') || 'Participants not reported.'));
      card.append(node('p','huddles-meta',summary.status || 'Status not reported.'));
      card.append(button('Inspect', () => inspect(summary.id))); return card;
    }));
    if (!summaries.length) directory.append(node('p','huddles-empty','No linked huddles are available here.'));
  }
  async function refresh({automatic=false}={}) {
    if (!context || destroyed) return;
    const token = generation, sequence = ++listRequest; status.textContent = 'Checking huddles…';
    try {
      const result = await list(input());
      if (!valid(token) || sequence !== listRequest) return;
      if (!Array.isArray(result?.items) || result.items.some(item => typeof item?.id !== 'string')) throw new Error('invalid view');
      summaries = result.items; renderDirectory(); status.textContent = '';
      if(selected&&!summaries.some(item=>item.id===selected)){close();status.textContent='That huddle is no longer available.';return;}
      if (selected && (!automatic || room?.following === true)) await load(selected,cursor);
    } catch (error) {
      if (!valid(token) || sequence !== listRequest) return;
      if (denied(error)) { revoke(); return; }
      status.textContent = 'Huddles could not be refreshed. The last available view may be out of date.';
    }
  }
  async function inspect(id) {
    if (!context || destroyed || typeof id !== 'string') return;
    selected = id; room = null; cursor = null; directory.hidden = true; inspector.hidden = false;
    inspector.replaceChildren(button('Back to huddles', close),node('p',null,'Opening huddle…'));
    await load(id,null);
  }
  function close() {
    readRequest++; selected = room = cursor = null; inspector.replaceChildren(); inspector.hidden = true;
    directory.hidden = false; renderDirectory();
  }
  async function load(id,before) {
    const token = generation, sequence = ++readRequest;
    try {
      const result = await read(input({huddleId:id,before,limit:50}));
      if (!valid(token) || sequence !== readRequest || selected !== id) return;
      if (result?.id !== id || !Array.isArray(result.messages)) throw new Error('invalid view');
      room = result; cursor = before;
      const draft = draftFor(id);
      if (!draft.pending && result.submission?.requestId && stateOf(result.submission.state) === 'unknown') {
        const saved = result.submission;
        draft.pending = {requestId:saved.requestId,text:String(saved.text || ''),targetAgentIds:Array.isArray(saved.targetAgentIds)?[...saved.targetAgentIds]:[],state:'unknown'};
        draft.text = draft.pending.text; draft.targets = draft.pending.targetAgentIds; draft.joining = true;
      }
      renderRoom(); status.textContent = '';
    } catch (error) {
      if (!valid(token) || sequence !== readRequest || selected !== id) return;
      if (denied(error)) {
        drafts.delete(id); selected = room = cursor = null; inspector.replaceChildren(); inspector.hidden = true; directory.hidden = false;
        summaries = summaries.filter(item => item.id !== id); renderDirectory(); status.textContent = 'That huddle is no longer available.'; return;
      }
      status.textContent = 'The huddle could not be refreshed. Existing message status is unchanged.';
    }
  }
  function renderRoom() {
    if (!room || !selected) return;
    const id = selected, draft = draftFor(id), oldHistory = inspector.querySelector('.huddles-history');
    const previousScroll = oldHistory?.scrollTop, followingEnd = oldHistory && oldHistory.scrollHeight-oldHistory.scrollTop-oldHistory.clientHeight<32;
    const oldText = inspector.querySelector('textarea'), editing = oldText && document.activeElement === oldText;
    const selection = editing ? [oldText.selectionStart,oldText.selectionEnd] : null;
    const targetsOpen = inspector.querySelector('.huddles-targets')?.open;
    const top = node('div','huddles-heading'); top.append(button('Back to huddles',close),node('h3',null,room.title || 'Work huddle'));
    const follow = button(room.following ? 'Quiet updates' : 'Follow details', () => changeFollowing(id,!room.following));
    follow.disabled = draft.following; follow.setAttribute('aria-pressed',String(room.following === true));
    const description = node('p','huddles-meta',room.following ? 'Following details in this view. Voice updates are considered separately.' : 'Routine coordination stays here. Refresh to see new discussion. Important results can return to the original conversation.');
    const controls = node('div','huddles-controls'); controls.append(follow,button('Refresh discussion',()=>load(id,cursor)));
    const history = node('div','huddles-history'); history.setAttribute('aria-label','Huddle discussion'); history.setAttribute('aria-live','off');
    const messages = room.messages.slice(-100);
    for (const message of messages) {
      const article = node('article','huddles-message'); article.dataset.messageId = String(message.id || '');
      const meta = node('div','huddles-message-meta'); meta.append(node('strong',null,message.actor || 'Participant'));
      const date = new Date(message.createdAt);
      if (message.createdAt !== null && message.createdAt !== undefined && Number.isFinite(date.getTime())) {
        const time = node('time',null,date.toLocaleString()); time.dateTime = date.toISOString(); meta.append(time);
      } else meta.append(node('span',null,'Time not reported'));
      article.append(meta,node('p','huddles-text',String(message.text || ''))); history.append(article);
    }
    if (!messages.length) history.append(node('p','huddles-empty','No messages on this page.'));
    const paging = node('div','huddles-controls');
    if (room.messages.length > 100) paging.append(node('span','huddles-meta','Showing the last 100 messages returned on this page.'));
    if (room.before !== null && room.before !== undefined) paging.append(button('Earlier messages',()=>load(id,room.before)));
    if (cursor !== null) paging.append(button('Back to latest',()=>load(id,null)));
    inspector.replaceChildren(top,node('p',null,room.purpose || ''),description,controls,history,paging);
    if (previousScroll !== undefined) history.scrollTop = followingEnd ? history.scrollHeight : previousScroll;
    if (room.canSend === true || draft.pending) {
      const join = button(draft.joining ? 'Hide message box' : 'Join discussion',()=>{draft.joining = !draft.joining; renderRoom(); if(draft.joining)inspector.querySelector('textarea')?.focus();});
      inspector.append(join);
      if (draft.joining) renderComposer(id,draft);
    } else inspector.append(node('p','huddles-meta','You can inspect this discussion. Posting is not available here.'));
    if (draft.pending && !draft.joining) inspector.append(node('p','huddles-status','A message still needs its delivery status checked.'));
    const nextTargets = inspector.querySelector('.huddles-targets'); if(nextTargets&&targetsOpen)nextTargets.open=true;
    const nextText = inspector.querySelector('textarea');
    if(editing&&nextText&&!nextText.disabled){nextText.focus({preventScroll:true});nextText.setSelectionRange(...selection);}
  }
  function renderComposer(id,draft) {
    const form = node('form','huddles-composer'), label = node('label',null,'Your message');
    const text = node('textarea'); text.rows = 3; text.maxLength = 6000; text.value = draft.text;
    text.setAttribute('aria-label','Message the huddle'); label.append(text);
    const targets = node('details','huddles-targets'), summary = node('summary',null,'To: Everyone here'); targets.append(summary);
    const agents = (room.participants || []).filter(person => person.kind === 'agent');
    const everyone = node('input'); everyone.type = 'checkbox'; everyone.checked = draft.targets.length === 0;
    const everyoneLabel = node('label',null,'Everyone here'); everyoneLabel.prepend(everyone); targets.append(everyoneLabel);
    const agentInputs = agents.map(agent => {
      const choice = node('input'); choice.type = 'checkbox'; choice.value = agent.id; choice.checked = draft.targets.includes(agent.id);
      const agentLabel = node('label',null,agent.label || 'Agent'); agentLabel.prepend(choice); targets.append(agentLabel); return choice;
    });
    const updateTargets = () => {
      draft.targets = agentInputs.filter(choice=>choice.checked).map(choice=>choice.value); everyone.checked = draft.targets.length === 0;
      summary.textContent = 'To: ' + (draft.targets.length ? agents.filter(agent=>draft.targets.includes(agent.id)).map(agent=>agent.label).join(', ') : 'Everyone here');
    };
    everyone.onchange = () => { agentInputs.forEach(choice=>{choice.checked=false;}); updateTargets(); };
    agentInputs.forEach(choice=>{choice.onchange=updateTargets;}); updateTargets();
    const submit = node('button',null,'Send message'); submit.type = 'submit';
    const locked = Boolean(draft.pending);
    text.disabled = locked; everyone.disabled = locked; agentInputs.forEach(choice=>{choice.disabled=locked;});
    submit.disabled = locked || room.canSend !== true || !draft.text.trim();
    text.oninput = () => { draft.text=text.value; submit.disabled = Boolean(draft.pending) || room.canSend !== true || !draft.text.trim(); };
    form.onsubmit = event => { event.preventDefault(); if(!submit.disabled)submitMessage(id,draft); };
    form.append(label,targets,submit);
    if (draft.pending) {
      const pending = draft.pending;
      const message = pending.state === 'sending' ? 'Sending your message…' : pending.state === 'recorded' ? 'Your message was saved. Agent responses may follow.' : 'The message’s delivery is not confirmed. It has not been resent.';
      const receipt = node('p','huddles-status',message); receipt.setAttribute('role','status'); form.append(receipt);
      if (pending.state === 'unknown' && typeof findSubmission === 'function') {
        const check = button('Check message status',()=>reconcile(id,draft)); check.disabled=draft.checking; form.append(check);
      }
    }
    inspector.append(form);
  }
  async function changeFollowing(id,following) {
    const token=generation,draft=draftFor(id); if(draft.following)return; draft.following=true; renderRoom();
    try {
      const result=await setFollowing(input({huddleId:id,following})); if(!valid(token))return;
      if(result?.following!==following)throw new Error('unconfirmed preference');
      summaries=summaries.map(item=>item.id===id?{...item,following}:item);
      if(selected===id&&room)room={...room,following}; status.textContent='';
    } catch(error) {
      if(!valid(token))return;
      if(denied(error)){revoke();return;}
      status.textContent='The update preference could not be confirmed. Refresh to check it.';
    } finally { if(valid(token)){draft.following=false;if(selected===id)renderRoom();else renderDirectory();} }
  }
  function applyReceipt(id,draft,result,pending) {
    const receipt = result?.requestId === pending.requestId ? stateOf(result.state) : 'unknown';
    if(receipt==='recorded') {
      readRequest++; // An earlier history read cannot restore a settled pending record.
      draft.text=''; draft.targets=[]; draft.pending=null;
      if(selected===id){status.textContent='Your message was saved. Agent responses may follow.';renderRoom();}
    } else if(receipt==='not_recorded') {
      readRequest++;
      draft.pending=null;
      if(selected===id){status.textContent='Your message was not saved. You can review it before sending.';renderRoom();}
    } else {pending.state='unknown';if(selected===id)renderRoom();}
  }
  async function submitMessage(id,draft) {
    if(!context||selected!==id||room?.canSend!==true||draft.pending||!draft.text.trim())return;
    const token=generation,pending={requestId:requestId(),text:draft.text.trim(),targetAgentIds:[...draft.targets],state:'sending'};
    draft.pending=pending;renderRoom();
    try {const result=await send(input({huddleId:id,requestId:pending.requestId,text:pending.text,targetAgentIds:[...pending.targetAgentIds]}));if(valid(token)&&draft.pending===pending)applyReceipt(id,draft,result,pending);}
    catch(error){if(!valid(token)||draft.pending!==pending)return;if(denied(error)){revoke();return;}pending.state='unknown';if(selected===id)renderRoom();}
  }
  async function reconcile(id,draft) {
    if(!context||draft.checking||draft.pending?.state!=='unknown'||typeof findSubmission!=='function')return;
    const token=generation,pending=draft.pending;draft.checking=true;renderRoom();
    try {const result=await findSubmission(input({huddleId:id,requestId:pending.requestId}));if(valid(token)&&draft.pending===pending)applyReceipt(id,draft,result,pending);}
    catch(error){if(valid(token)&&denied(error)){revoke();return;}}
    finally {if(valid(token)){draft.checking=false;if(selected===id)renderRoom();}}
  }
  return {
    async setContext(next) {
      if(destroyed)return;
      if(next?.key&&next.key===context?.key)return refresh();
      clear();if(!next)return;
      if(typeof next.key!=='string'||!next.key)throw new TypeError('An identity-bound context key is required.');
      context=Object.freeze({...next});refreshButton.disabled=false;await refresh();
    },
    refresh,inspect,close,revoke,
    destroy(){clear();destroyed=true;root.replaceChildren();}
  };
}
