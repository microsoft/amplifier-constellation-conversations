import {createVoice} from './voice.js';
import {presentMessages,appendMessageDetails,voiceLabel} from './message-presentation.mjs';
import {createHuddles} from './huddles.mjs';
import {createHuddleApi} from './huddles-api.mjs';
const $=id=>document.getElementById(id);
const ui={meta:null,session:null,items:[],conversation:null,view:null,eventSource:null,refreshing:false,refreshAgain:false,sending:false,draftTimer:null,searchTimer:null,saveChain:Promise.resolve(),refreshTimer:null,titleTimer:null,actionId:null,agents:[],people:[],files:[],historyCursor:null,historyTimer:null,roomTimer:null};
const viewId=localStorage.getItem('acv-view')||crypto.randomUUID();localStorage.setItem('acv-view',viewId);
const initials=name=>(name||'A').trim().split(/\s+/).map(p=>p[0]).slice(0,2).join('').toUpperCase();
const shortTime=at=>new Date(at).toLocaleTimeString([],{hour:'numeric',minute:'2-digit'});
const date=at=>new Date(at).toLocaleDateString([],{month:'short',day:'numeric'});
const el=(tag,className,text)=>{const n=document.createElement(tag);if(className)n.className=className;if(text!==undefined)n.textContent=text;return n;};
let huddleView=null,huddleContext=null,huddleRefresh=null,huddleRefreshAgain=false;
function clearHuddles(){huddleContext=null;huddleRefreshAgain=false;huddleView?.destroy();huddleView=null;$('conversation-huddles').hidden=true;$('conversation-huddles').open=false;}
async function refreshLinkedHuddles(){
  const c=ui.conversation;
  if(!c||!ui.session?.authenticated||$('app').hidden||c.id!==ui.view?.state.selected_conversation_id){clearHuddles();return;}
  const key=JSON.stringify([ui.session.owner,c.id]);
  if(huddleRefresh){huddleRefreshAgain=true;return;}
  try{
    if(huddleContext?.key!==key){
      clearHuddles();
      huddleContext=Object.freeze({key,owner:ui.session.owner,parentId:c.id});
      const callbacks=createHuddleApi({api,getContext:()=>huddleContext});
      huddleView=createHuddles({root:$('conversation-huddle-view'),...callbacks,list:async options=>{
        try{
          const result=await callbacks.list(options);
          if(result.current)result.items=[result.current,...result.items.filter(item=>item.id!==result.current.id)];
          if(huddleContext?.key===options.context.key){$('conversation-huddles').hidden=!result.items.length;$('conversation-huddle-count').textContent=result.items.length?`(${result.items.length})`:'';}
          return result;
        }catch(error){if(huddleContext?.key===options.context.key){$('conversation-huddles').hidden=false;$('conversation-huddle-count').textContent='';}throw error;}
      }});
      huddleRefresh=huddleView.setContext(huddleContext);
    }else huddleRefresh=huddleView.refresh();
    await huddleRefresh;
  }catch{
    if(huddleContext?.key===key){$('conversation-huddles').hidden=false;$('conversation-huddle-view').textContent='Huddles could not be opened. Your conversation is still available.';}
  }finally{
    huddleRefresh=null;
    if(huddleRefreshAgain){huddleRefreshAgain=false;void refreshLinkedHuddles();}
  }
}
$('conversation-huddles').ontoggle=()=>{if(!$('conversation-huddles').open)huddleView?.close();else void refreshLinkedHuddles();};
function renderText(target,text){
  target.classList.add('markdown');
  target.innerHTML=renderMarkdown(text);
}
function applyAction(includeScroll=false){const a=ui.view?.state.action;if(!a||a.id===ui.actionId||(!includeScroll&&a.target==='approval'))return;
  if(a.kind==='focus'){const node={composer:$('message'),search:$('search'),rename_title:$('title-input'),history_search:$('history-search'),invite_username:$('invite-username'),invite_agent_id:$('invite-agent-id'),approval:$('permissions')?.querySelector('button'),elicitation:$('elicitations')?.querySelector('input,select,button')}[a.target];if(!node||node.closest('[hidden]'))return;ui.actionId=a.id;node.focus();if(a.target==='rename_title')node.select();}
  else if(includeScroll&&a.kind==='scroll'&&['messages','permissions','elicitations'].includes(a.target)&&ui.view.state.panel==='conversation'){ui.actionId=a.id;requestAnimationFrame(()=>{$(a.target).scrollTop=a.position===-1?$(a.target).scrollHeight:Math.max(0,a.position||0);});}
}
function error(message){$('global-error').hidden=!message;$('global-error').querySelector('span').textContent=message||'';}
async function api(path,method='GET',body,options={}){
  const headers={};if(body!==undefined)headers['Content-Type']='application/json';if(method!=='GET'&&ui.session?.csrf)headers['X-CSRF-Token']=ui.session.csrf;
  const r=await fetch(path,{method,headers,credentials:'same-origin',body:body===undefined?undefined:JSON.stringify(body),signal:options.signal});
  let v;try{v=await r.json();}catch{throw new Error('The service returned an unreadable response. Please refresh and try again.');}
  if(!r.ok){const e=new Error(v.error||'The request could not be completed.');e.status=r.status;if(r.status===401&&path!='/auth/login')showLogin();throw e;}return v;
}
function showLogin(){clearHuddles();voice.close();ui.eventSource?.close();ui.eventSource=null;$('loading').hidden=true;$('app').hidden=true;$('login').hidden=false;$('username').value=ui.session?.owner||'';$('password').value='';}
async function start(){
  try{ui.session=await api('/auth/session');if(!ui.session.authenticated){showLogin();return;}await openApp();}catch(e){$('loading').hidden=true;$('app').hidden=true;$('login').hidden=false;$('login-error').hidden=false;$('login-error').textContent=e.message;}
}
async function openApp(){
  [ui.meta,ui.view]=await Promise.all([api('/api/v1/meta'),api(`/api/v1/views/${viewId}`)]);
  const desired=new URL(location.href).searchParams.get('conversation');
  if(desired&&desired!==ui.view.state.selected_conversation_id){try{await api(`/api/v1/conversations/${encodeURIComponent(desired)}`);await saveView({selected_conversation_id:desired,panel:'conversation',draft:'',attachment_ids:[],target_agent_ids:[],history_search:'',history_before:null,participants_open:false,share_history:false,elicitation_drafts:{}});}catch{history.replaceState({},'','/');}}
  $('loading').hidden=true;$('login').hidden=true;$('app').hidden=false;
  $('agent-name').textContent=ui.meta.agent.label;$('agent-initial').textContent=initials(ui.meta.agent.label);$('owner-name').textContent=ui.session.owner;$('agent-registry').hidden=!ui.meta.admin;$('check').hidden=!ui.meta.admin;$('view-label').textContent=`View ${viewId.slice(0,6)}`;$('message').placeholder=`Message ${ui.meta.agent.label}…`;
  $('connection-title').textContent=`Connected to ${ui.meta.agent.label}.`;$('connection-agent').textContent=ui.meta.agent.label;
  document.title=`${ui.meta.agent.label}: Conversations`;
  const details=[['ACP command',[ui.meta.agent.command,...(ui.meta.agent.args||[])].filter(Boolean).join(' ')||'Managed by the service owner'],['Working folder',ui.meta.agent.cwd],['Context provider',ui.meta.provider.id],['View ID',viewId],['Version',ui.meta.version]];
  $('connection-details').replaceChildren(...details.flatMap(([k,v])=>[el('dt',null,k),el('dd',null,v)]));
  applyView();await refresh();connectEvents();
}
function connectEvents(){
  ui.eventSource?.close();const events=new EventSource('/api/v1/events');ui.eventSource=events;
  events.addEventListener('change',()=>{$('connection-dot').classList.remove('offline');$('connection-dot').title='Service connected';queueRefresh();});
  events.onerror=()=>{$('connection-dot').classList.add('offline');$('connection-dot').title='Reconnecting to the service';};
}
function queueRefresh(){if(ui.refreshTimer)return;ui.refreshTimer=setTimeout(()=>{ui.refreshTimer=null;refresh().catch(e=>error(e.message));},200);}
async function refresh(){
  if($('app').hidden)return;if(ui.refreshing){ui.refreshAgain=true;return;}ui.refreshing=true;
  try{
    const [list,remote,agents,people]=await Promise.all([api('/api/v1/conversations?search='+encodeURIComponent(ui.view.state.search||'')),api(`/api/v1/views/${viewId}`),api('/api/v1/agents'),api('/api/v1/people')]);ui.items=list.items;ui.agents=agents.items;ui.people=people.items;
    if(remote.revision>ui.view.revision&&!ui.draftTimer&&!ui.searchTimer){ui.view=remote;applyView();}
    const id=ui.view.state.selected_conversation_id;
    if(id){try{ui.conversation=await api(`/api/v1/conversations/${encodeURIComponent(id)}`);}catch(e){if(e.status===404){ui.conversation=null;await saveView({selected_conversation_id:null});}else throw e;}}else ui.conversation=null;
    if(ui.conversation){
      const v=ui.view.state;const params=new URLSearchParams({limit:'100',search:v.history_search||''});if(v.history_before)params.set('before',v.history_before);
      const history=await api(`/api/v1/conversations/${id}/messages?${params}`);ui.conversation.messages=history.items;ui.historyCursor=history.next_cursor;
      ui.files=(await api(`/api/v1/conversations/${id}/attachments`)).items;
    }else{ui.files=[];ui.historyCursor=null;}
    renderList();renderChat();renderParticipants();void refreshLinkedHuddles();if(ui.view.state.panel==='activity')await renderActivity();
  }finally{ui.refreshing=false;if(ui.refreshAgain){ui.refreshAgain=false;queueRefresh();}}
}
function saveView(patch){
  ui.saveChain=ui.saveChain.catch(()=>{}).then(async()=>{
    let body={expected_revision:ui.view.revision,state:{...ui.view.state,...patch}};
    try{ui.view=await api(`/api/v1/views/${viewId}`,'PUT',body);}catch(e){if(e.status!==409)throw e;ui.view=await api(`/api/v1/views/${viewId}`);body={expected_revision:ui.view.revision,state:{...ui.view.state,...patch}};ui.view=await api(`/api/v1/views/${viewId}`,'PUT',body);}
    applyView();return ui.view;
  });return ui.saveChain;
}
function applyView(){
  const v=ui.view.state;
  $('voice-help').open=Boolean(v.voice_help_open);$('voice-read-aloud').checked=Boolean(v.voice_read_aloud);
  if(voice.state().enabled!==Boolean(v.voice_enabled))voice.enable(Boolean(v.voice_enabled));
  if(voice.state().readAloud!==Boolean(v.voice_read_aloud))voice.readAloud(Boolean(v.voice_read_aloud));
  $('chat-panel').hidden=v.panel!=='conversation';$('connection-panel').hidden=v.panel!=='connection';$('activity-panel').hidden=v.panel!=='activity';
  $('page-name').textContent={conversation:'Conversation',connection:'Agent connection',activity:'Activity'}[v.panel];
  $('connection-tab').classList.toggle('active',v.panel==='connection');$('activity-tab').classList.toggle('active',v.panel==='activity');
  $('app').classList.toggle('menu-open',Boolean(v.menu_open));$('connection-details-panel').open=Boolean(v.details_open);
  if(!ui.titleTimer&&$('title-input').value!==(v.title_draft||''))$('title-input').value=v.title_draft||'';
  if(v.dialog==='rename'&&!$('rename-dialog').open)$('rename-dialog').showModal();else if(v.dialog!=='rename'&&$('rename-dialog').open)$('rename-dialog').close();
  $('participants-panel').hidden=!v.participants_open||!ui.conversation||ui.conversation.read_only;
  if(!ui.historyTimer)$('history-search').value=v.history_search||'';
  if(!ui.roomTimer)$('connection-draft').value=v.connection_draft||'';
  $('share-history').checked=Boolean(v.share_history);$('agent-registry').open=Boolean(v.registry_open);
  applyAction();
  if(!ui.draftTimer&&$('message').value!==v.draft)$('message').value=v.draft;
  if(!ui.searchTimer&&$('search').value!==v.search)$('search').value=v.search;
  const next=v.selected_conversation_id?`/?conversation=${encodeURIComponent(v.selected_conversation_id)}`:'/';if(location.pathname+location.search!==next)history.replaceState({},'',next);
  updateSend();renderList();
}
async function selectConversation(id){
  clearHuddles();
  await flushDraft();await saveView({selected_conversation_id:id,panel:'conversation',draft:'',menu_open:false,attachment_ids:[],target_agent_ids:[],history_search:'',history_before:null,participants_open:false,share_history:false});await refresh();$('message').focus();
}
function renderList(){
  if(!ui.view)return;const query=($('search').value||'').toLocaleLowerCase();const list=ui.items;$('conversation-count').textContent=ui.items.length;
  $('conversation-list').replaceChildren(...list.map(c=>{
    const b=el('button','conversation-item'+(c.id===ui.view.state.selected_conversation_id?' active':''));b.type='button';b.setAttribute('aria-current',c.id===ui.view.state.selected_conversation_id?'page':'false');
    const wrap=el('span');wrap.append(el('strong',null,c.title),el('small',null,`${c.surface&&c.surface!=='web'?c.surface+' · ':''}${date(c.updated_at)}`));b.append(wrap);if(c.status==='working')b.append(el('span','busy'));
    b.onclick=()=>selectConversation(c.id).catch(e=>error(e.message));return b;
  }));
  if(!list.length)$('conversation-list').append(el('p','list-empty',query?'No matching conversations.':'Your conversations will appear here.'));
}
function renderChat(){
  const c=ui.conversation;const agent=ui.meta.agent.label;
  $('conversation-title').textContent=c?.title||'Pick up the conversation.';$('rename').hidden=!c||c.read_only||c.owner!==ui.session.owner;$('participants-toggle').hidden=!c||c.read_only;$('history-tools').hidden=!c;$('older-messages').hidden=!ui.historyCursor;$('latest-messages').hidden=!ui.view.state.history_before&&!ui.view.state.history_search;$('audience-label').textContent=c?.shared?'Shared room':'Private to you';$('conversation-eyebrow').textContent=c?.surface&&c.surface!=='web'?`${c.surface.toUpperCase()} CONVERSATION · READ ONLY`:'WEB CONVERSATION';
  const container=$('messages');const nearBottom=container.scrollHeight-container.scrollTop-container.clientHeight<120;const previousId=container.dataset.conversation;container.dataset.conversation=c?.id||'';
  const openDetails=new Set([...container.querySelectorAll('.message-details[open]')].map(n=>n.dataset.messageId));
  if(!c?.messages?.length){const welcome=el('div','welcome');welcome.append(el('span','avatar',initials(agent)),el('h2',null,`Talk with ${agent}.`),el('p',null,'Start something new, or pick up a thought from another place. You are talking to the same individual.'),el('p','welcome-note','Your agent can bring in relevant context from its authorized connections. Separate conversations keep their own audience and reply destination.'));container.replaceChildren(welcome);}
  else container.replaceChildren(...presentMessages(c.messages,{hasOlder:!!ui.historyCursor,hasNewer:!!ui.view.state.history_before,filtered:!!ui.view.state.history_search}).map(m=>{
    const n=el('article',`message ${m.role}${m.status==='streaming'?' streaming':''}`);n.dataset.messageId=m.id;n.dataset.messageIds=JSON.stringify(m.source_messages.map(x=>x.id));
    n.append(el('span','avatar',initials(m.actor||(m.role==='user'?ui.meta.owner:agent))));const body=el('div','message-body');const meta=el('div','message-meta');meta.append(el('strong',null,m.actor===ui.session.owner?'You':(m.actor||agent)));const t=el('time',null,shortTime(m.created_at));t.dateTime=new Date(m.created_at).toISOString();meta.append(t);body.append(meta);
    const sourceLabel=voiceLabel(m);if(sourceLabel)meta.append(el('span','message-source',sourceLabel));
    const text=el('div','message-content'+(!m.display_text?' empty-reply':''));renderText(text,m.display_text||(m.status==='streaming'?'Working…':'No reply was completed.'));body.append(text);for(const file of m.attachments||[]){const a=el('a','attachment-chip',`${file.name} · ${Math.ceil(file.bytes/1024)} KB`);a.href=`/api/v1/conversations/${c.id}/attachments/${file.id}`;a.download=file.name;if(['image/png','image/jpeg','image/webp','image/gif'].includes(file.mime_type)){const img=el('img','generated-image');img.src=a.href;img.alt=file.name;img.loading='lazy';body.append(img);}else if(['audio/wav','audio/mpeg'].includes(file.mime_type)){const audio=el('audio','generated-audio');audio.src=a.href;audio.controls=true;body.append(audio);}body.append(a);}if(m.status==='interrupted')body.append(el('p','message-interrupted','This turn was interrupted. The text above may be incomplete.'));
    appendMessageDetails(body,m);const details=body.querySelector('.message-details');if(details&&openDetails.has(m.id))details.open=true;
    if(m.role==='user')for(const d of c.deliveries?.filter(d=>m.source_messages.some(x=>x.id===d.message_id))||[]){
      const label=c.agents?.find(a=>a.id===d.agent_id)?.label||d.agent_id;
      const names={received:'Received · waiting to send',preparing:'Preparing',offering:'Considering whether to join',submitted:'Sent · acceptance pending',received_remote:'Received by agent',remoteQueued:'Queued by agent',incorporated:'Included in model context',completed:'Turn finished',promptRequired:'Queued for the next turn',held:'Agent chose to listen',notAccepted:'Not accepted · not retried',unknown:'Outcome unknown · not retried',ack_pending:'Review acknowledged · no replay',dismissed:'Continued without resending',cancelled:'Stopped before delivery'};
      const receipt=el('div','input-delivery',`${label}: ${names[d.state]||d.state}`);receipt.dataset.deliveryId=d.id;receipt.dataset.deliveryState=d.state;
      if(['unknown','notAccepted'].includes(d.state)&&m.actor===ui.session.owner){receipt.append(el('span',null,' Check prior work before continuing. '));const b=el('button','secondary','Continue without resending');b.type='button';b.disabled=c.status==='working';b.onclick=()=>resolveDelivery(d.id).catch(()=>{});receipt.append(b);}body.append(receipt);
    }n.append(body);return n;
  }));
  const visible=new Set(c?.messages?.map(m=>m.id)||[]),older=(c?.deliveries||[]).filter(d=>['unknown','notAccepted'].includes(d.state)&&!visible.has(d.message_id));
  $('delivery-review').hidden=!older.length;$('delivery-review').replaceChildren(...older.map(d=>{const b=el('button','secondary',`Review earlier message for ${c.agents?.find(a=>a.id===d.agent_id)?.label||d.agent_id}`);b.type='button';b.onclick=()=>showDelivery(d.id).catch(e=>error(e.message));return b;}));
  if(nearBottom||previousId!==(c?.id||''))container.scrollTop=container.scrollHeight;
  voice.observe(c?.messages,previousId!==(c?.id||''));
  const working=c?.status==='working',huddle=c?.huddle;const failed=c?.status==='error';$('turn-status').hidden=!working&&!failed&&!huddle;$('turn-status').classList.toggle('failed',failed);$('turn-status-text').textContent=huddle?(huddle.state==='stopped'?'This huddle is stopped. Its conversation remains available.':'This huddle is open. Inspecting it does not start or stop work.'):(c?.note||(working?`${agent} is working…`:''));$('stop').hidden=huddle?huddle.state!=='open':!working;$('stop').textContent=huddle?'Stop huddle':'Stop';
  $('composer').hidden=Boolean(c?.read_only);$('composer-hint').textContent=working?'You can add to the conversation while work continues.':'Enter to send · Shift + Enter for a new line';
  $('composer-footnote').textContent=c?.read_only?`This transcript comes from ${c.surface}. Continue there, or start a new web conversation with the same agent.`:huddle?'Everyone in this huddle can read your message. Share links to evidence here; files stay in their original location.':c?.shared?'Everyone here can read your message. A named recipient chooses which agent responds.':'A separate conversation with the same agent. Relevant context can follow through its authorized connections.';
  document.querySelector('label[for="file-upload"]').hidden=Boolean(huddle);$('message').maxLength=huddle?6000:60000;
  renderPermissions();renderElicitations();updateSend();
}
function renderPermissions(){
 const container=$('permissions');if(!container)return;
 const c=ui.conversation;container.replaceChildren(...(c?.permissions||[]).map(p=>{
  const card=el('section','permission-card');card.setAttribute('aria-label','Action needs your approval');card.append(el('h3',null,`${p.agent} needs your approval`),el('p',null,p.tool_call?.title||'Review the requested action'),el('small',null,`Expires ${shortTime(p.expires_at)}. This decision is private to you.`));
  const expanded=ui.view.state.permission_details===p.id;const toggle=el('button','secondary',expanded?'Hide action details':'Show action details');toggle.type='button';toggle.setAttribute('aria-expanded',String(expanded));toggle.onclick=()=>saveView({permission_details:expanded?null:p.id}).then(renderPermissions).catch(e=>error(e.message));card.append(toggle);
  const actions=el('div','permission-actions');for(const o of [...p.options,{optionId:null,name:'Cancel request',kind:'cancel'}]){
   const b=el('button','secondary',o.name+(o.kind.endsWith('_always')?' (remember this policy)':''));b.type='button';b.onclick=async()=>{b.disabled=true;try{await api(`/api/v1/conversations/${c.id}/permissions/${p.id}`,'POST',{option_id:o.optionId});await refresh();}catch(e){error(e.message);b.disabled=false;}};actions.append(b);
  }card.append(actions);if(expanded)card.append(el('pre','permission-detail',JSON.stringify(p.tool_call,null,2)));return card;
 }));applyAction(true);
}

function formOptions(field){const items=field.type==='array'?field.items:field;return (items?.enum||items?.oneOf||items?.anyOf||[]).map((v,i)=>typeof v==='string'?{value:v,label:items.enumNames?.[i]||v}:{value:v.const,label:v.title||v.const});}
function formValues(form){const value=Object.create(null);for(const input of form.querySelectorAll('[data-field]')){if(input.type==='checkbox')value[input.dataset.field]=input.checked;else if(input.multiple)value[input.dataset.field]=[...input.selectedOptions].map(o=>o.value);else if(input.value!==''||input.required)value[input.dataset.field]=input.type==='number'?Number(input.value):input.value;}return value;}
async function decideForm(id,action,content){const c=ui.conversation;if(!c?.elicitations?.some(p=>p.id===id))throw new Error('This form is no longer active.');await api(`/api/v1/conversations/${c.id}/elicitations/${id}`,'POST',{action,...(action==='accept'?{content}: {})});const drafts={...(ui.view.state.elicitation_drafts||{})};delete drafts[id];await saveView({elicitation_drafts:drafts});await refresh();}
function renderElicitations(force=false){
 const container=$('elicitations');if(!container)return;const forms=ui.conversation?.elicitations||[];
 $('chat-panel').classList.toggle('has-elicitation',forms.length>0);const signature=JSON.stringify(forms);if(container.dataset.signature!==signature){container.dataset.signature=signature;
 container.replaceChildren(...forms.map(p=>{
  if(p.mode==='url'){
   const card=el('section','permission-card elicitation-form elicitation-url');card.dataset.requestId=p.id;card.setAttribute('aria-label',`${p.agent} requests an external step`);
   card.append(el('h3',null,`${p.agent} requests an external step`),el('p',null,p.message),el('strong',null,p.host),el('p','external-url',p.url),el('small',null,'Continue opens this address in a separate tab. Sign in there; no credentials are entered in this conversation.'));
   const actions=el('div','permission-actions');const link=el('a','primary','Open and continue');link.href=p.url;link.target='_blank';link.rel='noopener noreferrer';link.onclick=()=>decideForm(p.id,'accept').catch(e=>error(e.message));actions.append(link);
   for(const action of ['decline','cancel']){const b=el('button','secondary',action==='decline'?'Decline':'Cancel request');b.type='button';b.onclick=()=>decideForm(p.id,action).catch(e=>error(e.message));actions.append(b);}card.append(actions);return card;
  }
  const form=el('form','permission-card elicitation-form');form.dataset.requestId=p.id;form.setAttribute('aria-label',`${p.agent} has a question`);
  form.append(el('h3',null,`${p.agent} has a question`),el('p',null,p.message),el('small',null,`Your answer goes to this agent. Review it before sending. Expires ${shortTime(p.expires_at)}.`));
  for(const [name,field] of Object.entries(p.schema.properties||{})){
   const label=el('label','elicitation-field',field.title||name);const choices=formOptions(field);let input;
   if(choices.length){input=el('select');input.multiple=field.type==='array';if(!input.multiple)input.append(new Option('Choose…',''));for(const o of choices)input.append(new Option(o.label,o.value));}
   else{input=el('input');input.type=field.type==='boolean'?'checkbox':['integer','number'].includes(field.type)?'number':({email:'email',uri:'url',date:'date'}[field.format]||'text');if(input.type==='number'){input.step=field.type==='integer'?'1':'any';if(field.minimum!==undefined)input.min=field.minimum;if(field.maximum!==undefined)input.max=field.maximum;}if(field.minLength!==undefined)input.minLength=field.minLength;if(field.maxLength!==undefined)input.maxLength=field.maxLength;else if(input.type==='text')input.maxLength=16384;if(field.format==='date-time')input.placeholder='2026-11-02T09:00:00-08:00';}
   input.dataset.field=name;input.name=name;input.required=(p.schema.required||[]).includes(name)&&input.type!=='checkbox';input.autocomplete='off';label.append(input);if(field.description)label.append(el('small',null,field.description));form.append(label);
  }
  const actions=el('div','permission-actions');for(const [action,text]of[['accept','Send answer'],['decline','Decline'],['cancel','Cancel request']]){const button=el('button',action==='accept'?'primary':'secondary',text);button.type=action==='accept'?'submit':'button';if(action!=='accept')button.onclick=()=>decideForm(p.id,action).catch(e=>error(e.message));actions.append(button);}form.append(actions);
  form.oninput=()=>{const drafts={...(ui.view.state.elicitation_drafts||{}),[p.id]:formValues(form)};saveView({elicitation_drafts:drafts}).catch(e=>error(e.message));};
  form.onsubmit=e=>{e.preventDefault();if(form.reportValidity())decideForm(p.id,'accept',formValues(form)).catch(e=>error(e.message));};return form;
 }));}
 for(const p of forms){const form=[...container.children].find(f=>f.dataset.requestId===p.id);const draft=ui.view.state.elicitation_drafts?.[p.id]||{};for(const input of form.querySelectorAll('[data-field]')){if(!force&&document.activeElement===input)continue;const field=p.schema.properties[input.dataset.field];const value=Object.prototype.hasOwnProperty.call(draft,input.dataset.field)?draft[input.dataset.field]:field.default;if(input.type==='checkbox')input.checked=Boolean(value);else if(input.multiple){for(const o of input.options)o.selected=Array.isArray(value)&&value.includes(o.value);}else input.value=value===undefined?'':String(value);}}
 applyAction(true);
}
function updateSend(){const stale=Boolean(ui.view?.state.selected_conversation_id&&ui.conversation?.id!==ui.view.state.selected_conversation_id);$('file-upload').disabled=stale||Boolean(ui.conversation?.huddle);$('send').disabled=ui.sending||stale||(!$('message').value.trim()&&!ui.view?.state.attachment_ids?.length)||ui.conversation?.read_only||Boolean(ui.conversation?.huddle&&ui.conversation.huddle.state!=='open');}
async function flushDraft(){if(ui.draftTimer){clearTimeout(ui.draftTimer);ui.draftTimer=null;await saveView({draft:$('message').value});}}
async function newConversation(){await flushDraft();const c=await api('/api/v1/conversations','POST',{title:'New conversation'});await selectConversation(c.id);}
async function sendMessage(e){
  e?.preventDefault();if($('send').disabled)return false;const text=$('message').value.trim();ui.sending=true;updateSend();error('');
  try{
    await flushDraft();let c=ui.conversation;
    if(!c){c=await api('/api/v1/conversations','POST',{title:text.split('\n')[0].slice(0,70)});await saveView({selected_conversation_id:c.id,panel:'conversation'});}
    if(c.title==='New conversation'&&c.owner===ui.session.owner)await api(`/api/v1/conversations/${c.id}`,'PATCH',{title:text.split('\n')[0].slice(0,70)});
    const target_agent_ids=[...(ui.view.state.target_agent_ids||[])],attachment_ids=[...(ui.view.state.attachment_ids||[])];
    if(c.huddle&&(attachment_ids.length||new TextEncoder().encode(text).byteLength>6000))throw new Error('Use a shorter huddle message and links to evidence instead of attachments.');
    const bytes=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(JSON.stringify({room:c.id,text,target_agent_ids,attachment_ids})));
    const digest=[...new Uint8Array(bytes)].map(b=>b.toString(16).padStart(2,'0')).join('');
    const request_id=ui.view.state.draft_request_sha256===digest&&ui.view.state.draft_request_id?ui.view.state.draft_request_id:crypto.randomUUID();
    // Persist the source ID before transmitting, including across reload after
    // a lost HTTP response. Equal text sent later gets a different source ID.
    await saveView({draft_request_id:request_id,draft_request_sha256:digest});
    await api(`/api/v1/conversations/${c.id}/messages`,'POST',{text,request_id,target_agent_ids,attachment_ids});
    const unchanged=$('message').value.trim()===text;
    if(unchanged)$('message').value='';
    await saveView({...(unchanged?{draft:'',attachment_ids:[]}:{}),draft_request_id:'',draft_request_sha256:'',history_search:'',history_before:null});await refresh();return true;
  }catch(e){error(e.message);return false;}finally{ui.sending=false;updateSend();$('message').focus();}
}
async function showDelivery(id){const d=ui.conversation?.deliveries?.find(d=>d.id===id);if(!d)throw new Error('This delivery is no longer available. Refresh the conversation.');await saveView({history_search:'',history_before:d.source_sequence+1,panel:'conversation'});await refresh();}
async function resolveDelivery(id){try{if(!ui.conversation)throw new Error('Open the conversation first.');await api(`/api/v1/conversations/${ui.conversation.id}/deliveries/${encodeURIComponent(id)}/resolve`,'POST',{disposition:'proceed_without_replay'});await refresh();}catch(e){error(e.message);throw e;}}
function renderParticipants(){
  const c=ui.conversation,v=ui.view.state,owner=c?.owner===ui.session.owner;
  $('participants-panel').hidden=!c||c.read_only||!v.participants_open;$('invite-controls').hidden=!owner;
  $('agent-invite-controls').hidden=Boolean(c?.huddle);
  $('participants-toggle').setAttribute('aria-expanded',String(Boolean(v.participants_open)));
  const rows=[];
  for(const person of c?.members||[]){const row=el('div','participant-row');row.append(el('span',null,person.username+(person.role==='owner'?' · room owner':'')));if(owner&&person.role!=='owner'){const b=el('button','remove-participant','Remove');b.onclick=()=>api(`/api/v1/conversations/${c.id}/members/${encodeURIComponent(person.username)}`,'DELETE').then(refresh).catch(e=>error(e.message));row.append(b);}rows.push(row);}
  for(const agent of c?.agents||[]){const row=el('div','participant-row');row.append(el('span',null,agent.label+' · agent'));if(owner&&!c?.huddle){const b=el('button','remove-participant','Remove');b.onclick=()=>api(`/api/v1/conversations/${c.id}/agents/${encodeURIComponent(agent.id)}`,'DELETE').then(refresh).catch(e=>error(e.message));row.append(b);}rows.push(row);}
  $('participant-list').replaceChildren(...rows);
  const options=(node,items,key,label,prompt,selected)=>{node.replaceChildren(new Option(prompt,''),...items.map(a=>new Option(a[label]??a,a[key]??a)));node.value=selected||'';};
  options($('invite-username'),ui.people.filter(p=>!c?.members?.some(m=>m.username===p)),null,null,'Choose a person…',v.invite_username);
  options($('invite-agent-id'),ui.agents.filter(a=>!c?.agents?.some(m=>m.id===a.id)),'id','label','Choose an agent…',v.invite_agent_id);
  options($('reply-target'),c?.agents||[],'id','label',(c?.members?.length||0)+(c?.agents?.length||0)>2?'Everyone here · agents decide when to help':'Your agent',v.target_agent_ids?.[0]);
  if((v.target_agent_ids||[]).length>1){const label=(c?.agents||[]).filter(a=>v.target_agent_ids.includes(a.id)).map(a=>a.label).join(', ');$('reply-target').add(new Option(label,'__selected'));$('reply-target').value='__selected';}
  const named=(c?.agents||[]).filter(a=>(v.target_agent_ids||[]).includes(a.id));$('message').placeholder=named.length===1?`Message ${named[0].label}…`:(c?.shared?'Message this conversation…':`Message ${ui.meta.agent.label}…`);
  $('room-explainer').textContent=c?.status==='working'?'Wait for this turn to finish before changing the audience.':`${c?.members?.length||0} ${(c?.members?.length||0)===1?'person':'people'} and ${c?.agents?.length||0} ${(c?.agents?.length||0)===1?'agent':'agents'} can participate here.`;
  for(const id of ['invite-person','invite-agent'])$(id).disabled=c?.status==='working'||!v.share_history;
  $('draft-attachments').replaceChildren(...(v.attachment_ids||[]).map(id=>{const f=ui.files.find(f=>f.id===id);const chip=el('span','attachment-chip',f?.name||'Attached file');const b=el('button',null,'×');b.type='button';b.setAttribute('aria-label','Remove '+(f?.name||'attachment'));b.onclick=()=>api(`/api/v1/conversations/${c.id}/attachments/${id}`,'DELETE').then(()=>saveView({attachment_ids:v.attachment_ids.filter(x=>x!==id)})).then(refresh).catch(e=>error(e.message));chip.append(b);return chip;}));
  updateSend();
}
async function addParticipant(kind){const c=ui.conversation;if(!c)return;const v=ui.view.state;const value=kind==='members'?v.invite_username:v.invite_agent_id;if(!value){error('Choose someone to add first.');return;}await api(`/api/v1/conversations/${c.id}/${kind}`,'POST',{[kind==='members'?'username':'agent_id']:value,share_history:Boolean(v.share_history)});await saveView({invite_username:'',invite_agent_id:'',share_history:false,target_agent_ids:[]});await refresh();}
$('participants-toggle').onclick=()=>saveView({participants_open:!ui.view.state.participants_open}).then(renderParticipants).catch(e=>error(e.message));
$('invite-username').onchange=()=>saveView({invite_username:$('invite-username').value}).catch(e=>error(e.message));
$('invite-agent-id').onchange=()=>saveView({invite_agent_id:$('invite-agent-id').value}).catch(e=>error(e.message));
$('share-history').onchange=()=>saveView({share_history:$('share-history').checked}).then(renderParticipants).catch(e=>error(e.message));
$('invite-person').onclick=()=>addParticipant('members').catch(e=>error(e.message));
$('invite-agent').onclick=()=>addParticipant('agents').catch(e=>error(e.message));
$('reply-target').onchange=()=>{if($('reply-target').value==='__selected')return;saveView({target_agent_ids:$('reply-target').value?[$('reply-target').value]:[]}).catch(e=>error(e.message));};
$('history-search').oninput=()=>{clearTimeout(ui.historyTimer);ui.historyTimer=setTimeout(()=>{ui.historyTimer=null;saveView({history_search:$('history-search').value,history_before:null}).then(refresh).catch(e=>error(e.message));},300);};
$('older-messages').onclick=()=>saveView({history_before:ui.historyCursor}).then(refresh).catch(e=>error(e.message));
$('latest-messages').onclick=()=>{ $('history-search').value='';saveView({history_before:null,history_search:''}).then(refresh).catch(e=>error(e.message));};
$('file-upload').onchange=async()=>{try{if(!ui.conversation)await newConversation();const files=Array.from($('file-upload').files);if(files.length+(ui.view.state.attachment_ids||[]).length>4)throw new Error('Attach up to four files to one message.');for(const file of files){if(file.size>2*1024*1024)throw new Error('Each file must be at most 2 MB.');let mime=file.type;const ext=file.name.split('.').pop().toLowerCase();mime=({md:'text/markdown',txt:'text/plain',csv:'text/csv',json:'application/json',pdf:'application/pdf'})[ext]||mime;const data=await new Promise((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(String(reader.result).split(',')[1]);reader.onerror=()=>reject(new Error('This file could not be read.'));reader.readAsDataURL(file);});const upload=await api(`/api/v1/conversations/${ui.conversation.id}/attachments`,'POST',{name:file.name,mime_type:mime,data});await saveView({attachment_ids:[...(ui.view.state.attachment_ids||[]),upload.id]});}await refresh();}catch(e){error(e.message);}finally{$('file-upload').value='';}};
$('agent-registry').addEventListener('toggle',()=>{if(ui.view&&$('agent-registry').open!==Boolean(ui.view.state.registry_open))saveView({registry_open:$('agent-registry').open}).catch(e=>error(e.message));});
$('connection-draft').oninput=()=>{clearTimeout(ui.roomTimer);ui.roomTimer=setTimeout(()=>{ui.roomTimer=null;saveView({connection_draft:$('connection-draft').value}).catch(e=>error(e.message));},400);};
$('save-connection').onclick=async()=>{try{const v=JSON.parse($('connection-draft').value);const {id,...config}=v;config.env={};config.mcp_servers=[];config.timeout_seconds??=900;config.args??=[];await api('/api/v1/agents','POST',{id,config});$('connection-save-result').textContent='Connection saved. Add this agent to a conversation when you are ready.';await saveView({connection_draft:''});await refresh();}catch(e){$('connection-save-result').textContent=e.message;}};
const activityLabels={'input.received':'Message received','input.acknowledged':'Delivery reviewed','conversation.created':'Conversation started','conversation.removed':'Web conversation removed','turn.started':'Agent started working','turn.completed':'Reply completed','turn.interrupted':'Turn interrupted','tool.progress':'Tool progress','permission.declined':'Action needed approval','permission.requested':'Action awaiting approval','permission.decided':'Approval reviewed','context.read':'Context retrieved','session.rebuilt':'Working context rebuilt','turn.passed':'Agent chose to listen','room.member_added':'Person joined','room.member_removed':'Person removed','room.agent_added':'Agent joined','room.agent_removed':'Agent removed','agent.configured':'Agent connection saved','configuration.saved':'Connection settings saved','auth.signed_in':'Signed in','auth.signed_out':'Signed out','auth.failed':'Unsuccessful sign-in','conversation.imported':'External conversation updated'};
async function renderActivity(){const {items}=await api('/api/v1/activity');$('activity-list').replaceChildren(...items.map(a=>{const row=el('article','activity-item');row.append(el('time',null,shortTime(a.at)));const body=el('div');body.append(el('strong',null,activityLabels[a.kind]||a.kind));const c=ui.items.find(c=>c.id===a.conversation_id);const detail=a.data.title||a.data.reason||a.data.note||(a.data.operation?`Requested ${a.data.operation} context`:'');if(c){const p=el('p');const link=el('button',null,c.title);link.onclick=()=>selectConversation(c.id).catch(e=>error(e.message));p.append(link);body.append(p);}if(detail)body.append(el('p',null,detail));row.append(body);return row;}));if(!items.length)$('activity-list').append(el('p','list-empty','Agent activity will appear when you start a conversation.'));}
$('login-form').addEventListener('submit',async e=>{e.preventDefault();$('sign-in').disabled=true;$('login-error').hidden=true;try{ui.session=await api('/auth/login','POST',{username:$('username').value,password:$('password').value});$('password').value='';await openApp();}catch(e){$('login-error').textContent=e.message;$('login-error').hidden=false;}finally{$('sign-in').disabled=false;}});
$('sign-out').onclick=async()=>{try{await api('/auth/logout','POST',{});ui.session=null;showLogin();}catch(e){error(e.message);}};
$('new-conversation').onclick=()=>newConversation().catch(e=>error(e.message));
$('composer').addEventListener('submit',sendMessage);
$('message').addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing){e.preventDefault();sendMessage();}});
$('message').addEventListener('input',()=>{updateSend();clearTimeout(ui.draftTimer);ui.draftTimer=setTimeout(()=>{ui.draftTimer=null;saveView({draft:$('message').value}).catch(e=>error(e.message));},450);});
$('search').addEventListener('input',()=>{renderList();clearTimeout(ui.searchTimer);ui.searchTimer=setTimeout(()=>{ui.searchTimer=null;saveView({search:$('search').value}).then(refresh).catch(e=>error(e.message));},350);});
$('connection-tab').onclick=()=>saveView({panel:'connection'}).catch(e=>error(e.message));
$('activity-tab').onclick=()=>saveView({panel:'activity'}).then(renderActivity).catch(e=>error(e.message));
$('stop').onclick=async()=>{if(!ui.conversation)return;try{await api(`/api/v1/conversations/${ui.conversation.id}/cancel`,'POST',{});await refresh();}catch(e){error(e.message);}};
$('check').onclick=async()=>{$('check').disabled=true;$('check-result').textContent='Opening the connection…';try{const result=await api('/api/v1/check','POST',{});$('check-result').textContent=`Connected${result.agent_info?.name?' · '+result.agent_info.name:''}. No model prompt was sent.`;}catch(e){$('check-result').textContent=e.message;}finally{$('check').disabled=false;}};
$('rename').onclick=()=>saveView({dialog:'rename',title_draft:ui.conversation?.title||'',action:{id:crypto.randomUUID(),kind:'focus',target:'rename_title'}}).catch(e=>error(e.message));
$('rename-cancel').onclick=()=>saveView({dialog:null,title_draft:''}).catch(e=>error(e.message));
$('rename-dialog').addEventListener('cancel',e=>{e.preventDefault();$('rename-cancel').click();});
$('title-input').addEventListener('input',()=>{clearTimeout(ui.titleTimer);ui.titleTimer=setTimeout(()=>{ui.titleTimer=null;saveView({title_draft:$('title-input').value}).catch(e=>error(e.message));},300);});
$('connection-details-panel').addEventListener('toggle',()=>{if(ui.view&&$('connection-details-panel').open!==Boolean(ui.view.state.details_open))saveView({details_open:$('connection-details-panel').open}).catch(e=>error(e.message));});
$('rename-form').addEventListener('submit',async e=>{e.preventDefault();try{await api(`/api/v1/conversations/${ui.conversation.id}`,'PATCH',{title:$('title-input').value});clearTimeout(ui.titleTimer);ui.titleTimer=null;await saveView({dialog:null,title_draft:''});await refresh();}catch(e){error(e.message);}});
$('dismiss-error').onclick=()=>error('');
$('menu').onclick=()=>saveView({menu_open:!ui.view.state.menu_open}).catch(e=>error(e.message));
window.addEventListener('online',()=>{if(!ui.eventSource&&!$('app').hidden)connectEvents();queueRefresh();});
setInterval(async()=>{if($('app').hidden)return;try{const session=await api('/auth/session');if(!session.authenticated){ui.session=session;showLogin();}}catch{}},30000);
async function voiceAction(command){
  if(!ui.session?.authenticated||$('app').hidden)throw new Error('Sign in first.');
  const c=ui.conversation;
  switch(command.action){
    case 'elicitation_open_url':{const p=c?.elicitations?.find(p=>p.id===command.id&&p.mode==='url');if(!p)throw new Error('This request is no longer active.');window.open(p.url,'_blank','noopener,noreferrer');await decideForm(p.id,'accept');break;}
    case 'elicitation_draft':{const id=command.id;if(!c?.elicitations?.some(p=>p.id===id))throw new Error('This form is no longer active.');await saveView({elicitation_drafts:{...(ui.view.state.elicitation_drafts||{}),[id]:command.content}});renderElicitations(true);break;}
    case 'elicitation_submit':case 'elicitation_decline':case 'elicitation_cancel':{await decideForm(command.id,({elicitation_submit:'accept',elicitation_decline:'decline',elicitation_cancel:'cancel'})[command.action],command.content??ui.view.state.elicitation_drafts?.[command.id]??{});break;}
    case 'dictate':{const text=[$('message').value,command.text].filter(Boolean).join(' ');if(text.length>60000)throw new Error('The draft is full. Send or clear it first.');$('message').value=text;await saveView({draft:text});updateSend();break;}
    case 'delivery_show':await showDelivery(command.id);break;
    case 'delivery_resolve':await resolveDelivery(command.id);break;
    case 'send':if(!await sendMessage())throw new Error('Message was not sent. Check the draft, connection, or current agent response.');break;
    case 'clear':clearTimeout(ui.draftTimer);ui.draftTimer=null;await saveView({draft:''});break;
    case 'new':await newConversation();break;
    case 'cancel':if(c)await api(`/api/v1/conversations/${c.id}/cancel`,'POST',{});await refresh();break;
    case 'activity':case 'connection':case 'conversation':await saveView({panel:command.action});await refresh();break;
    case 'participants':case 'hide_participants':await saveView({participants_open:command.action==='participants'});renderParticipants();break;
    case 'open':{const matches=ui.items.filter(x=>x.title.toLowerCase()===command.text.toLowerCase());if(matches.length!==1)throw new Error('Say the exact title of one conversation in the list.');await selectConversation(matches[0].id);break;}
    case 'target':{if(!c)throw new Error('Open a conversation first.');const matches=(c.agents||[]).filter(x=>x.label.toLowerCase()===command.text.toLowerCase());const everyone=command.text.toLowerCase()==='everyone';if(!everyone&&matches.length!==1)throw new Error('Say the name of one agent in this conversation, or everyone.');await saveView({target_agent_ids:everyone?[]:[matches[0].id]});renderParticipants();break;}
    case 'rename':if(!c)throw new Error('Open a conversation first.');await api(`/api/v1/conversations/${c.id}`,'PATCH',{title:command.text});await refresh();break;
    case 'approval_details':case 'hide_approval_details':case 'read_approval':case 'approve_once':case 'reject_once':{
      const permissions=c?.permissions||[];if(permissions.length!==1)throw new Error(permissions.length?'More than one action needs approval. Select the intended action on screen.':'There is no action waiting for your approval.');
      const permission=permissions[0];
      if(command.action==='read_approval')return `${permission.agent} requests: ${permission.tool_call?.title||'an action'}. Say Workshop, allow once, or Workshop, reject once.`;
      if(command.action.endsWith('details')){await saveView({permission_details:command.action==='approval_details'?permission.id:null});renderPermissions();break;}
      const option=permission.options.filter(x=>x.kind===(command.action==='approve_once'?'allow_once':'reject_once'));
      if(option.length!==1)throw new Error('This action has different approval choices. Review them on screen.');
      await api(`/api/v1/conversations/${c.id}/permissions/${permission.id}`,'POST',{option_id:option[0].optionId});await refresh();break;
    }
    case 'read':{const text=[...(c?.messages||[])].reverse().find(x=>x.role==='assistant'&&x.status==='complete')?.text;if(!text)throw new Error('There is no completed reply to read yet.');return text;}
    default:throw new Error('Unknown view command.');
  }
}
const voice=createVoice({perform:voiceAction,status:s=>{$('voice-status').textContent=s.text;const active=s.enabled&&s.armed;$('voice-toggle').textContent=active?'Stop voice':'Start voice';$('voice-toggle').setAttribute('aria-pressed',String(active));},
  onEnabled:value=>{if(ui.view&&ui.session?.authenticated&&!$('app').hidden&&Boolean(ui.view.state.voice_enabled)!==value)saveView({voice_enabled:value}).catch(e=>error(e.message));},
  onReadAloud:value=>{if(ui.view&&Boolean(ui.view.state.voice_read_aloud)!==value)saveView({voice_read_aloud:value}).catch(e=>error(e.message));}});
$('voice-toggle').onclick=()=>voice.enable(!voice.state().armed||!voice.state().enabled,true);
$('voice-read-aloud').onchange=()=>voice.readAloud($('voice-read-aloud').checked);
$('voice-help').addEventListener('toggle',()=>{if(ui.view&&Boolean(ui.view.state.voice_help_open)!==$('voice-help').open)saveView({voice_help_open:$('voice-help').open}).catch(e=>error(e.message));});
window.agentConversationsControl={perform:voiceAction,voice,view:()=>structuredClone(ui.view),setView:saveView};
window.addEventListener('pagehide',()=>voice.close());
if('serviceWorker' in navigator)navigator.serviceWorker.register('/sw.js').catch(()=>{});
start();
