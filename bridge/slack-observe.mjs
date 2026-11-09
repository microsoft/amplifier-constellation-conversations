/** Read-only, fixed-channel Slack observations. Retrieved messages never authorize actions. */
import {readFile} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {realpathSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {z} from 'zod/v4';
import {makeSlackClient} from './slack-action.mjs';

const MAX_INPUT=65536,MAX_OUTPUT=131072;
const id=z.string().min(1).refine(s=>Buffer.byteLength(s)<=160&&!/[\x00-\x1f]/.test(s));
const ts=z.string().regex(/^\d{10,16}\.\d{1,12}$/);
const inputSchema=z.object({version:z.literal(1),cursor:z.object({seen:z.array(z.object({id,revision:z.string().regex(/^[a-f0-9]{64}$/)}).strict()).max(64)}).strict().nullable().optional(),
  actions:z.array(z.object({request_id:z.string().uuid(),receipt:z.record(z.string(),z.unknown())}).strict()).max(4).default([]),
}).strict().refine(v=>new Set(v.actions.map(a=>a.request_id)).size===v.actions.length);
const configSchema=z.object({slack_home:z.string().refine(isAbsolute),connection_id:id,
  channel_id:z.string().regex(/^C[A-Z0-9]{8,32}$/),expected_bot_user_id:z.string().regex(/^U[A-Z0-9]{8,32}$/),
}).strict();
/** @param {unknown} value @returns {Record<string,any>} */
function object(value){return value&&typeof value==='object'&&!Array.isArray(value)?/** @type {Record<string,any>} */(value):{};}
/** @param {unknown} value */
const revision=value=>createHash('sha256').update(JSON.stringify(value)).digest('hex');
/** @param {string} path */
async function readJson(path){const bytes=await readFile(path);if(bytes.length>32768)throw new Error('config_limit');return JSON.parse(bytes.toString('utf8'));}
/** @param {typeof fetch} fetchImpl @param {AbortSignal} [deadline] */
function boundedFetch(fetchImpl,deadline){
  /** @type {import('@slack/web-api').FetchFunction} */
  return async(url,init)=>{
    const response=await fetchImpl(url,{...init,...(deadline?{signal:AbortSignal.any([deadline,...(init?.signal?[init.signal]:[])])}:{})});
    if(!response.body)return response;
    const chunks=[];let size=0;const reader=response.body.getReader();
    try{for(;;){const {value,done}=await reader.read();if(done)break;size+=value.length;if(size>524288)throw new Error('response_limit');chunks.push(value);}}
    catch(error){await reader.cancel().catch(()=>{});throw error;}
    return new Response(Buffer.concat(chunks),{status:response.status,statusText:response.statusText,headers:response.headers});
  };
}
/** @param {string} token @param {typeof fetch} [fetchImpl] @param {AbortSignal} [deadline] */
export const makeSlackObserverClient=(token,fetchImpl=fetch,deadline)=>makeSlackClient(token,boundedFetch(fetchImpl,deadline));
/** @param {string} token @param {AbortSignal} deadline */
const defaultClient=(token,deadline)=>makeSlackObserverClient(token,fetch,deadline);
/** @template T @param {Promise<T>} operation @param {AbortSignal} deadline @returns {Promise<T>} */
function beforeDeadline(operation,deadline){
  if(deadline.aborted)return Promise.reject(new Error('observation_deadline'));
  return new Promise((resolve,reject)=>{
    const stop=()=>reject(new Error('observation_deadline'));deadline.addEventListener('abort',stop,{once:true});
    operation.then(resolve,reject).finally(()=>deadline.removeEventListener('abort',stop));
  });
}

/** @param {{configPath:string,input:unknown}} options
 * @param {(token:string,deadline:AbortSignal)=>ReturnType<typeof makeSlackClient>} [createClient]
 * @param {string} [observedAt]
 */
export async function observeSlack({configPath,input},createClient=defaultClient,observedAt=new Date().toISOString()){
  const deadline=AbortSignal.timeout(9500);
  /** @type {ReturnType<typeof inputSchema.parse>|undefined} */ let request;
  /** @type {string|null} */ let connection=null;
  /** @type {ReturnType<typeof configSchema.parse>|undefined} */ let config;
  /** @param {ReturnType<typeof inputSchema.parse>['actions'][number]} action @param {Record<string,unknown>} [extra] */
  const unknown=(action,extra={})=>{
    const value={request_id:action.request_id,connection_id:connection,state:'unknown',scope:'message_delivery',verified:false,
      audience:{kind:'channel',experience:'slack',channel_id:config?.channel_id,connection_id:connection},
      provenance:{kind:'slack_channel_history',authority:false},...extra};
    return {...value,revision:revision(value),observed_at:observedAt};
  };
  try{
    if(!isAbsolute(configPath)||Buffer.byteLength(JSON.stringify(input))>MAX_INPUT)throw new Error('invalid_input');
    request=inputSchema.parse(input);config=configSchema.parse(await readJson(configPath));connection=config.connection_id;
    const token=object(await readJson(join(config.slack_home,'secrets','slack.json'))).bot_token;
    if(typeof token!=='string'||!token.startsWith('xoxb-')||token.length>8192||/\s/.test(token))throw new Error('credentials_unavailable');
    const client=createClient(token,deadline),identity=await beforeDeadline(client.auth.test(),deadline);
    if(identity.ok!==true||identity.user_id!==config.expected_bot_user_id)throw new Error('identity_mismatch');
    const history=await beforeDeadline(client.conversations.history({channel:config.channel_id,limit:32,include_all_metadata:false}),deadline);
    if(history.ok!==true||!Array.isArray(history.messages)||history.messages.length>32)throw new Error('invalid_history');
    const audience={kind:'channel',experience:'slack',channel_id:config.channel_id,connection_id:connection};
    /** @type {Array<Record<string,any>>} */ const events=[];let omittedEvents=0;
    for(const message of history.messages){
      if(message.type!=='message'||!ts.safeParse(message.ts).success||typeof message.text!=='string'||!id.safeParse(message.user).success)continue;
      if(!message.text.trim()||Buffer.byteLength(message.text)>4000){omittedEvents++;continue;}
      const event={id:`slack:${config.channel_id}:${message.ts}`,connection_id:connection,
        occurred_at:new Date(Number(message.ts)*1000).toISOString(),kind:'message',actor:message.user,audience,
        provenance:{kind:'slack_channel_history',channel_id:config.channel_id,message_ts:message.ts,
          edited_ts:message.edited?.ts??null,thread_ts:message.thread_ts??null,authority:false},text:message.text};
      if(Buffer.byteLength(JSON.stringify(event))<=7800)events.push({...event,revision:revision(event)});else omittedEvents++;
    }
    events.sort((a,b)=>a.occurred_at.localeCompare(b.occurred_at));
    const previous=new Map((request.cursor?.seen??[]).map(row=>[row.id,row.revision]));
    const fresh=events.filter(e=>previous.get(e.id)!==e.revision);
    const gap=!!request.cursor&&history.has_more===true&&events.length>0&&![...previous.keys()].some(key=>events.some(e=>e.id===key));
    const seen=new Map(previous);for(const event of events){seen.delete(event.id);seen.set(event.id,event.revision);}
    const actions=[];
    for(const action of request.actions){
      const original=action.receipt,receipt=object(original.receipt);
      if(original.request_id!==action.request_id||original.state!=='completed'||original.scope!=='message_delivery'||receipt.channel!==config.channel_id||!ts.safeParse(receipt.ts).success){
        actions.push(unknown(action,{reason:'no_confirmed_original_delivery_receipt'}));continue;
      }
      try{
        const message=history.messages.find(m=>m.ts===receipt.ts);
        if(!message||message.type!=='message'||message.user!==config.expected_bot_user_id||
            (message.client_msg_id&&message.client_msg_id!==action.request_id))throw new Error('message_unconfirmed');
        const value={request_id:action.request_id,connection_id:connection,state:'completed',scope:'message_delivery',verified:true,
          work_outcome:'unknown',audience,receipt:{channel:config.channel_id,ts:receipt.ts},
          provenance:{kind:'slack_api_message',message_ts:receipt.ts,actor:message.user,edited_ts:message.edited?.ts??null,
            completion:'message_delivery_only',authority:false}};
        actions.push({...value,revision:revision(value),observed_at:observedAt});
      }catch{actions.push(unknown(action,{reason:'message_not_confirmed_in_bounded_read'}));}
    }
    const result={version:1,connection_id:connection,observed_at:observedAt,availability:'available',
      cursor:{seen:[...seen.entries()].slice(-64).map(([id,revision])=>({id,revision}))},
      coverage:{baseline:!request.cursor,bounded:true,gap:gap||omittedEvents>0,omitted_events:omittedEvents,
        omission_reason:omittedEvents?'event_text_empty_or_exceeded_bound':null,older_messages_omitted:history.has_more===true,
        threads_read:false,work_completion_supported:false,output_truncated:false},events:fresh,actions};
    while(Buffer.byteLength(JSON.stringify(result))>MAX_OUTPUT&&result.events.length){result.events.shift();result.coverage.gap=true;result.coverage.output_truncated=true;}
    if(Buffer.byteLength(JSON.stringify(result))>MAX_OUTPUT)throw new Error('output_limit');
    return result;
  }catch{
    return {version:1,connection_id:connection,observed_at:observedAt,availability:'unavailable',cursor:request?.cursor??null,
      coverage:{gap:true,bounded:true,reason:'observation_unavailable'},events:[],actions:request?.actions.map(a=>unknown(a,{reason:'observation_unavailable'}))??[]};
  }
}

async function main(){
  /** @type {unknown} */ let input;
  try{
    if(process.argv.length!==4||process.argv[2]!=='--config')throw new Error('invalid_arguments');
    const chunks=[];let size=0;for await(const chunk of process.stdin){size+=chunk.length;if(size>MAX_INPUT)throw new Error('input_limit');chunks.push(chunk);}
    input=JSON.parse(Buffer.concat(chunks).toString('utf8'));
  }catch{input=null;}
  process.stdout.write(JSON.stringify(await observeSlack({configPath:process.argv[3]??'',input}))+'\n');
}
// Match Node's canonical module URL even through a versioned install symlink.
let invokedAsEntry=false;
if(process.argv[1]){
  try{invokedAsEntry=realpathSync(fileURLToPath(import.meta.url))===realpathSync(process.argv[1]);}catch{}
}
if(invokedAsEntry) await main();
