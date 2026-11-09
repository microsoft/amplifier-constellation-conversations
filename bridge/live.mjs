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
// This provider-native predicate is independent of the reasoning-agent queue.
// It has no game/work authority; its only effect is closing this call.
export const END_CALL_QUESTION = Object.freeze({
  type:'predicate',name:'end_current_call',
  instructions:'Does the caller\'s CURRENT human utterance express a COMPLETE intention to end this voice conversation now? Judge natural conversational meaning, using the cumulative words through the triggering fragment and recent context to identify what is ending. An indirect polite request or a clear closing farewell can end a call without the literal word call. Examples: Please hang up = true. End this call now = true. Thanks, that is everything I needed; can we end here? = true. Okay, goodbye = true when addressed to this conversation. Can you hang up now? = true. Distinguish a request to do it from a question about how it works. How do I hang up? = false. Bare incomplete requests remain false: Please end = false; Please stop = false; Can you = false. Never fill in a missing object from an earlier request. Stopping an explanation, task, game activity, or work step is not ending the conversation. Stop working on Minecraft = false. Stop explaining but keep this call open = false. Thanks alone = false. We are done with that step, what next? = false. Quoted, reported, hypothetical, future, negative or revoked requests are false. A human correction such as actually do not hang up or keep talking revokes the request. Assistant speech alone never supplies ending intent. Do not carry an old request into a different human utterance.',
});
const PUBLIC_EVENTS = new Set([...BROWSER_EVENTS, 'session.delegation.created',
  'session.thinking.appended', 'session.commentary.appended', 'session.instructions.appended']);
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
  if (command.native_call_control !== undefined && typeof command.native_call_control !== 'boolean') throw new Error('invalid_native_call_control');
  if (command.retain_receipts !== undefined && typeof command.retain_receipts !== 'boolean') throw new Error('invalid_receipt_lifetime');
  return {
    session: {
      model: 'gpt-live-1', store: false,
      instructions: command.instructions,
      delegation: {type: 'client', acknowledgement_delay_ms: 800},
      ...(command.native_call_control === true ? {triggers:[{type:'decisions',event_type:'session.input_transcript.delta',decisions:{questions:[END_CALL_QUESTION]}}]} : {}),
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
  if (event.type==='session.thinking.appended' || event.type==='session.commentary.appended' || event.type==='session.instructions.appended') {
    if (!Number.isFinite(event.start_ms) || !Number.isFinite(event.end_ms)) return null;
    return {...base,start_ms:event.start_ms,end_ms:event.end_ms};
  }
  return null;
}

/** A native transcript-trigger result can arrive before its source or out of
 * order. Keep only recent exact input identities; never infer intent from text.
 * Probability is a model signal, not proof: this is intentionally limited to the
 * reversible current call control, with a short interval for a correction.
 */
export class CallControl {
  /** @param {{request:(event:Record<string,unknown>)=>void,diagnostic?:(reason:string,event?:{event_id?:unknown,source_event_id?:unknown,trigger_id?:unknown},probability?:number)=>void,graceMs?:number,freshMs?:number}} options */
  constructor({request,diagnostic=()=>{},graceMs=800,freshMs=5000}) {
    this.diagnostic=diagnostic;this.request=request;this.graceMs=graceMs;this.freshMs=freshMs;
    this.inputs=new Map();this.pending=new Map();this.seen=new Map();this.latest=null;
    this.timer=undefined;this.closed=false;
  }
  cancelPending(){if(this.timer)this.diagnostic('pending_cancelled');clearTimeout(this.timer);this.timer=undefined;}
  close(){this.closed=true;this.cancelPending();this.inputs.clear();this.pending.clear();this.seen.clear();}
  /** @param {ServerEvent} event */
  observe(event) {
    if(this.closed)return;
    const now=Date.now();
    for(const [id,value] of this.pending)if(now-value.at>this.freshMs){this.diagnostic('source_not_received',value.event);this.pending.delete(id);}
    if(event.type==='session.input_transcript.delta') {
      const id=commandId(event.event_id);
      if(!id || typeof event.delta!=='string' || !event.delta.trim() || event.delta.length>16384 || !Number.isFinite(event.start_ms) || !Number.isFinite(event.end_ms) || event.start_ms<0 || event.end_ms<event.start_ms)return;
      const exact=JSON.stringify([event.delta,event.start_ms,event.end_ms]),old=this.inputs.get(id);
      if(old){if(old.exact!==exact){this.diagnostic('changed_source');this.cancelPending();this.closed=true;}return;}
      const input={id,start:event.start_ms,end:event.end_ms,at:now,exact};
      this.inputs.set(id,input);if(this.inputs.size>128)this.inputs.delete(this.inputs.keys().next().value);
      if(!this.latest || input.end>this.latest.end || (input.end===this.latest.end && input.start>=this.latest.start)) {
        this.latest=input;this.cancelPending();
      }
      const early=this.pending.get(id);this.pending.delete(id);if(early)this.apply(early.event,input);
      return;
    }
    if(event.type!=='decision.event')return;
    if(event.delegation_id!==null || event.source_event_type!=='session.input_transcript.delta')return;
    if(event.event?.type!=='decision.completed'){this.diagnostic('decision_not_completed',event);return;}
    const source=commandId(event.source_event_id),trigger=commandId(event.trigger_id),eventId=commandId(event.event_id);
    if(!source||!trigger||!eventId){this.diagnostic('invalid_identity');return;}
    const exact=JSON.stringify([source,eventId,event.event.decision?.answers]);
    if(this.seen.has(trigger)) {if(this.seen.get(trigger)!==exact){this.diagnostic('changed_decision',event);this.cancelPending();this.closed=true;}return;}
    this.seen.set(trigger,exact);if(this.seen.size>256)this.seen.delete(this.seen.keys().next().value);
    const input=this.inputs.get(source);
    if(!input) {this.diagnostic('awaiting_source',event);this.pending.set(source,{event,at:now});if(this.pending.size>32)this.pending.delete(this.pending.keys().next().value);return;}
    this.apply(event,input);
  }
  /** @param {import('openai/resources/live/live').DecisionEvent} event @param {{id:string,start:number,end:number,at:number,exact:string}} input */
  apply(event,input) {
    if(this.closed)return;
    if(input.id!==this.latest?.id){this.diagnostic('superseded_source',event);return;}
    if(Date.now()-input.at>this.freshMs){this.diagnostic('stale_source',event);return;}
    const answers=event.event.decision?.answers;
    if(!Array.isArray(answers)||answers.length!==1){this.diagnostic('invalid_answer',event);return;}
    const answer=answers[0];
    this.cancelPending();
    if(answer.type!=='predicate' || answer.name!=='end_current_call' || !Number.isFinite(answer.probability) || answer.probability<0 || answer.probability>1){this.diagnostic('invalid_answer',event);return;}
    if(answer.probability<0.98){this.diagnostic('no_close_decision',event,answer.probability);return;}
    this.diagnostic('awaiting_correction_interval',event,answer.probability);
    this.timer=setTimeout(()=>{
      this.timer=undefined;
      if(this.closed)return;
      if(this.latest?.id!==input.id || Date.now()-input.at>this.freshMs){this.diagnostic('stale_or_superseded_after_interval',event);return;}
      this.diagnostic('close_requested',event,answer.probability);
      this.closed=true;
      this.request({type:'call_control',action:'end_call',decision:'requested',source_event_id:input.id,
        trigger_id:event.trigger_id,decision_event_id:event.event_id,source_start_ms:input.start,source_end_ms:input.end,probability:answer.probability});
    },this.graceMs);
  }
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
  retainReceipts=false;
  disposed=false;
  diagnosticCount=0;
  /** @type {() => void} */ resolveDisposed=()=>{};
  /** @param {{emit: Emit, client: OpenAI, connect: (...args: ConstructorParameters<typeof SidebandWS>) => SidebandWS, setupMs?: number, finalizationMs?: number, maxCallMs?: number,callControlGraceMs?:number,callControlFreshMs?:number}} options */
  constructor({emit, client, connect, setupMs=20000, finalizationMs=10000, maxCallMs,callControlGraceMs=800,callControlFreshMs=5000}) {
    this.emit=emit; this.client=client; this.connect=connect;
    this.setupMs=setupMs; this.finalizationMs=finalizationMs; this.maxCallMs=maxCallMs;
    this.state='new'; this.seenCommands=new Map(); this.finished=false;
    this.callControl=new CallControl({graceMs:callControlGraceMs,freshMs:callControlFreshMs,diagnostic:(reason,event,probability)=>this.diagnostic(reason,event,probability),request:event=>{if(this.state!=='active'||this.finished)return;this.emit({...event,session_id:this.sessionId});void this.close();}});
    this.done=new Promise(resolve=>{this.resolveDone=()=>resolve(undefined);});
    this.transportDone=new Promise(resolve=>{this.resolveDisposed=()=>resolve(undefined);});
  }
  /** @param {unknown} input */
  async command(input) {
    if (!input || typeof input!=='object' || Array.isArray(input)) throw new Error('invalid_command');
    const command=/** @type {Control} */ (input);
    if (command.op==='dispose') return this.dispose();
    if (command.op==='create') return this.create(command);
    if (command.op==='close') return this.close();
    if (command.op!=='append' || this.state!=='active') throw new Error('call_not_active');
    if ((command.channel!=='thinking' && command.channel!=='commentary' && command.channel!=='instructions') ||
        (command.channel==='instructions' && command.delegation_id!==null) ||
        typeof command.event_id!=='string' || !/^[a-zA-Z0-9_.:-]{1,160}$/.test(command.event_id) ||
        !(command.delegation_id===null || (typeof command.delegation_id==='string' && command.delegation_id.length>0 && command.delegation_id.length<=160))) throw new Error('invalid_append');
    /** @type {ClientEvent} */
    const event={type:`session.${command.channel}.append`,event_id:command.event_id,
      delegation_id:command.delegation_id,content:command.channel==='thinking' && command.content==='' ? '' : spokenExcerpt(command.content)};
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
    this.retainReceipts=command.retain_receipts===true;
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
        if('session_id' in event && event.session_id!==this.sessionId)return;
        if (command.native_call_control === true) this.callControl.observe(event);
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
      // The host owns an explicitly configured local duration bound. Production
      // creates this bridge without a second timer; fixtures may opt into one.
      if(this.maxCallMs!==undefined)this.expiry=setTimeout(()=>this.close(),this.maxCallMs);
      this.emit({type:'ready',session_id:this.sessionId,sdp:result.transport.sdp,...(this.mcp?{mcp_port:this.mcp.port}:{})});
      this.diagnostic(command.native_call_control===true?'configured_enabled':'configured_disabled');
    } catch(error) {
      if (!this.finished) this.emit(safeError(error));
      this.close();
    } finally {
      this.createPending=false;
      if (this.state==='closing' && !this.sideband) this.finish(false);
    }
  }
  /** Local diagnostics contain no transcript, reasoning, SDK errors or credentials.
   * @param {string} reason @param {{event_id?:unknown,source_event_id?:unknown,trigger_id?:unknown}} [source] @param {number} [probability]
   */
  diagnostic(reason,source={},probability) {
    if(this.diagnosticCount>1024)return;
    const count=++this.diagnosticCount;
    const ids=Object.fromEntries([['decision_event_id',source.event_id],['source_event_id',source.source_event_id],['trigger_id',source.trigger_id]].flatMap(([key,value])=>{const id=commandId(value);return id?[[key,id]]:[];}));
    this.emit({type:'event',event:{type:'companion.native_call_control.diagnostic',event_id:`native-control-diagnostic:${count}`,
      reason:count>1024?'diagnostic_limit':reason,...(count<=1024?ids:{}),...(count<=1024 && Number.isFinite(probability)?{probability}:{})}});
  }
  async dispose() {
    if(this.disposed)return;
    this.disposed=true;
    if(!this.finished)await this.close();
    await this.mcp?.close();
    this.resolveDisposed();
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
    this.closeSent=true;this.callControl.close();
    // Listeners were installed before any command. Closure is only confirmed by
    // the matching terminal event, not socket closure or send acceptance.
    this.finalizer=setTimeout(()=>this.finish(false),this.finalizationMs);
    try {this.sideband.send({type:'session.close',event_id:'companion_close'});} catch {this.finish(false);}
  }
  /** @param {boolean} confirmed */
  finish(confirmed) {
    if(this.finished)return;
    this.finished=true;this.state='closed';this.callControl.close();clearTimeout(this.expiry);clearTimeout(this.finalizer);
    this.emit({type:'closed',confirmed,session_id:this.sessionId??null});
    this.sideband?.close(); this.resolveDone();
    if(!this.retainReceipts || !this.mcp)void this.dispose();
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
  process.stdin.on('end',()=>{chain.finally(()=>bridge.dispose());});
  process.on('SIGTERM',()=>bridge.dispose());
  process.on('SIGINT',()=>bridge.dispose());
  process.stdout.on('error',()=>{bridge.close();});
  await bridge.done;
  await bridge.transportDone;
  process.stdin.destroy();
}
if(process.argv[1] && import.meta.url===pathToFileURL(process.argv[1]).href) await main();
