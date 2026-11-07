/** Trusted, per-call GPT-Live bridge. ACP and application authority stay in the host.
 * No vendor framing here: the official OpenAI SDK owns HTTP and sideband transport.
 */
import OpenAI from 'openai';
import {SidebandWS} from 'openai/resources/live/sideband/ws';
import {pathToFileURL} from 'node:url';
import {startCompanionMcp} from './companion-mcp.mjs';

/** @typedef {import('openai/resources/live/live').LiveCreateParams} LiveCreateParams */
/** @typedef {import('openai/resources/live/sideband/sideband').ConnectServerEvent} ServerEvent */
/** @typedef {import('openai/resources/live/sideband/sideband').ConnectClientEvent} ClientEvent */
/** @typedef {Record<string, unknown>} Control */
/** @typedef {(value: Record<string, unknown>) => void} Emit */

export const BROWSER_EVENTS = Object.freeze(/** @satisfies {Array<ServerEvent['type']>} */ ([
  'session.started', 'session.input_transcript.delta', 'session.output_transcript.delta',
  'session.usage.updated', 'session.closed', 'error',
]));
const PUBLIC_EVENTS = new Set([...BROWSER_EVENTS, 'session.delegation.created',
  'session.thinking.appended', 'session.commentary.appended']);
// Parameter names are protocol identifiers, never provider-supplied values.
const ERROR_PARAMS = new Set(['type','event_id','delegation_id','content','session',
  'session.model','session.instructions','session.input','session.voice','session.audio',
  'session.audio.output.voice','session.delegation','session.delegation.type']);
/** @param {unknown} value */
const machineName=value=>typeof value==='string' && /^[a-z][a-z0-9_]{0,63}$/.test(value) ? value : undefined;
/** @param {unknown} value */
const commandId=value=>typeof value==='string' && /^[a-zA-Z0-9_.:-]{1,160}$/.test(value) ? value : undefined;
/** @type {Set<string>} */
const VOICES = new Set(/** @satisfies {Array<import('openai/resources/live/live').BuiltInVoice>} */ (['alloy','ash','ballad','beacon','bossa','cedar','cinder','coral','delta','echo','gleam','marin','meridian','quartz','ripple','sage','shida','shimmer','stone','tempo','verse','vesper','willow']));

/** @param {Control} command @returns {LiveCreateParams} */
export function sessionOptions(command) {
  if (typeof command.sdp !== 'string' || command.sdp.length > 131072 || !command.sdp.startsWith('v=0')) throw new Error('invalid_offer');
  if (typeof command.voice !== 'string' || !VOICES.has(command.voice)) throw new Error('invalid_voice');
  if (typeof command.instructions !== 'string' || command.instructions.length > 24000) throw new Error('invalid_instructions');
  if (command.history !== undefined && (typeof command.history !== 'string' || Buffer.byteLength(command.history) > 12000)) throw new Error('invalid_history');
  return {
    session: {
      model: 'gpt-live-1', store: false,
      instructions: command.instructions,
      delegation: {type: 'client', acknowledgement_delay_ms: 600},
      audio: {output: {voice: command.voice}},
      client: {data_channel: {allowed_client_events: [], allowed_server_events: BROWSER_EVENTS.map(type=>({type}))}},
      // Retained conversation is context, not privileged application instructions.
      ...(typeof command.history === 'string' && command.history ? {input: [{type:'message',role:'user',content:[{type:'input_text',text:command.history}]}]} : {}),
    },
    transport: {type:'webrtc',sdp:command.sdp},
  };
}

// The host supplies complete semantic fragments. Clipping here can change a
// fact (for example, deleting "on ARM64"), so enforce the budget without edits.
/** @param {unknown} text */
export function spokenExcerpt(text, limit=480) {
  if (typeof text !== 'string' || !text.trim() || Buffer.byteLength(text) > limit) throw new Error('invalid_content');
  return text;
}

/** @param {unknown} error */
export function safeError(error) {
  const status = error && typeof error === 'object' && 'status' in error && typeof error.status === 'number' && Number.isInteger(error.status) ? error.status : undefined;
  const code = status===401 ? 'credentials_rejected' : status===403 ? 'model_access_denied' : status===429 ? 'provider_capacity' : 'voice_transport_failed';
  return {type:'error',code,message: {
    credentials_rejected:'The voice service rejected its saved credentials.',
    model_access_denied:'This project cannot use the selected voice service.',
    provider_capacity:'The voice service is at its current capacity. No agent request was retried.',
    voice_transport_failed:'The voice connection could not continue. Check the call details before trying again.',
  }[code],...(status ? {status} : {})};
}

/** Positive projection deliberately excludes provider configuration and extras.
 * @param {ServerEvent} event
 * @param {{has:(id:string)=>boolean}} [knownCommands]
 * @returns {Record<string, unknown> | null}
 */
export function publicEvent(event,knownCommands=new Set()) {
  if (!PUBLIC_EVENTS.has(event?.type)) return null;
  const id='event_id' in event && typeof event.event_id==='string' ? event.event_id : undefined;
  const base={type:event.type,...(id ? {event_id:id}:{}),...('client_event_id' in event && typeof event.client_event_id==='string' ? {client_event_id:event.client_event_id}:{})};
  if (event.type==='error') {
    const code=machineName(event.error?.code),type=machineName(event.error?.type);
    const param=typeof event.error?.param==='string' && ERROR_PARAMS.has(event.error.param) ? event.error.param : undefined;
    const nestedId=commandId(event.error?.client_event_id),outerId=commandId(event.client_event_id);
    const conflict=nestedId && outerId && nestedId!==outerId;
    const client=!conflict && nestedId && knownCommands.has(nestedId) ? nestedId : undefined;
    const outer=!conflict && outerId && knownCommands.has(outerId) ? outerId : undefined;
    // Keep the official nested correlation field; never copy raw message,
    // configuration, headers, or SDK cause. A conflict remains distinguishable.
    return {type:event.type,...(id?{event_id:id}:{}),...(outer?{client_event_id:outer}:{}),
      error:{code:code||'voice_service_error',message:'The voice service reported an error.',
        ...(type?{type}:{}),...(param?{param}:{}),...(client?{client_event_id:client}:{})}};
  }
  if (event.type==='session.started' || event.type==='session.closed') {
    if (typeof event.session?.id!=='string') return null;
    if (event.type==='session.closed' && (!['close_requested','expired','content','remote_hangup','connection_lost'].includes(event.reason) || typeof event.usage?.seconds!=='number' || !Number.isFinite(event.usage.seconds))) return null;
    return {...base,session:{id:event.session.id},
      ...(event.type==='session.closed' ? {reason:event.reason,usage:{seconds:event.usage?.seconds}}:{})};
  }
  if (event.type==='session.usage.updated') {
    if (typeof event.usage?.seconds!=='number' || !Number.isFinite(event.usage.seconds)) return null;
    return {...base,usage:{seconds:event.usage.seconds},
      ...(typeof event.context_window?.usage_ratio==='number' && Number.isFinite(event.context_window.usage_ratio) ? {context_window:{usage_ratio:event.context_window.usage_ratio}}:{})};
  }
  if (event.type==='session.delegation.created') {
    if (typeof event.delegation?.id!=='string' || event.delegation.target!=='client' || !Number.isFinite(event.offset_ms)) return null;
    return {...base,delegation:{id:event.delegation.id,target:event.delegation.target},offset_ms:event.offset_ms};
  }
  if (event.type==='session.input_transcript.delta' || event.type==='session.output_transcript.delta') {
    if (typeof event.delta!=='string' || !Number.isFinite(event.start_ms) || !Number.isFinite(event.end_ms)) return null;
    return {...base,delta:event.delta,start_ms:event.start_ms,end_ms:event.end_ms};
  }
  if (event.type==='session.thinking.appended' || event.type==='session.commentary.appended') {
    if (!Number.isFinite(event.start_ms) || !Number.isFinite(event.end_ms)) return null;
    return {...base,start_ms:event.start_ms,end_ms:event.end_ms};
  }
  return null;
}

export class LiveBridge {
  /** @type {Awaited<ReturnType<typeof startCompanionMcp>> | undefined} */ mcp;
  /** @type {SidebandWS | undefined} */ sideband;
  /** @type {string | undefined} */ sessionId;
  /** @type {ReturnType<typeof setTimeout> | undefined} */ expiry;
  /** @type {ReturnType<typeof setTimeout> | undefined} */ finalizer;
  /** @type {() => void} */ resolveDone=()=>{};
  createPending=false;
  closeSent=false;
  /** @param {{emit: Emit, client: OpenAI, connect: (...args: ConstructorParameters<typeof SidebandWS>) => SidebandWS, setupMs?: number, finalizationMs?: number, maxCallMs?: number}} options */
  constructor({emit, client, connect, setupMs=20000, finalizationMs=10000, maxCallMs=1800000}) {
    this.emit=emit; this.client=client; this.connect=connect;
    this.setupMs=setupMs; this.finalizationMs=finalizationMs; this.maxCallMs=maxCallMs;
    this.state='new'; this.seenCommands=new Map(); this.finished=false;
    this.done=new Promise(resolve=>{this.resolveDone=()=>resolve(undefined);});
  }
  /** @param {unknown} input */
  async command(input) {
    if (!input || typeof input!=='object' || Array.isArray(input)) throw new Error('invalid_command');
    const command=/** @type {Control} */ (input);
    if (command.op==='create') return this.create(command);
    if (command.op==='close') return this.close();
    if (command.op!=='append' || this.state!=='active') throw new Error('call_not_active');
    if ((command.channel!=='thinking' && command.channel!=='commentary') ||
        typeof command.event_id!=='string' || !/^[a-zA-Z0-9_.:-]{1,160}$/.test(command.event_id) ||
        !(command.delegation_id===null || (typeof command.delegation_id==='string' && command.delegation_id.length>0 && command.delegation_id.length<=160))) throw new Error('invalid_append');
    /** @type {ClientEvent} */
    const event={type:`session.${command.channel}.append`,event_id:command.event_id,
      delegation_id:command.delegation_id,content:spokenExcerpt(command.content)};
    const exact=JSON.stringify(event), previous=this.seenCommands.get(command.event_id);
    if (previous && previous!==exact) throw new Error('changed_command');
    if (previous) return; // Local duplicate is never resent across a transport boundary.
    if (this.seenCommands.size>=2048) throw new Error('command_limit');
    this.seenCommands.set(command.event_id,exact);
    this.sideband?.send(event);
  }
  /** @param {Control} command */
  async create(command) {
    if (this.state!=='new') throw new Error('already_created');
    const options=sessionOptions(command);
    const setupDeadline=Date.now()+this.setupMs;
    this.state='creating';this.createPending=true;
    try {
      if (command.mcp !== undefined) {
        if (!command.mcp || typeof command.mcp!=='object' || Array.isArray(command.mcp)) throw new Error('invalid_mcp');
        const config=/** @type {Record<string, unknown>} */(command.mcp);
        if (typeof config.api_base!=='string' || typeof config.call_id!=='string' || typeof config.token!=='string') throw new Error('invalid_mcp');
        this.mcp=await startCompanionMcp({apiBase:config.api_base,callId:config.call_id,token:config.token,port:0});
      }
      const result=await this.client.live.create(options,{timeout:this.setupMs,maxRetries:0});
      this.sessionId=result.session?.id;
      if (typeof this.sessionId!=='string' || result.transport?.type!=='webrtc' || typeof result.transport.sdp!=='string') throw new Error('invalid_answer');
      this.sideband=this.connect(this.client,{session_id:this.sessionId},{maxPayload:1048576,maxQueueSize:262144,reconnect:null});
      this.sideband.on('event',event=>{
        if (this.finished || !event || typeof event!=='object') return;
        if ((event.type==='session.started' || event.type==='session.closed') && event.session?.id!==this.sessionId) return;
        // Never mirror private reasoning, raw audio, or session configuration.
        const safe=publicEvent(event,this.seenCommands);
        if (safe) this.emit({type:'event',event:safe});
        if (safe && event.type==='session.closed' && event.session?.id===this.sessionId) this.finish(true);
      });
      this.sideband.on('error',error=>{
        if(this.finished)return;
        // The official SDK emits a server error both as an event and through
        // this callback. A rejected append or interrupted speech is not a
        // failed transport: retain the projected receipt and keep listening
        // for an actual session.closed. Never resend the rejected command.
        if(error.error?.type==='error')return;
        this.emit(safeError(error));void this.close();
      });
      this.sideband.on('close',()=>{if(!this.finished)this.finish(false);});
      const socket=this.sideband.socket;
      await new Promise((resolve,reject)=>{
        if(socket.readyState===1) return resolve(undefined);
        if(socket.readyState>=2) return reject(new Error('sideband_closed'));
        // Creation and sideband attachment share one setup budget; teardown has
        // its own finalization allowance rather than doubling connection time.
        const timer=setTimeout(()=>{cleanup();reject(new Error('sideband_timeout'));},Math.max(1,setupDeadline-Date.now()));
        const opened=()=>{cleanup();resolve(undefined);}, failed=()=>{cleanup();reject(new Error('sideband_failed'));};
        const cleanup=()=>{clearTimeout(timer);socket.off('open',opened);socket.off('error',failed);socket.off('close',failed);};
        socket.on('open',opened);socket.on('error',failed);socket.on('close',failed);
      });
      if(this.finished) return;
      if(this.state==='closing') {this.startClosing();return;}
      this.state='active';
      this.expiry=setTimeout(()=>this.close(),this.maxCallMs);
      this.emit({type:'ready',session_id:this.sessionId,sdp:result.transport.sdp,...(this.mcp?{mcp_port:this.mcp.port}:{})});
    } catch(error) {
      if (!this.finished) this.emit(safeError(error));
      this.close();
    } finally {
      this.createPending=false;
      if (this.state==='closing' && !this.sideband) this.finish(false);
    }
  }
  close() {
    if(this.finished) return this.done;
    this.state='closing'; clearTimeout(this.expiry);
    // A bounded in-flight create may still allocate a session. Wait for its ID,
    // attach using the official SDK, and close it before emitting any ready event.
    if(!this.sideband) {if(!this.createPending)this.finish(false);return this.done;}
    this.startClosing();
    return this.done;
  }
  startClosing() {
    if(this.finished || this.closeSent || !this.sideband)return;
    this.closeSent=true;
    // Listeners were installed before any command. Closure is only confirmed by
    // the matching terminal event, not socket closure or send acceptance.
    this.finalizer=setTimeout(()=>this.finish(false),this.finalizationMs);
    try {this.sideband.send({type:'session.close',event_id:'companion_close'});} catch {this.finish(false);}
  }
  /** @param {boolean} confirmed */
  finish(confirmed) {
    if(this.finished)return;
    this.finished=true;this.state='closed';clearTimeout(this.expiry);clearTimeout(this.finalizer);
    this.emit({type:'closed',confirmed,session_id:this.sessionId??null});
    this.mcp?.close();
    this.sideband?.close(); this.resolveDone();
  }
}

async function main() {
  let outputBytes=0;
  /** @type {Emit} */
  const emit=value=>{
    const line=JSON.stringify(value)+'\n'; outputBytes+=Buffer.byteLength(line);
    if (outputBytes>32*1024*1024 || process.stdout.writableLength>2*1024*1024) {process.exitCode=1;bridge?.close();return;}
    process.stdout.write(line);
  };
  /** @type {LiveBridge} */
  let bridge;
  try {
    bridge=new LiveBridge({emit,client:new OpenAI({maxRetries:0,timeout:20000}),connect:(...args)=>new SidebandWS(...args)});
  } catch {emit({type:'error',code:'credentials_missing',message:'The installation has no voice API credentials.'});return;}
  let buffered=''; let chain=Promise.resolve();
  process.stdin.setEncoding('utf8');
  process.stdin.on('data',chunk=>{
    buffered+=chunk;
    if(Buffer.byteLength(buffered)>256*1024){emit({type:'error',code:'input_limit',message:'Call control input exceeded its limit.'});process.stdin.destroy();bridge.close();return;}
    let nl;
    while((nl=buffered.indexOf('\n'))>=0){
      const line=buffered.slice(0,nl);buffered=buffered.slice(nl+1);
      if(!line.trim())continue;
      chain=chain.then(()=>bridge.command(JSON.parse(line))).catch(()=>{emit({type:'error',code:'invalid_control',message:'Call control could not be applied.'});bridge.close();});
    }
  });
  process.stdin.on('end',()=>{chain.finally(()=>bridge.close());});
  process.on('SIGTERM',()=>bridge.close());
  process.on('SIGINT',()=>bridge.close());
  process.stdout.on('error',()=>{bridge.close();});
  await bridge.done;
  process.stdin.destroy();
}
if(process.argv[1] && import.meta.url===pathToFileURL(process.argv[1]).href) await main();
