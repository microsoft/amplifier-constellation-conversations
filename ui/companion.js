import {ConversationFloor} from './companion-floor.mjs';
import {safeCardLink, renderCardText, savedCardTime} from './companion-cards.mjs';
import {createHuddles} from './huddles.mjs';
import {createHuddleApi} from './huddles-api.mjs';
// The browser owns media and presentation only. Provider commands, delegation,
// authorization and persisted call events belong to the Companion service.
const $ = id => document.getElementById(id);
const element = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const initials = label => (label || 'Agent').trim().split(/\s+/).map(p => p[0]).slice(0, 2).join('').toUpperCase();
const state = {session:null, contacts:[], available:false, call:null, generation:0, authGeneration:0, recovery:null, voiceContact:null, finalizingCalls:new Set()};
const audio = $('remote-audio');

async function api(path, method = 'GET', body, options = {}) {
  const headers = body === undefined ? {} : {'Content-Type':'application/json'};
  if (method !== 'GET' && state.session?.csrf) headers['X-CSRF-Token'] = state.session.csrf;
  const {signal, ...fetchOptions} = options;
  const abort = new AbortController(), timer = setTimeout(() => abort.abort(), path === '/api/v1/companion/calls' ? 40000 : 10000);
  const cancel = () => abort.abort();
  if (signal?.aborted) abort.abort(); else signal?.addEventListener('abort', cancel, {once:true});
  try {
    const response = await fetch(path, {method, headers, credentials:'same-origin', cache:'no-store', signal:abort.signal,
      body:body === undefined ? undefined : JSON.stringify(body), ...fetchOptions});
    let value;
    try { value = await response.json(); }
    catch { const error = new Error('The service returned an unreadable response.'); error.uncertain = method !== 'GET'; throw error; }
    if (!response.ok) {
      const error = new Error(typeof value.error === 'string' ? value.error : 'The request could not be completed.');
      error.status = response.status;
      error.uncertain = response.status >= 500;
      throw error;
    }
    return value;
  } catch (error) {
    // A transport failure can happen after a mutation was applied. Only an
    // explicit rejection establishes that the operation failed definitively.
    if (method !== 'GET' && error.uncertain === undefined) error.uncertain = true;
    throw error;
  } finally { clearTimeout(timer); signal?.removeEventListener('abort', cancel); }
}
function notice(message) { $('notice').hidden = !message; $('notice-text').textContent = message || ''; }
function current(call) { return Boolean(call && state.call === call && call.generation === state.generation && !call.ended); }
function conversationLink(id) { return '/?conversation=' + encodeURIComponent(id); }

function startHuddleView(call) {
  const context = Object.freeze({key:`${call.authGeneration}:${call.id}`,owner:state.session.owner,parentId:call.conversationId});
  const callbacks = createHuddleApi({api,getContext:()=>current(call) ? context : null});
  call.huddles = createHuddles({root:$('call-huddle-view'),...callbacks,list:async options=>{
    try {
      const result = await callbacks.list(options);
      if (current(call)) {
        $('call-huddle-count').textContent = result.items.length ? `(${result.items.length})` : '';
        $('call-huddles').hidden = !result.items.length;
      }
      return result;
    } catch (error) {
      if (current(call)) { $('call-huddles').hidden=false; $('call-huddle-count').textContent=''; }
      throw error;
    }
  }});
  call.huddleRefreshAt=Date.now();
  call.huddleRefresh=call.huddles.setContext(context).finally(()=>{call.huddleRefresh=null;});
}
function refreshHuddles(call) {
  // Reuse the existing call poll. This only refreshes authorized presentation;
  // it never creates input, a model turn, or voice context.
  if (!current(call) || !call.huddles || call.huddleRefresh || Date.now()-call.huddleRefreshAt<5000) return;
  call.huddleRefreshAt=Date.now();
  call.huddleRefresh=call.huddles.refresh().finally(()=>{call.huddleRefresh=null;});
}
$('call-huddles').ontoggle=()=>{
  const call=state.call;if(!current(call)||!call.huddles)return;
  if(!$('call-huddles').open)call.huddles.close();
  else {call.huddleRefreshAt=0;refreshHuddles(call);}
};

async function openContacts() {
  const authGeneration = state.authGeneration, generation = state.generation, recovery = state.recovery;
  const config = await api('/api/v1/companion');
  if (authGeneration !== state.authGeneration) return;
  state.contacts = Array.isArray(config.contacts) ? config.contacts : [];
  state.available = config.available === true;
  const prior = config.active_call;
  // Catalog absence is not proof of closure. Preserve an unresolved creation
  // request, and never restore recovery from a read begun before it was cleared.
  if (!state.call && generation === state.generation && state.recovery === recovery && !recovery
      && prior && typeof prior.id === 'string' && !['closed','interrupted'].includes(prior.state)) {
    state.recovery = {callId:prior.id,conversationId:prior.conversation_id,request:null,discovered:true};
  }
  $('loading').hidden = true; $('login').hidden = true; $('app').hidden = false; $('sign-out').hidden = false;
  $('availability').hidden = state.available;
  $('availability').textContent = config.reason || 'Voice calling has not been enabled on this installation yet.';
  $('empty-contacts').hidden = state.contacts.length !== 0;
  $('contacts').replaceChildren(...state.contacts.map(contact => {
    const card = element('article', 'contact-card'); card.dataset.agentId = contact.id;
    const avatar = element('div', 'contact-avatar', initials(contact.label)); avatar.setAttribute('aria-hidden', 'true');
    card.append(avatar, element('h2', null, contact.label || 'Agent'), element('p', null, contact.description || 'Your agent, with room to talk.'));
    const voice = element('button', 'contact-voice text-button', `Voice · ${contact.voice || 'Settings'}`); voice.type = 'button'; voice.setAttribute('aria-label', `Voice settings for ${contact.label || 'agent'}`); voice.onclick = () => openVoiceSettings(contact); card.append(voice);
    const button = element('button', 'primary', 'Call'); button.type = 'button'; button.setAttribute('aria-label', `Call ${contact.label || 'agent'}`);
    button.disabled = !state.available || Boolean(state.recovery) || Boolean(state.call);
    button.append(element('span', null, '↗')); button.lastChild.setAttribute('aria-hidden', 'true');
    button.onclick = () => startCall(contact);
    card.append(button); return card;
  }));
  showRecovery();
  checkRecovery();
}
function showLogin() {
  if (state.call) endCall('Your sign-in expired. Your microphone and camera are off.',true,true,'auth_expired');
  for (const call of state.finalizingCalls) releaseTransport(call);
  // A later login must not inherit private presentation or recovery state. Old
  // asynchronous callbacks may close their media, but cannot repopulate it.
  state.authGeneration++; state.recovery = null; state.contacts = [];
  $('voice-settings').close(); state.voiceContact = null; $('voice-choice').replaceChildren(); $('voice-settings-status').textContent = ''; $('voice-model-facts').replaceChildren(); $('voice-advanced-facts').replaceChildren(); $('cards').replaceChildren(); $('board-items').replaceChildren(); $('board-history').open = false; $('paste-input').value = ''; $('paste-composer').hidden = true; $('share-receipt').hidden = true; $('transcript').replaceChildren(); $('contacts').replaceChildren();
  $('call-permissions').replaceChildren(); $('permission-alert').hidden = true; $('context-items').replaceChildren(); $('context-empty').hidden = false;
  $('caption-speaker').textContent = ''; $('caption-text').textContent = ''; notice('');
  $('ended-detail').textContent = '';
  $('call-ended').hidden = true; $('call-screen').hidden = true; $('phonebook').hidden = false; showRecovery();
  for (const id of ['continue-work','ended-work-link','recovery-work-link']) { $(id).hidden = true; $(id).href = '/'; }
  $('loading').hidden = true; $('app').hidden = true; $('login').hidden = false; $('sign-out').hidden = true;
  $('username').value = state.session?.owner || ''; $('password').value = '';
}
async function start() {
  try {
    state.session = await api('/auth/session');
    if (!state.session.authenticated) { showLogin(); return; }
    await openContacts();
  } catch (error) { showLogin(); $('login-error').textContent = error.message; $('login-error').hidden = false; }
}

function refreshStatus(call) {
  if (!current(call)) return;
  call.connected = call.peer?.connectionState === 'connected' && call.providerConnected === true;
  let status = 'connecting', title = 'Connecting…', detail = 'Opening a voice conversation.';
  if (call.closing && !call.failed && call.peer?.connectionState !== 'failed') {
    status = 'closing'; title = 'Ending call…';
    detail = 'Waiting for the voice service to confirm. Accepted work can continue in the conversation.';
    if (call.pollError || call.heartbeatError || call.reconcilingUntil) {
      title = 'Checking call status…'; detail = 'Ending was requested, but the final call status is not confirmed.';
    }
  } else if (call.pollError || call.heartbeatError || call.reconcilingUntil || call.peer?.connectionState === 'disconnected') {
    status = 'reconnecting'; title = 'Connection interrupted'; detail = 'Checking the existing call. No new call or work is being started.';
  } else if (call.peer?.connectionState === 'failed' || call.failed) {
    status = 'error'; title = 'Audio connection lost'; detail = 'End this call and call again. Work already accepted stays in its conversation.';
  } else if (call.connected) {
    status = call.working.size ? 'working' : 'live'; title = call.working.size ? 'Working together' : 'Connected';
    detail = call.working.size ? 'You can keep talking while your agent works.' : call.muted ? 'Your microphone is muted.' : 'Speak naturally. You can interrupt or take a pause.';
  }
  $('presence').dataset.status = status; $('call-status').dataset.status = status;
  $('call-status-text').textContent = title; $('call-detail').textContent = detail;
  $('mic-state').textContent = !call.stream ? 'Microphone starting' : call.muted ? 'Microphone muted' : 'Microphone on';
  $('mute').disabled = !call.stream;
  $('camera-toggle').disabled = !call.id || !call.connected;
  $('choose-files').disabled = !call.id || !call.connected;
  $('show-paste').disabled = $('paste-input').disabled = $('share-text').disabled = !call.id || !call.connected;
  $('mute').setAttribute('aria-pressed', String(Boolean(call.muted)));
  $('mute-label').textContent = call.muted ? 'Unmute mic' : 'Mute mic';
}

function waitForIce(peer, call) {
  if (peer.iceGatheringState === 'complete') return Promise.resolve();
  // SDP offers already carry candidates as gathering proceeds. Wait briefly for
  // completion, without inventing a second signaling or trickle-ICE protocol.
  return new Promise(resolve => {
    const finish = () => { clearTimeout(timer); peer.removeEventListener('icegatheringstatechange', changed); resolve(); };
    const changed = () => { if (peer.iceGatheringState === 'complete' || !current(call)) finish(); };
    const timer = setTimeout(finish, 1800); peer.addEventListener('icegatheringstatechange', changed);
  });
}
function wakeLockStatus(call) {
  if (!current(call)) return;
  const supported = Boolean(navigator.wakeLock?.request);
  $('screen-awake').disabled = !supported;
  $('screen-awake').checked = supported && call.keepScreenAwake;
  const message = !supported ? 'This browser cannot keep the screen awake. Keep this page open during the call.'
    : !call.keepScreenAwake ? 'Your usual screen timeout applies. Locking the phone may interrupt this call.'
    : call.wakeLock && !call.wakeLock.released ? 'Keeping the screen awake while this call is visible. Switching apps may interrupt audio.'
    : call.wakeRequest ? 'Asking to keep the screen awake.'
    : 'The screen may dim. Keep this page open; the browser can release the screen lock.';
  const warning = !supported || (call.keepScreenAwake && !call.wakeLock && !call.wakeRequest && !document.hidden);
  $('screen-awake-status').classList.toggle('sr-only', !warning);
  $('screen-awake-status').classList.toggle('screen-awake-warning', warning);
  $('screen-awake-status').textContent = warning ? (supported ? 'Screen may dim. Keep this page open.' : 'Screen-on unavailable in this browser.') : message;
  $('screen-awake').parentElement.title = message;
}
function releaseWakeLock(call) {
  // Invalidate any pending acquisition too. A late grant must not outlive the
  // caller's choice, page visibility, or this call.
  call.wakeRequest = null;
  const lock = call.wakeLock; call.wakeLock = null;
  lock?.release().catch(() => {});
  wakeLockStatus(call);
}
async function requestWakeLock(call) {
  if (!current(call) || !call.keepScreenAwake || document.hidden || !navigator.wakeLock?.request || call.wakeLock || call.wakeRequest) { wakeLockStatus(call); return; }
  const request = {}; call.wakeRequest = request; wakeLockStatus(call);
  try {
    const lock = await navigator.wakeLock.request('screen');
    if (!current(call) || !call.keepScreenAwake || document.hidden || call.wakeRequest !== request) { await lock.release(); return; }
    call.wakeLock = lock;
    lock.addEventListener('release', () => { if (call.wakeLock === lock) { call.wakeLock = null; wakeLockStatus(call); } });
  } catch { /* A denied wake lock never prevents a call. */ }
  finally { if (call.wakeRequest === request) call.wakeRequest = null; wakeLockStatus(call); }
}
function updateDuration(call) {
  if (!current(call)) return;
  const seconds = Math.floor((Date.now() - call.startedAt) / 1000);
  $('call-duration').textContent = `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}
async function startCall(contact) {
  if (state.call || state.recovery || !state.available) return;
  const call = {generation:++state.generation, authGeneration:state.authGeneration, contact, id:null, request:null, ended:false, connected:false,
    browserId:crypto.randomUUID(), keepScreenAwake:true, surfaceSeen:new Set(), userSpeaking:null, agentSpeaking:null, cursor:0, working:new Map(), progress:new Map(), seen:new Set(), shares:new Map(), cards:new Map(), contextItems:new Map(), board:[], muted:false, startedAt:Date.now(), timers:new Set(), transcriptRow:null};
  state.call = call;
  audio.muted = false;
  call.floor = new ConversationFloor({changed:({playback,interruption})=>{
    if (!current(call)) return;
    call.playback = playback; call.interruption = interruption;
    call.userSpeaking = playback.floor === 'unknown' ? null : playback.floor === 'user';
    // Muting consumes the live track; pause() would leave stale playback to
    // resume. Provider controls do not acknowledge what reached the speaker.
    audio.muted = playback.state === 'suppressed';
    call.agentSpeaking = playback.output_active === null ? null : playback.output_active && !audio.muted && !audio.paused;
    surfacePresence(call);
  }});
  document.body.classList.add('call-active');
  notice(''); $('phonebook').hidden = true; $('call-ended').hidden = true; $('call-screen').hidden = false;
  $('call-avatar').textContent = initials(contact.label); $('call-name').textContent = contact.label || 'Your agent';
  $('call-description').textContent = contact.description || 'The same agent. Another place to talk.';
  $('call-duration').textContent = '00:00'; $('cards').replaceChildren(); $('board-items').replaceChildren(); $('board-history').open = false; $('paste-input').value = ''; $('paste-composer').hidden = true; $('share-receipt').hidden = true; $('transcript').replaceChildren();
  $('call-permissions').replaceChildren(); $('permission-alert').hidden = true; $('context-items').replaceChildren(); $('context-empty').hidden = false;
  $('transcript-empty').hidden = false; $('space-empty').hidden = false; $('work-status').hidden = true;
  $('connection-status').hidden = true; $('connection-status').textContent = '';
  $('call-huddles').hidden = true; $('call-huddles').open = false; $('call-huddle-view').replaceChildren();
  updateBoard(call); resetFollowing();
  $('card-count').hidden = true; $('card-count').textContent = '0'; $('continue-work').hidden = true;
  $('caption-speaker').textContent = ''; $('caption-text').textContent = 'Take your time. There’s room for a pause.';
  $('speaker-state').textContent = 'Speaker waiting'; $('enable-audio').hidden = true;
  $('hangup').disabled = false; setTab('space'); refreshStatus(call); updateShareCapability(call); wakeLockStatus(call);
  refreshCapabilities(call);
  try {
    if (!navigator.mediaDevices?.getUserMedia || !window.RTCPeerConnection) throw new Error('Voice calling needs a browser with WebRTC and microphone access on HTTPS.');
    // This function is entered only from an explicit Call click. Never request
    // media permissions while restoring login, rendering contacts, or polling.
    const stream = await navigator.mediaDevices.getUserMedia({audio:{echoCancellation:true,noiseSuppression:true,autoGainControl:true},video:false});
    if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
    call.stream = stream;
    const peer = new RTCPeerConnection(); call.peer = peer;
    for (const track of stream.getTracks()) { peer.addTrack(track, stream); track.addEventListener('ended', () => { if (current(call)) endCall('The microphone became unavailable. Your camera is off, and accepted work can continue.',true,true,'microphone_ended'); }); }
    const channel = peer.createDataChannel('oai-events'); call.channel = channel;
    // Receive-only by design. The server sideband is the sole owner of provider
    // controls and delegation. Durable API events are also the sole transcript
    // source, so data-channel copies cannot duplicate captions or task results.
    channel.addEventListener('message', () => {});
    for (const event of ['open','closing','close','error']) channel.addEventListener(event, () => heartbeat(call,'channel_state'));
    peer.addEventListener('track', event => attachRemoteAudio(call, event.streams?.[0] || new MediaStream([event.track])));
    peer.addEventListener('connectionstatechange', () => {
      if (!current(call)) return;
      if (peer.connectionState === 'connected') { call.startedAt = call.audioStartedAt || Date.now(); call.audioStartedAt = call.startedAt; call.peerLostAt = null; call.failed = false; }
      if (['failed','disconnected'].includes(peer.connectionState)) { call.peerLostAt ||= Date.now(); call.failed = peer.connectionState === 'failed'; }
      heartbeat(call,'peer_state');
      refreshStatus(call);
    });
    peer.addEventListener('iceconnectionstatechange', () => heartbeat(call,'ice_state'));
    const AudioContext = window.AudioContext || window.webkitAudioContext;
    if (AudioContext) { try { call.audioContext = new AudioContext(); call.audioContext.resume().catch(() => {}); startMicrophoneMeter(call); } catch { /* Playback still works without the meter. */ } }
    refreshStatus(call);
    await peer.setLocalDescription(await peer.createOffer()); await waitForIce(peer, call);
    if (!current(call)) return;
    const sdp = peer.localDescription?.sdp;
    if (!sdp) throw new Error('The browser could not prepare its audio connection.');
    call.request = {request_id:crypto.randomUUID(),agent_id:contact.id,sdp}; call.requestPending = true;
    const result = await api('/api/v1/companion/calls', 'POST', call.request);
    if (typeof result.id === 'string') call.id = result.id;
    if (!result.id || !result.sdp || !result.conversation_id) throw Object.assign(new Error('The service returned an incomplete call connection.'), {uncertain:true});
    call.requestPending = false;
    call.id = result.id; call.conversationId = result.conversation_id; call.voice = result.voice; call.providerConnected = result.state === 'connected';
    if (!current(call)) { await closeKnownCall(call); return; }
    $('continue-work').href = conversationLink(call.conversationId); $('continue-work').hidden = false;
    await peer.setRemoteDescription({type:'answer',sdp:result.sdp});
    try { startHuddleView(call); }
    catch {
      // Optional coordination presentation must not close a healthy voice call.
      $('call-huddles').hidden=false;
      $('call-huddle-view').textContent='Huddles are unavailable in this browser. Your call can continue.';
    }
    call.lastControlAt = Date.now(); call.lastWatchdogAt = Date.now();
    call.durationTimer = setInterval(() => updateDuration(call), 1000);
    call.surfaceTimer = setInterval(() => surfacePresence(call), 2000); surfacePresence(call);
    call.heartbeatTimer = setInterval(() => heartbeat(call), 5000);
    call.controlTimer = setInterval(() => checkControl(call), 500);
    requestWakeLock(call); heartbeat(call); poll(call);
  } catch (error) {
    if (call.authGeneration === state.authGeneration && (current(call) || state.recovery?.call === call) && !call.recoverySettled && call.requestPending && (!error.status || error.uncertain)) {
      if (state.recovery?.call !== call) state.recovery = {request:call.request,callId:call.id,call};
    }
    call.requestPending = false;
    if (current(call)) endCall(error.name === 'NotAllowedError' ? 'Microphone access was not granted. No voice connection was opened.' : error.message,true,true,'setup_failed');
    else if (call.id) closeKnownCall(call).catch(() => {});
  }
}

function attachRemoteAudio(call, stream) {
  if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
  call.remoteStream = stream; call.outputReady = false; audio.srcObject = stream;
  audio.play().catch(() => { if (current(call)) { $('enable-audio').hidden = false; $('speaker-state').textContent = 'Tap to hear'; } });
  if (call.audioContext) {
    try {
      call.audioSource?.disconnect();
      call.audioSource = call.audioContext.createMediaStreamSource(stream);
      call.analyser = call.audioContext.createAnalyser(); call.analyser.fftSize = 256; call.audioSource.connect(call.analyser);
      const values = new Uint8Array(call.analyser.fftSize);
      call.meterTimer = setInterval(() => {
        if (!current(call)) return;
        call.analyser.getByteTimeDomainData(values);
        const level = Math.sqrt(values.reduce((sum, x) => sum + ((x - 128) / 128) ** 2, 0) / values.length);
        // A continuous WebRTC track can be playing silence. Animate speaking
        // only when audio is actually playing and carries measured energy.
        call.rawOutputActive = level > .012;
        sampleConversationFloor(call);
        const speaking = !audio.paused && !audio.muted && audio.volume > 0 && call.rawOutputActive;
        call.agentSpeaking = call.audioContext.state === 'running' && !audio.paused ? speaking : null;
        $('presence').classList.toggle('speaking', speaking);
        $('speaker-state').textContent = audio.muted ? 'Listening to you' : speaking ? `${call.contact.label} speaking` : audio.paused ? 'Speaker paused' : 'Speaker on';
      }, 100);
    } catch { /* Do not fabricate a speaking indicator if analysis is unavailable. */ }
  }
}
audio.addEventListener('playing', () => { if (state.call) { state.call.outputReady = true; sampleConversationFloor(state.call); $('enable-audio').hidden = true; $('speaker-state').textContent = 'Speaker on'; } });
for (const name of ['pause','ended','waiting']) audio.addEventListener(name, () => {
  $('presence').classList.remove('speaking'); if (state.call) { state.call.outputReady = false; sampleConversationFloor(state.call); $('speaker-state').textContent = name === 'waiting' ? 'Speaker buffering' : 'Speaker paused'; }
});
$('enable-audio').onclick = async () => {
  try { await state.call?.audioContext?.resume(); await audio.play(); $('enable-audio').hidden = true; }
  catch { notice('Audio playback is still blocked. Check this browser’s sound permissions.'); }
};

function diagnostics(call, trigger) {
  const age = at => Math.min(3600000,Math.max(0,Math.round(Date.now() - (at || call.startedAt))));
  return {trigger,visibility:document.hidden?'hidden':'visible',peer_state:call.peer?.connectionState || 'unknown',
    ice_state:call.peer?.iceConnectionState || 'unknown',channel_state:call.channel?.readyState || 'unknown',
    audio_state:call.audioContext?.state || 'unknown',wake_lock:call.wakeLock&&!call.wakeLock.released?'held':navigator.wakeLock?'released':'unavailable',
    control_age_ms:age(call.lastControlAt),heartbeat_age_ms:age(call.lastHeartbeatAt),timer_gap_ms:Math.min(3600000,Math.max(0,Math.round(call.timerGap || 0)))};
}
function controlReceived(call, heartbeatReceived = false) {
  call.lastControlAt = Date.now();
  if (heartbeatReceived) call.lastHeartbeatAt = Date.now();
  if (!call.peerLostAt) call.reconcilingUntil = null;
}
function resumeCall(call, trigger) {
  if (!current(call) || !call.id) return;
  requestWakeLock(call);
  const now = Date.now(), stale = now - call.lastControlAt > 5000 || (call.peerLostAt && now - call.peerLostAt > 5000);
  // One bounded opportunity to reconcile after suspension. Repeated focus or
  // online events cannot extend it; only fresh control plus healthy media resets it.
  if (stale && !call.reconcilingUntil) call.reconcilingUntil = now + 10000;
  if (now - (call.lastResumeAt || 0) < 1000) { call.pendingHeartbeatTrigger = trigger; return; }
  call.lastResumeAt = now;
  for (const key of ['heartbeatRequest','pollRequest']) {
    const pending = call[key];
    if (pending && now - pending.startedAt > 2500) { call[key] = null; pending.abort.abort(); }
  }
  heartbeat(call,trigger); clearTimeout(call.pollTimer); poll(call);
  call.audioContext?.resume().catch(() => {});
  if (audio.srcObject && audio.paused) audio.play().catch(() => { if (current(call)) $('enable-audio').hidden = false; });
  refreshStatus(call);
}
function checkControl(call) {
  if (!current(call)) return;
  const now = Date.now(); call.timerGap = Math.max(0,now - call.lastWatchdogAt); call.lastWatchdogAt = now;
  if (call.timerGap > 5000) resumeCall(call,'timer_resume');
  if (call.reconcilingUntil && now < call.reconcilingUntil) return;
  if (now - call.lastControlAt > 15000) {
    endCall('Contact with the Companion service was lost. Your microphone and camera are off. Existing work has not been retried.',true,true,'control_timeout');
  } else if (call.peerLostAt && now - call.peerLostAt > 15000) {
    endCall('The audio connection could not recover. Your microphone and camera are off. Existing work stays in the conversation.',true,true,'audio_timeout');
  }
}
async function heartbeat(call, trigger = 'heartbeat') {
  if (!current(call) || !call.id) return;
  if (call.heartbeatRequest) { if (trigger !== 'heartbeat') call.pendingHeartbeatTrigger = trigger; return; }
  const request = {abort:new AbortController(),startedAt:Date.now()}; call.heartbeatRequest = request;
  trigger = call.pendingHeartbeatTrigger || trigger; call.pendingHeartbeatTrigger = null;
  try {
    await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/heartbeat`, 'POST', {diagnostics:diagnostics(call,trigger)}, {signal:request.abort.signal,keepalive:document.hidden});
    if (current(call) && call.heartbeatRequest === request) { call.heartbeatError = false; controlReceived(call,true); refreshStatus(call); }
  } catch (error) {
    if (current(call) && call.heartbeatRequest === request) {
      call.heartbeatError = true; refreshStatus(call);
      if (error.status === 401) endCall('Your sign-in expired. Your microphone and camera are off.',true,true,'auth_expired');
    }
  } finally { if (call.heartbeatRequest === request) call.heartbeatRequest = null; }
}
async function poll(call) {
  if (!current(call) || !call.id || call.pollRequest) return;
  clearTimeout(call.pollTimer);
  const request = {abort:new AbortController(),startedAt:Date.now()}; call.pollRequest = request;
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}?after=${call.cursor}`, 'GET', undefined, {signal:request.abort.signal});
    if (!current(call) || call.pollRequest !== request) return;
    call.pollError = false; controlReceived(call);
    call.providerConnected = result.state === 'connected';
    if (result.state === 'closing') call.closing = true;
    for (const event of result.events || []) renderEvent(call, event);
    renderCallPermissions(call, result.permissions);
    pollSurface(call);
    if (Number.isSafeInteger(result.next_cursor) && result.next_cursor >= call.cursor) call.cursor = result.next_cursor;
    if (['closed','interrupted'].includes(result.state)) {
      endCall(terminalMessage(result), false); showTerminal(result); return;
    }
    refreshStatus(call); refreshWorkStatus(call);
    refreshHuddles(call);
  } catch (error) {
    if (!current(call) || call.pollRequest !== request) return;
    call.pollError = true; refreshStatus(call);
    if (error.status === 401) { endCall('Your sign-in expired. Your microphone and camera are off.',true,true,'auth_expired'); return; }
  } finally {
    if (call.pollRequest === request) {
      call.pollRequest = null;
      if (current(call)) call.pollTimer = setTimeout(() => poll(call), call.pollError ? 2500 : 900);
    }
  }
}
function renderCallPermissions(call, permissions) {
  const pending = (Array.isArray(permissions) ? permissions : []).filter(p => p.conversation_id === call.conversationId
    && typeof p.id === 'string' && Number.isSafeInteger(p.expires_at) && p.expires_at > Date.now());
  const signature = JSON.stringify(pending);
  if (call.permissionSignature === signature) return;
  call.permissionSignature = signature;
  call.permissionDecisions ||= new Map();
  $('permission-alert').hidden = pending.length === 0;
  $('permission-alert').textContent = pending.length === 1 ? 'An action needs your approval · Review' : `${pending.length} actions need your approval · Review`;
  $('call-permissions').replaceChildren(...pending.map(permission => {
    const card = element('section', 'output-card call-permission'); card.dataset.permissionId = permission.id;
    card.append(element('h2', null, `${permission.agent || call.contact.label} needs your approval`),
      element('p', null, permission.tool_call?.title || 'Review the requested action'),
      element('small', null, `Only this request. Expires ${new Date(permission.expires_at).toLocaleTimeString()}.`));
    const details = element('details'), preview = element('pre', 'permission-detail', JSON.stringify(permission.tool_call || {}, null, 2));
    details.append(element('summary', null, 'Action details'), preview); card.append(details);
    const actions = element('div', 'permission-actions'), status = element('p', 'small', call.permissionDecisions.get(permission.id) || ''); status.setAttribute('role', 'status');
    // This surface offers only a decision for the exact pending request.
    const options = (permission.options || []).filter(o => ['allow_once','reject_once'].includes(o.kind));
    for (const option of [...options, {optionId:null,name:'Cancel request',kind:'cancel'}]) {
      const button = element('button', 'secondary', option.kind === 'cancel' ? option.name : `${option.name} (this request only)`);
      button.type = 'button'; button.disabled = call.permissionDecisions.has(permission.id);
      button.onclick = async () => {
        if (!current(call) || call.permissionDecisions.has(permission.id)) return;
        if (Date.now() >= permission.expires_at) { status.textContent = 'This approval request has expired.'; return; }
        call.permissionDecisions.set(permission.id, 'Sending this decision…');
        actions.querySelectorAll('button').forEach(b => { b.disabled = true; });
        try {
          await api(`/api/v1/conversations/${encodeURIComponent(call.conversationId)}/permissions/${encodeURIComponent(permission.id)}`, 'POST', {option_id:option.optionId});
          if (current(call)) { status.textContent = 'Decision received.'; call.permissionDecisions.set(permission.id, status.textContent); }
        } catch (error) {
          if (!current(call)) return;
          status.textContent = error.uncertain ? 'Decision receipt is unconfirmed. Check the conversation before deciding again.' : error.message;
          if (error.uncertain) call.permissionDecisions.set(permission.id, status.textContent);
          else { call.permissionDecisions.delete(permission.id); if (Date.now() < permission.expires_at) actions.querySelectorAll('button').forEach(b => { b.disabled = false; }); }
        }
      };
      actions.append(button);
    }
    card.append(actions, status); return card;
  }));
}
$('permission-alert').onclick = () => { setTab('space'); $('space-panel').scrollTop = 0; $('call-permissions').querySelector('button')?.focus({preventScroll:true}); };
function renderEvent(call, event) {
  if (!Number.isSafeInteger(event.seq) || event.seq <= call.cursor || call.seen.has(event.seq)) return;
  call.seen.add(event.seq);
  // Sequence retention is bounded; cursor remains authoritative on polling.
  if (call.seen.size > 500) call.seen.delete(call.seen.values().next().value);
  const data = event.data || {};
  renderContextEvent(call, event);
  if (event.kind === 'lifecycle' && data.type === 'host_close_requested') {
    // The host accepted closure; a spoken intention or decision alone is not
    // this receipt, and only the matching terminal later means the call ended.
    call.closing = true; call.closingSequence ??= event.seq;
    refreshStatus(call); refreshWorkStatus(call);
  } else if (event.kind === 'transcript' && ['user','assistant'].includes(data.role) && typeof data.text === 'string') {
    const followTranscript = followsEnd($('transcript-panel')), followCaption = followsEnd($('latest-caption'));
    const speaker = data.role === 'user' ? 'You' : call.contact.label;
    const previous = call.transcriptRow;
    // GPT-Live publishes transcript fragments, not complete user messages.
    // Join only same-speaker consecutive fragments; never infer a new turn.
    const sameSpeech = previous && previous.role === data.role && Number.isFinite(data.start_ms)
      && Number.isFinite(previous.endMs) && data.start_ms <= previous.endMs + 1200;
    if (sameSpeech && previous.text.length < 6000) {
      previous.text += data.text; previous.body.textContent = previous.text; previous.seq = event.seq; previous.endMs = data.end_ms;
    } else {
      const row = element('article', 'transcript-entry'); row.dataset.role = data.role;
      const body = element('p', null, data.text); row.append(element('strong', null, speaker), body); $('transcript').append(row);
      call.transcriptRow = {role:data.role,text:data.text,body,seq:event.seq,endMs:data.end_ms};
    }
    $('caption-speaker').textContent = speaker;
    $('caption-text').textContent = call.transcriptRow.text.slice(-360);
    $('transcript-empty').hidden = true;
    followChange($('transcript-panel'),followTranscript); followChange($('latest-caption'),followCaption);
  } else if (event.kind === 'delegation') {
    const waiting = ['waiting_transcript','dispatched'].includes(data.state);
    if (waiting) call.working.set(data.id, data.state); else { call.working.delete(data.id); call.progress.delete(data.id); }
    refreshWorkStatus(call);
    if (['failed','unknown','held'].includes(data.state) && data.message) addCard(call, 'Work update', data.message, call.conversationId, {card_id:`update:${event.seq}`,event_at:event.at});
    refreshStatus(call);
  } else if (event.kind === 'progress' && call.working.has(data.delegation_id)) {
    call.progress.set(data.delegation_id, data); refreshWorkStatus(call);
  } else if (event.kind === 'card' && typeof data.text === 'string') {
    addCard(call, typeof data.title === 'string' && data.title ? data.title.slice(0,300) : `From ${call.contact.label}`, data.text, data.conversation_id || call.conversationId, {...data,card_id:String(event.seq),event_at:event.at});
  } else if (event.kind === 'surface_command') {
    executeSurface(call, data);
  } else if (event.kind === 'action_observation') {
    $('connection-status').textContent = connectionObservationText(data);
    $('connection-status').hidden = false;
  } else if (event.kind === 'upload' && typeof data.upload_id === 'string') {
    renderShareEvent(call, data);
  } else if (event.kind === 'provider' && data.type === 'error') {
    // A known context append can lose its receipt during acknowledged close.
    // Preserve its Context entry and durable error, without a misleading
    // invitation to keep talking. Other errors, and earlier errors, stay loud.
    const correlationIds = [data.client_event_id, data.error?.client_event_id].filter(id => id !== undefined);
    const clientEventId = correlationIds[0];
    const exactCorrelation = correlationIds.length > 0
      && correlationIds.every(id => typeof id === 'string' && id.length > 0 && id === clientEventId);
    const closingAppend = Number.isSafeInteger(call.closingSequence) && event.seq > call.closingSequence
      && data.error?.code === 'context_injection_incomplete' && exactCorrelation
      && ['queued','accepted'].includes(call.contextItems.get(`voice:${clientEventId}`)?.facts.deliveryState);
    if (closingAppend) return;
    // The correlated context receipt describes the same rejection and stays
    // quiet. A server error alone neither closes the call nor retries work.
    const correlated = typeof data.client_event_id === 'string' || typeof data.error?.client_event_id === 'string';
    notice(correlated ? 'A voice update could not be confirmed. You can keep talking; saved results remain available.'
      : 'The voice service reported a problem. Your saved results remain available.');
  } else if (event.kind === 'error') {
    notice(typeof data.message === 'string' ? data.message : 'The voice service reported an error. Check the conversation before repeating a request.');
  } else if (event.kind === 'closed') {
    call.providerCloseReason = typeof data.reason === 'string' ? data.reason : '';
  }
}
function connectionObservationText(data) {
  const observation = data.observation || {}, state = observation.state, label = typeof data.label === 'string' && data.label ? data.label : 'The connection';
  if (observation.scope === 'conversation') {
    const outcome = inputOutcomeDetails(observation);
    if (outcome) {
      const wording = ({admitted:'The game connector admitted this request.',settled:'The game turn ended.',interrupted:'The game turn was interrupted.'})[state]
        || (observation.outcome_reason === 'not_retained' ? 'A retained handling record is not available for this request.' : 'Game handling remains unconfirmed.');
      return `${label}: ${wording}${outcome.blocker ? ' A blocker was reported; details are in Context.' : ''}${outcome.responses.length ? ' Public replies from the same turn are in Context.' : ''} The result of this request is not confirmed.`;
    }
    const wording = ({admitted:'The game connector admitted your request; forwarding to the agent is not confirmed.',settled:'The conversation turn ended; a reply and work outcome are not confirmed.',interrupted:'Conversation handling was interrupted; the outcome is unconfirmed.'})[state]
      || (state === 'replied' && observation.conversation?.reply_verified === true ? 'The agent replied. A reply does not confirm task acceptance or resumed work.' : 'Conversation handling remains unconfirmed.');
    return `${label}: ${wording}`;
  }
  if (observation.scope === 'message_delivery') return `${label}: ${observation.verified === true && ['completed','delivered'].includes(state) ? 'The message was delivered.' : 'Message delivery is unconfirmed.'} Further work is not verified.`;
  if (observation.scope === 'task') {
    const wording = ({accepted:'accepted your request',running:'is working on your request',waiting:'is waiting',blocked:'reported a blocker',reported_complete:'reports the work ready for your review',stopped:'stopped the work',budget_exhausted:'reached the work limit',needs_review:'needs your review',superseded:'has a newer task; the earlier receipt remains saved'})[state] || 'has no confirmed task status';
    return `${label} ${wording}. Reported status; not independently verified.`;
  }
  return `${label}: No recognized status is available. The work outcome is unconfirmed.`;
}
function inputOutcomeDetails(observation) {
  if (observation.scope !== 'conversation' || observation.provenance?.kind !== 'minecraft_input_outcome'
      || observation.provenance.version !== 1 || observation.fulfillment !== 'not_established'
      || observation.response_association !== 'same_turn_source_set_not_individual_fulfillment') return null;
  return {blocker:typeof observation.conversation?.blocked_reason === 'string' ? observation.conversation.blocked_reason.slice(0,1000) : '',
    responses:(Array.isArray(observation.responses) ? observation.responses : []).filter(response=>response && response.audience === 'public' && typeof response.text === 'string').slice(0,8)};
}
function connectionObservationContent(data) {
  const observation = data.observation || {}, outcome = inputOutcomeDetails(observation);
  let content = connectionObservationText(data);
  if (!outcome) return content;
  if (outcome.blocker) content += `\n\nReported blocker: ${outcome.blocker}`;
  if (outcome.responses.length) {
    content += '\n\nPublic replies associated with the same turn. Other requests may have been handled in that turn; these replies do not establish that this request was fulfilled.';
    for (const response of outcome.responses) {
      const at = contextTime(response.at);
      content += `\n\n${at ? at.toLocaleString() : 'Time not recorded'} — ${response.text.slice(0,4000)}`;
    }
  }
  if (observation.domain_private_responses_withheld === true || (Number.isSafeInteger(observation.private_responses_withheld) && observation.private_responses_withheld > 0)) content += '\n\nPrivate response text was withheld from this report.';
  // The domain reports omission as a flag, not a count or a promise that the
  // missing content remains retrievable. Local filtering has separate counts.
  if (observation.domain_responses_omitted === true) content += '\n\nSome replies are not included in this report.';
  else if (Number.isSafeInteger(observation.responses_omitted) && observation.responses_omitted > 0) content += '\n\nAdditional public replies were omitted from this bounded report.';
  return content;
}
function contextTime(value) {
  const date = typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? new Date(value)
    : typeof value === 'string' && /^\d{4}-\d\d-\d\dT/.test(value) ? new Date(value) : null;
  return date && Number.isFinite(date.getTime()) ? date : null;
}
function contextReceipt(call, key, facts, event) {
  const follow = followsEnd($('context-panel'));
  let entry = call.contextItems.get(key);
  if (!entry) {
    const node = element('details', 'context-entry'), summary = element('summary');
    const title = element('strong'), status = element('span', 'context-state'), timing = element('span', 'context-time');
    summary.append(title, status, timing);
    const source = element('p', 'context-source'), body = element('p', 'context-text'), note = element('p', 'context-explanation');
    node.append(summary, source, body, note); $('context-items').append(node);
    entry = {node,title,status,timing,source,body,note,facts:{},firstAt:event.at}; call.contextItems.set(key, entry);
  }
  entry.facts = {...entry.facts,...facts};
  const value = entry.facts, at = contextTime(event.at), first = contextTime(entry.firstAt), sourceAt = contextTime(value.sourceAt);
  entry.title.textContent = value.title;
  entry.status.textContent = value.status;
  entry.timing.replaceChildren();
  if (at) {
    const time = element('time', null, at.toLocaleTimeString([], {hour:'numeric',minute:'2-digit',second:'2-digit'}));
    time.dateTime = at.toISOString(); time.title = at.toLocaleString(); entry.timing.append(time);
  } else entry.timing.textContent = 'Time not recorded';
  const sourceParts = [value.source || 'Source not recorded'];
  if (sourceAt) sourceParts.push(`Source time: ${sourceAt.toLocaleString()}`);
  if (first) sourceParts.push(`First recorded: ${first.toLocaleString()}`);
  if (at && (!first || first.getTime() !== at.getTime())) sourceParts.push(`Receipt updated: ${at.toLocaleString()}`);
  entry.source.textContent = sourceParts.join(' · ');
  entry.body.textContent = value.content || 'The text was not retained with this receipt.';
  entry.note.textContent = value.note;
  $('context-empty').hidden = true;
  followChange($('context-panel'), follow);
}
function renderContextEvent(call, event) {
  const data = event.data || {}, text = value => typeof value === 'string' && value.trim() ? value : undefined;
  if (event.kind === 'context_delivery') {
    const descriptions = {
      queued:['Waiting for voice receipt','Sent to the voice connection. Acceptance has not been confirmed.'],
      accepted:['Accepted by voice service','The voice service accepted this text. This does not confirm it was used or spoken.'],
      not_queued:['Not sent to voice','This text was not queued for the voice connection.'],
      rejected:['Rejected by voice service','The voice service rejected this update. Saved source information remains separate.'],
      provider_error:['Voice receipt uncertain','A service error left delivery unconfirmed. This does not mean the text was spoken.']
    };
    const [status,note] = descriptions[data.state] || ['Voice receipt unknown','No recognized delivery state is available.'];
    const key = `voice:${text(data.event_id) || `event-${event.seq}`}`;
    const facts = {title:'Voice context',status,note,deliveryState:data.state};
    if (text(data.content)) facts.content = data.content;
    if (text(data.source_label)) facts.source = data.source_label;
    else if (text(data.channel) && !call.contextItems.get(key)?.facts.source) facts.source = ({thinking:'Companion · background context',commentary:'Companion · conversational context'})[data.channel] || 'Companion · voice context';
    if (data.source_at) facts.sourceAt = data.source_at;
    contextReceipt(call, key, facts, event);
  } else if (event.kind === 'context_supplied') {
    const target = data.target === 'agent' ? call.contact.label : 'voice session';
    contextReceipt(call, `supplied:${text(data.id) || event.seq}`, {
      title:text(data.title) || 'Supplied context',status:`Supplied to ${target}`,
      source:text(data.source_label) || 'Companion',sourceAt:data.source_at,content:text(data.content),
      note:'Recorded when supplied. This is not confirmation that the model used it or that it was spoken.'
    }, event);
  } else if (event.kind === 'delegation' && text(data.id)) {
    const descriptions = {
      waiting_transcript:['Collecting your request','Waiting for the available speech before sending this request.'],
      dispatched:['Saved for agent','Saved for agent handling. This does not confirm forwarding, task acceptance or completion.'],
      completed:['Agent response saved','A response is saved in this call’s history. Check the result for what was answered or completed.'],
      incorporated:['Included in an agent turn','Included with other input. This alone does not confirm each question was answered.'],
      held:['Request held','The request has not been dispatched.'],
      failed:['Request failed','The request could not be completed.'],
      unknown:['Result unconfirmed','A result is not confirmed. Existing work has not been replayed.']
    };
    const [status,note] = descriptions[data.state] || ['Request status unknown','No recognized delivery state is available.'];
    const facts = {title:`Request to ${call.contact.label}`,status,note,source:'Your call'};
    if (data.state === 'completed' && data.delivery_state === 'incorporated') facts.note = 'A shared agent response is saved in history. This does not confirm that every included question was answered.';
    if (text(data.request_text)) facts.content = data.request_text;
    if (text(data.message)) facts.note = `${note} ${data.message}`;
    contextReceipt(call, `request:${data.id}`, facts, event);
  } else if (event.kind === 'card' && text(data.awareness_id)) {
    contextReceipt(call, `observation:${data.awareness_id}`, {
      title:text(data.title) || 'Connection observation',status:'Saved observation',
      source:text(data.attribution) || text(data.source?.actor) || 'Connected service',sourceAt:data.source?.occurred_at,
      content:text(data.text),note:'Reported by this source. Saving an observation does not confirm it reached voice context, was spoken, or changed any work.'
    }, event);
  } else if (event.kind === 'action_observation') {
    const observation = data.observation || {};
    contextReceipt(call, `action:${text(observation.request_id) || text(data.request_id) || event.seq}`, {
      title:text(data.label) || 'Connection request',status:'Connection report',source:text(data.label) || 'Connected service',
      sourceAt:observation.observed_at,content:connectionObservationContent(data),
      note:'This report is separate from voice delivery and from what was spoken.'
    }, event);
  }
}
function refreshWorkStatus(call) {
  if (!current(call)) return;
  $('work-status').hidden = call.working.size === 0;
  if (!call.working.size) { $('work-status').textContent = ''; return; }
  if (call.closing) {
    $('work-status').textContent = 'This call is ending. Request status remains in the conversation.';
    return;
  }
  const update = [...call.progress.values()].at(-1);
  const elapsed = Number.isSafeInteger(update?.elapsed_seconds) && update.elapsed_seconds >= 0 ? ` after ${update.elapsed_seconds} seconds` : '';
  $('work-status').textContent = update
    ? (update.state === 'blocked' ? 'An earlier request needs review before more work can start. Open the conversation to review it.' : update.state === 'awaiting_permission' ? 'A request is waiting for your decision. Open work controls to review it.' : `Last update: a request was still pending${elapsed}. No completed result has arrived.`) + (call.working.size > 1 ? ` ${call.working.size} requests pending.` : '')
    : `${call.contact.label} has ${call.working.size === 1 ? 'a pending request' : call.working.size + ' pending requests'}. You can keep talking.`;
}
// Answer content uses sanitized Markdown and validated links. No raw HTML,
// executable URL schemes, frames, or arbitrary same-origin media endpoints.
function safeLink(value, conversationId) {
  return safeCardLink(value, conversationId, location.origin);
}
function mediaType(url) {
  try { return ({png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',webp:'image/webp',gif:'image/gif',mp4:'video/mp4',webm:'video/webm',ogv:'video/ogg',mp3:'audio/mpeg',wav:'audio/wav',ogg:'audio/ogg'})[new URL(url).pathname.split('.').at(-1).toLowerCase()]; } catch { return null; }
}
function resultLink(label, url) {
  const link = element('a', null, label); link.href = url; link.target = '_blank'; link.rel = 'noopener noreferrer'; return link;
}
function appendMedia(card, item, conversationId) {
  if (!item || typeof item !== 'object') return;
  const href = safeLink(item.url, conversationId); if (!href) return;
  const mime = item.mime_type || mediaType(href), name = item.name || 'Shared media';
  const kind = item.kind === 'link' ? null : item.kind === 'image' ? 'img' : item.kind === 'video' ? 'video' : ['image/png','image/jpeg','image/gif','image/webp'].includes(mime) ? 'img'
    : ['video/mp4','video/webm','video/ogg'].includes(mime) ? 'video'
      : ['audio/wav','audio/mpeg','audio/ogg'].includes(mime) ? 'audio' : null;
  const area = element('div', 'card-media'); area.append(resultLink(name + ' ↗', href));
  if (kind) {
    const load = () => {
      const media = element(kind, 'shared-media'); media.referrerPolicy = 'no-referrer';
      if (kind === 'img') { media.alt = name; media.loading = 'lazy'; }
      else { media.controls = true; media.preload = 'none'; if (kind === 'video') media.playsInline = true; }
      media.src = href; media.addEventListener('error', () => { media.remove(); area.append(element('p', 'small', 'Preview unavailable. You can open the link.')); }, {once:true});
      area.append(media);
    };
    if (new URL(href).origin === location.origin) load();
    else { const button = element('button', 'secondary media-preview-button', `Load ${kind === 'img' ? 'image' : kind} preview`); button.type = 'button'; button.onclick = () => { button.remove(); load(); }; area.append(button); }
  }
  card.append(area);
}
function appendRichText(card, text, conversationId, includeMedia = true) {
  const links = renderCardText(card, text, conversationId, location.origin), media = new Map();
  for (const item of links) if (mediaType(item.url)) media.set(item.url, item);
  if (includeMedia) for (const item of [...media.values()].slice(0,8)) appendMedia(card, item, conversationId);
}
const BOARD_LIMIT = 3;
const followers = new Map();
function followsEnd(panel) { return panel.scrollHeight - panel.clientHeight - panel.scrollTop < 48; }
function followChange(panel, follow = followsEnd(panel)) {
  const tracker = followers.get(panel); if (!tracker) return;
  tracker.follow = follow; tracker.unread = !follow;
  if (follow && !panel.hidden) panel.scrollTop = panel.scrollHeight;
  if (tracker.button) tracker.button.hidden = !tracker.unread || panel.hidden;
}
function resetFollowing() { for (const [panel,tracker] of followers) { tracker.follow = true; tracker.unread = false; panel.scrollTop = 0; if(tracker.button)tracker.button.hidden = true; } }
for (const [panelId,contentId,buttonId] of [['space-panel','board-items','space-new'],['transcript-panel','transcript','transcript-new'],['context-panel','context-items','context-new'],['latest-caption','caption-text',null]]) {
  const panel = $(panelId), button = buttonId && $(buttonId), tracker = {follow:true,unread:false,button}; followers.set(panel,tracker);
  panel.addEventListener('scroll', () => { tracker.follow = followsEnd(panel); if (tracker.follow) tracker.unread = false; if(button)button.hidden = !tracker.unread || panel.hidden; }, {passive:true});
  if (button) button.onclick = () => { panel.scrollTop = panel.scrollHeight; followChange(panel,true); };
  new ResizeObserver(() => { if (current(state.call) && !panel.hidden) followChange(panel,tracker.follow); }).observe($(contentId));
}
function readingProtected(call, entry) {
  return entry.card.contains(document.activeElement) || (call.readingCard === entry.id && !followsEnd($('space-panel')));
}
function boardProtected(call, entry) { return entry.pinned || readingProtected(call,entry); }
function updateBoard(call) {
  if (!current(call)) return;
  for (const entry of call.cards.values()) {
    const shown = call.board.includes(entry.id); entry.card.dataset.presented = String(shown);
    entry.show.hidden = shown; entry.pin.hidden = !shown; entry.remove.hidden = !shown;
    entry.pin.textContent = entry.pinned ? 'Unpin' : 'Pin'; entry.pin.setAttribute('aria-pressed',String(Boolean(entry.pinned)));
  }
  $('space-empty').hidden = call.board.length !== 0;
  $('board-count').textContent = `${call.board.length} of ${BOARD_LIMIT}`;
  $('history-count').textContent = String(call.cards.size - call.board.length);
  $('card-count').hidden = call.cards.size === 0; $('card-count').textContent = String(call.board.length);
}
function changeBoard(call, ids, user = false) {
  if (!current(call) || ids.length > BOARD_LIMIT || new Set(ids).size !== ids.length) throw Object.assign(new Error('The whiteboard has room for three items.'),{reason:'board_full'});
  if (ids.some(id=>!call.cards.has(id))) throw new Error('That item is no longer on this page. Open the conversation to find it.');
  if (!user && (call.board.some(id=>!ids.includes(id) && boardProtected(call,call.cards.get(id))) || (JSON.stringify(ids) !== JSON.stringify(call.board) && call.board.some(id=>readingProtected(call,call.cards.get(id)))))) throw Object.assign(new Error('An item is pinned or being read. The whiteboard was left as it is.'),{reason:'item_protected'});
  const following = followsEnd($('space-panel'));
  for (const id of call.board) if (!ids.includes(id)) { const entry = call.cards.get(id); entry.pinned = false; $('cards').prepend(entry.card); }
  call.board = ids;
  for (const [index,id] of ids.entries()) { const card = call.cards.get(id).card, before = $('board-items').children[index]; if (before !== card) $('board-items').insertBefore(card,before || null); }
  updateBoard(call); followChange($('space-panel'),following); surfacePresence(call);
}
function presentCard(call, id, user = false, reveal = false) {
  if (!call.board.includes(id)) {
    let ids = [...call.board];
    if (ids.length === BOARD_LIMIT && user) {
      const replace = ids.find(old=>!boardProtected(call,call.cards.get(old)));
      if (replace) ids = ids.filter(old=>old !== replace);
    }
    changeBoard(call,[...ids,id],user);
  }
  if (reveal) {
    const card = call.cards.get(id).card; setTab('space');
    $('space-panel').scrollTop += card.getBoundingClientRect().top - $('space-panel').getBoundingClientRect().top - 12;
    if (user) call.readingCard = id;
  }
}
function revealEntry(call, entry) {
  try { presentCard(call,entry.id,true,true); }
  catch { setTab('space'); $('board-history').open = true; $('space-panel').scrollTop += entry.card.getBoundingClientRect().top - $('space-panel').getBoundingClientRect().top - 12; }
}
function appendCard(call, card, intent = false) {
  if (!current(call)) return;
  const id = card.dataset.cardId || `local:${crypto.randomUUID()}`, following = followsEnd($('space-panel'));
  card.dataset.boardId = id; card.tabIndex = -1;
  const actions = element('div','board-item-actions');
  const show = element('button','text-button','Show on whiteboard'), pin = element('button','text-button','Pin'), remove = element('button','text-button','Put away');
  for (const button of [show,pin,remove]) { button.type = 'button'; actions.append(button); }
  const entry = {id,card,show,pin,remove,pinned:false}; call.cards.set(id,entry); card.append(actions); $('cards').prepend(card);
  show.onclick = () => revealEntry(call,entry);
  pin.onclick = () => { entry.pinned = !entry.pinned; updateBoard(call); surfacePresence(call); };
  remove.onclick = () => changeBoard(call,call.board.filter(value=>value !== id),true);
  card.addEventListener('pointerdown', () => { call.readingCard = id; });
  card.addEventListener('focusin', () => { call.readingCard = id; });
  // Automatic answers and observations stay in history. Only an intentional
  // presentation or a user-shared item asks for room on the whiteboard.
  if (intent) { try { presentCard(call,id,intent === 'user'); } catch { /* Saved in history; never evict something being read. */ } }
  while (call.cards.size > 40) {
    const oldest = [...call.cards.values()].find(item=>!call.board.includes(item.id) && !item.card.contains(document.activeElement) && !call.shares.get(item.card.dataset.uploadId)?.sending);
    if (!oldest) break;
    const share = call.shares.get(oldest.card.dataset.uploadId); if (share?.previewUrl) URL.revokeObjectURL(share.previewUrl);
    call.shares.delete(oldest.card.dataset.uploadId); call.cards.delete(oldest.id); oldest.card.remove();
  }
  updateBoard(call); followChange($('space-panel'),following);
}
function addCard(call, title, text, conversationId, data = {}) {
  if (!current(call)) return;
  const card = element('article', 'output-card');
  if (data.card_id) card.dataset.cardId = String(data.card_id);
  const heading = element('h2', null, title); heading.append(savedCardTime(data.event_at));
  card.append(heading);
  if (typeof data.huddle_id === 'string' && data.huddle_id && data.source?.huddle_id === data.huddle_id) {
    const attribution=data.attribution||{};
    card.append(element('p','card-context',`${typeof attribution.actor==='string'?attribution.actor:'A participant'} reported this update.`));
    for(const item of Array.isArray(data.evidence)?data.evidence:[]) {
      if(typeof item?.label!=='string'||typeof item?.reference!=='string')continue;
      const url=safeLink(item.reference,conversationId);
      card.append(url?resultLink(item.label,url):element('p','small',`${item.label}: ${item.reference}`));
    }
    if(conversationId===call.conversationId&&call.huddles) {
      const inspect=element('button','secondary','Inspect huddle');inspect.type='button';
      inspect.onclick=()=>{if(!current(call))return;$('call-huddles').hidden=false;$('call-huddles').open=true;void call.huddles.inspect(data.huddle_id);};card.append(inspect);
    }else{
      const link=element('a','conversation-link','Inspect huddle ↗');link.href=conversationLink(data.huddle_id);link.target='constellation-work';card.append(link);
    }
  }
  if (data.late) card.append(element('p', 'card-context', 'Answer to your earlier request'));
  const answer = element('div', 'card-answer'), note = data.kind === 'note', explicitMedia = !note && safeLink(data.url, conversationId);
  if (note) answer.append(element('p',null,text)); else appendRichText(answer, text, conversationId, !explicitMedia); card.append(answer);
  if (explicitMedia) appendMedia(card, {url:explicitMedia,name:typeof data.title === 'string' ? data.title : 'Shared media',kind:data.kind}, conversationId);
  if (data.truncated) addFullResultControl(call,card,answer,data,conversationId);
  for (const item of (!note && Array.isArray(data.attachments) ? data.attachments : []).slice(0,8)) {
    if (typeof item?.id === 'string') appendMedia(card, {...item,url:`/api/v1/conversations/${encodeURIComponent(conversationId)}/attachments/${encodeURIComponent(item.id)}`}, conversationId);
  }
  for (const item of (!note && Array.isArray(data.media) ? data.media : []).slice(0,8)) appendMedia(card, item, conversationId);
  // This named target is always our own work surface. noopener on a named
  // anchor would force a new browsing context on every click.
  if (conversationId) { const link = element('a', 'conversation-link', 'Open conversation ↗'); link.href = conversationLink(conversationId); link.target = 'constellation-work'; card.append(link); }
  appendCard(call, card, data.presentation === 'current');
}
function addFullResultControl(call, card, answer, data, conversationId) {
  const status = element('p','result-status','This is a shortened result. The complete answer is saved in the conversation.'); card.append(status);
  if (typeof data.message_id !== 'string' || !/^[A-Za-z0-9_-]{1,120}$/.test(data.message_id)) return;
  const path = `/api/v1/companion/calls/${encodeURIComponent(call.id)}/results/${encodeURIComponent(data.message_id)}`;
  // Fetch only the exact result route for this call. Event data cannot choose
  // another endpoint, external host, query string, or redirect destination.
  if (data.result_url !== path) return;
  const button = element('button','text-button full-result-button','Show full result'); button.type = 'button'; card.append(button);
  button.onclick = async () => {
    if (!current(call) || !card.isConnected || button.disabled) return;
    button.disabled = true; status.textContent = 'Loading the complete saved result…';
    try {
      const result = await api(path,'GET',undefined,{redirect:'error'});
      if (!current(call) || !card.isConnected) return;
      if (result.message_id !== data.message_id || typeof result.text !== 'string') throw new Error('The service did not return this saved result.');
      const following = followsEnd($('space-panel')); answer.replaceChildren(); appendRichText(answer,result.text,conversationId); followChange($('space-panel'),following);
      status.textContent = 'Full saved result shown.'; button.remove();
    } catch (error) { if (current(call) && card.isConnected) { status.textContent = error.message + ' You can open the conversation or try again.'; button.disabled = false; } }
  };
}
const detailTabs = ['space','transcript','context'];
function setTab(name) {
  for (const candidate of detailTabs) {
    const selected = name === candidate, button = $(candidate + '-tab');
    button.setAttribute('aria-selected', String(selected)); button.tabIndex = selected ? 0 : -1;
    $(candidate + '-panel').hidden = !selected;
    $(candidate + '-new').hidden = !selected || !followers.get($(candidate + '-panel'))?.unread;
  }
}
for (const name of detailTabs) {
  $(name + '-tab').onclick = () => setTab(name);
  $(name + '-tab').onkeydown = event => { if (['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) { event.preventDefault(); const next = event.key === 'Home' ? detailTabs[0] : event.key === 'End' ? detailTabs.at(-1) : detailTabs[(detailTabs.indexOf(name)+(event.key === 'ArrowRight' ? 1 : detailTabs.length-1))%detailTabs.length]; setTab(next); $(next + '-tab').focus(); } };
}

function stopCamera(call) {
  if (call) { call.cameraRequest = null; call.cameraLoading = false; call.lastCameraSharedId = null; }
  call?.cameraStream?.getTracks().forEach(track => track.stop()); if (call) call.cameraStream = null;
  $('voice-photos').checked = false; $('voice-photo-hint').textContent = 'Enable this to ask for a photo hands-free. Each request takes one photo; turning off the camera revokes permission.'; if (current(call)) surfacePresence(call);
  $('camera-preview').srcObject = null; $('camera-toggle').setAttribute('aria-pressed', 'false'); $('camera-label').textContent = 'Show something';
  $('camera-choice').replaceChildren(); $('camera-picker').hidden = true; $('camera-choice').disabled = false;
  $('camera-panel').hidden = true; $('camera-progress').hidden = true;
}
function updateShareCapability(call) {
  if (!current(call)) return;
  $('share-capability').textContent = call.contact.image_input === false
    ? `${call.contact.label} can save images here, but its current connection cannot inspect them. Text is supported; PDF inspection depends on document support.`
    : call.contact.image_input === true
      ? 'Images can be inspected when you ask.'
      : 'Image inspection is not verified. Files can still be saved.';
  $('camera-guide').textContent = call.contact.image_input === false
    ? `${call.contact.label} cannot inspect photos through this connection. You can still save a photo and describe what you see.`
    : `Point at what you want help with, then share a photo and tell ${call.contact.label} what you’re trying to do.`;
}
async function refreshCapabilities(call) {
  try {
    const result = await api(`/api/v1/companion/agents/${encodeURIComponent(call.contact.id)}/capabilities`);
    if (current(call)) { call.contact.image_input = typeof result.image_input === 'boolean' ? result.image_input : null; updateShareCapability(call); }
  } catch { if (current(call)) { call.contact.image_input = null; updateShareCapability(call); } }
}
async function toggleCamera() {
  const call = state.call; if (!current(call) || !call.id) return;
  if (call.cameraStream || call.cameraLoading) { stopCamera(call); return; }
  await openCamera(call);
}
async function listCameras(call) {
  if (!current(call) || !call.cameraStream || !navigator.mediaDevices.enumerateDevices) return;
  const stream = call.cameraStream;
  try {
    const devices = (await navigator.mediaDevices.enumerateDevices()).filter(device=>device.kind === 'videoinput' && device.deviceId);
    if (!current(call) || call.cameraStream !== stream) return;
    const selected = stream.getVideoTracks()[0]?.getSettings?.().deviceId;
    $('camera-choice').replaceChildren(...devices.map((device,index)=>{
      const option = element('option',null,device.label || `Camera ${index+1}`); option.value = device.deviceId; return option;
    }));
    if (selected && devices.some(device=>device.deviceId === selected)) $('camera-choice').value = selected;
    $('camera-picker').hidden = devices.length < 2;
  } catch { /* The granted default camera remains usable without enumeration. */ }
}
async function openCamera(call, deviceId) {
  const request = {}; call.cameraRequest = request; call.cameraLoading = true;
  // Mobile devices may not open a second camera while the first is in use.
  // Release only the old video track; microphone and voice transport stay put.
  call.cameraStream?.getTracks().forEach(track=>track.stop()); call.cameraStream = null;
  $('camera-preview').srcObject = null; $('camera-choice').disabled = true;
  $('camera-toggle').setAttribute('aria-pressed','true'); $('camera-label').textContent = 'Camera off';
  surfacePresence(call);
  try {
    const stream = await navigator.mediaDevices.getUserMedia({video:{...(deviceId?{deviceId:{exact:deviceId}}:{facingMode:{ideal:'environment'}}),width:{ideal:1280},height:{ideal:720}},audio:false});
    if (!current(call) || call.cameraRequest !== request) { stream.getTracks().forEach(track => track.stop()); return; }
    call.cameraStream = stream; $('camera-preview').srcObject = stream; $('camera-panel').hidden = false; setTab('space');
    $('camera-toggle').setAttribute('aria-pressed', 'true'); $('camera-label').textContent = 'Camera off';
    await $('camera-preview').play();
    if (!current(call) || call.cameraRequest !== request) return;
    $('space-panel').scrollTop = 0; surfacePresence(call); listCameras(call);
    for (const track of stream.getTracks()) track.addEventListener('ended', () => { if (current(call) && call.cameraStream === stream) stopCamera(call); });
  } catch (error) {
    if (current(call) && call.cameraRequest === request) { notice(error.name === 'NotAllowedError' ? 'Camera permission was not granted. The voice call is unchanged.' : 'The selected camera could not be opened. Your voice call can continue.'); stopCamera(call); }
  } finally { if (current(call) && call.cameraRequest === request) { call.cameraLoading = false; $('camera-choice').disabled = false; } }
}
$('camera-choice').onchange = () => { if (current(state.call) && state.call.cameraStream) openCamera(state.call,$('camera-choice').value); };
navigator.mediaDevices?.addEventListener?.('devicechange', () => listCameras(state.call));
function captureCue(call) {
  // A short mechanical double click belongs to the actual captured frame, not
  // an agent's promise or an upload/inspection result. No microphone input.
  const context = call.audioContext;
  if (!current(call)) return;
  const unavailable = () => notice('Photo captured. The shutter sound is unavailable in this browser.');
  if (context?.state !== 'running') { unavailable(); return; }
  try {
    const rate = context.sampleRate, buffer = context.createBuffer(1, Math.ceil(rate * .12), rate), samples = buffer.getChannelData(0);
    for (let i = 0; i < samples.length; i++) {
      const t = i / rate, local = t < .035 ? t : t - .065;
      if (local >= 0 && local < .035) samples[i] = (Math.random() * 2 - 1) * .22 * Math.exp(-local * 140);
    }
    const source = context.createBufferSource(); source.buffer = buffer; source.connect(context.destination);
    source.onended = () => source.disconnect(); source.start();
  } catch { unavailable(); /* A blocked cue must not lose or repeat the captured photo. */ }
}
async function capturePhoto() {
  const call = state.call, video = $('camera-preview');
  if (!current(call) || !call.cameraStream || !video.videoWidth || call.capturing) { notice('Wait for the camera preview before capturing a photo.'); return; }
  call.capturing = true; $('capture-photo').disabled = true;
  try {
    const canvas = document.createElement('canvas'), scale = Math.min(1,1280/video.videoWidth,1280/video.videoHeight);
    canvas.width = Math.round(video.videoWidth * scale); canvas.height = Math.round(video.videoHeight * scale);
    canvas.getContext('2d').drawImage(video,0,0,canvas.width,canvas.height);
    const blob = await new Promise(resolve => canvas.toBlob(resolve,'image/jpeg',.85));
    if (!current(call)) return;
    if (!blob) throw new Error('The camera could not capture this photo.');
    captureCue(call);
    await shareFile(call, new File([blob], 'companion-photo.jpg', {type:'image/jpeg'}), 'camera');
  } catch (error) { if (current(call)) notice(error.message); }
  finally { call.capturing = false; if (current(call)) $('capture-photo').disabled = false; }
}
function fileMime(file) {
  return ({md:'text/markdown',txt:'text/plain',csv:'text/csv',json:'application/json',pdf:'application/pdf',png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',gif:'image/gif',webp:'image/webp'})[file.name.split('.').at(-1).toLowerCase()] || file.type;
}
function shareStatus(share, status, message) {
  const following = followsEnd($('space-panel'));
  share.card.dataset.state = status; share.status.textContent = message; share.retry.hidden = true;
  const call = state.call;
  if (current(call) && share.source === 'camera' && call.lastCameraSharedId === share.id) {
    $('camera-progress').hidden = false;
    $('camera-progress').textContent = ({captured:'Photo taken. Preparing to share…',uploading:'Sharing this photo…',available:call.contact.image_input === false ? 'Photo saved. This agent cannot inspect it; describe what you see.' : 'Photo saved. Tell your agent what you’d like help with. Inspection is not yet confirmed.',failed:'This photo was not shared. You can keep talking.',unconfirmed:'The photo may be saved, but its receipt is missing. Check the conversation before sharing it again.'})[status] || message;
  }
  if (state.call?.lastSharedId === share.id) { $('share-receipt').hidden = false; $('share-receipt-text').textContent = `${share.name} · ${({captured:'Preparing to share…',uploading:'Uploading…',available:'Saved. Inspection not confirmed.',failed:'Not shared.',unconfirmed:'Upload receipt unknown.'})[status] || message}`; }
  followChange($('space-panel'),following);
}
function availableText(call, share) {
  return share.mime?.startsWith('image/') && call.contact.image_input === false
    ? 'Saved and available in this conversation. This connection cannot inspect images.'
    : 'Saved and available in this conversation. Sharing does not mean the agent has read it.';
}
function shareCard(call, id, name) {
  const card = element('article', 'output-card upload-card'); card.dataset.uploadId = id; card.dataset.cardId = `upload:${id}`;
  card.append(element('h2', null, name));
  const status = element('p', 'upload-status'), retry = element('button', 'text-button', 'Check voice notification'); retry.type = 'button'; retry.hidden = true;
  card.append(status, retry);
  const share = {id,name,card,status,retry,attachmentIds:[],sending:true}; call.shares.set(id,share);
  appendCard(call, card, 'user'); call.lastSharedId = id; return share;
}
async function notifyShare(call, share, status) {
  if (!current(call)) return null;
  return api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/uploads`, 'POST', {upload_id:share.id,state:status,source:share.source,name:share.name,attachment_ids:status === 'available' ? share.attachmentIds : []});
}
async function announceAvailable(call, share) {
  share.retry.hidden = true;
  try {
    const result = await notifyShare(call, share, 'available');
    if (!current(call) || !result) return;
    const confirmed = result.voice_awareness === 'queued';
    shareStatus(share, 'available', availableText(call, share) + (confirmed ? ' Voice notification queued.' : ' Voice notification has not been confirmed.'));
    share.retry.hidden = confirmed; share.availableRecorded = true; surfacePresence(call);
  } catch (error) {
    if (current(call)) { shareStatus(share, 'available', availableText(call, share) + ' Voice notification is unconfirmed.'); share.retry.hidden = false; }
  }
}
async function shareFile(call, file, source, uploadId = null) {
  if (!current(call) || !call.conversationId) return;
  const mime = fileMime(file), allowed = ['image/png','image/jpeg','image/gif','image/webp','application/pdf','text/plain','text/markdown','text/csv','application/json'];
  const name = (file.name || 'shared-file').replace(/[\\/"\u0000-\u001f\u007f]/g,'_').slice(0,150);
  const share = shareCard(call, uploadId || crypto.randomUUID(), name); Object.assign(share,{source,mime,sending:true,local:true});
  if (source === 'camera') call.lastCameraSharedId = share.id;
  share.retry.onclick = () => announceAvailable(call, share);
  if (call.board.includes(share.card.dataset.boardId)) revealEntry(call,call.cards.get(share.card.dataset.boardId));
  shareStatus(share,'captured',source === 'camera' ? 'Captured. Preparing to share…' : 'Selected. Preparing to share…');
  if (!allowed.includes(mime) || !file.size || file.size > (mime.startsWith('text/') || mime === 'application/json' ? 200000 : 2*1024*1024)) {
    share.sending = false; shareStatus(share,'failed','Not shared. Use an image or PDF up to 2 MB, or UTF-8 text, Markdown, CSV, or JSON up to 200 KB.');
    notifyShare(call,share,'failed').catch(() => {}); return;
  }
  if (mime.startsWith('image/')) { share.previewUrl = URL.createObjectURL(file); const preview = element('img','shared-media local-share-preview'); preview.src = share.previewUrl; preview.alt = name + ' — local copy'; share.card.insertBefore(preview,share.status); }
  try {
    await notifyShare(call,share,'captured').catch(() => null);
    if (!current(call)) return;
    shareStatus(share,'uploading','Uploading to this conversation…');
    await notifyShare(call,share,'uploading').catch(() => null);
    if (!current(call)) return;
    const data = await new Promise((resolve,reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result).split(',')[1]); reader.onerror = () => reject(new Error('This file could not be read.')); reader.readAsDataURL(file); });
    if (!current(call)) return;
    share.uploadStarted = true;
    const uploaded = await api(`/api/v1/conversations/${encodeURIComponent(call.conversationId)}/attachments`, 'POST', {name,mime_type:mime,data});
    if (!current(call)) return;
    if (typeof uploaded.id !== 'string') throw Object.assign(new Error('The service did not confirm a saved attachment.'),{uncertain:true});
    share.attachmentIds = [uploaded.id];
    shareStatus(share,'available',availableText(call,share) + ' Notifying voice…');
    const link = element('a','conversation-link','Open conversation ↗'); link.href = conversationLink(call.conversationId); link.target = 'constellation-work'; share.card.append(link);
    await announceAvailable(call,share);
  } catch (error) {
    if (current(call)) {
      const uncertain = share.uploadStarted && (!error.status || error.uncertain);
      shareStatus(share,uncertain ? 'unconfirmed' : 'failed',uncertain ? 'Upload receipt unavailable. The file may have been saved; check the conversation before sharing it again.' : 'Not shared. ' + error.message);
      // An unknown upload outcome is never replayed. A failure event only means
      // the artifact is unavailable to voice, not that storage definitely failed.
      notifyShare(call,share,'failed').catch(() => {});
    }
  } finally { share.sending = false; }
  return share;
}
function renderShareEvent(call, data) {
  let share = call.shares.get(data.upload_id);
  if (share?.local) return; // The local upload tracks its richer receipt/uncertainty state.
  share ||= shareCard(call,data.upload_id,typeof data.name === 'string' ? data.name : 'Shared item');
  share.availableRecorded = data.state === 'available'; share.sending = ['captured','uploading'].includes(data.state);
  shareStatus(share,data.state,({captured:'Captured on this device; not yet uploaded.',uploading:'Uploading to this conversation…',available:'Saved and available in this conversation. Sharing does not mean the agent has read it.',failed:'The item could not be made available. Check before sharing it again.'})[data.state] || 'Sharing status unavailable.');
}
async function shareFiles(files, source) {
  const call = state.call;
  if (!current(call) || !call.id || !call.connected) { notice('Connect a call before sharing an item.'); return; }
  setTab('space');
  if (files.length > 4) { notice('Share up to four files at a time.'); return; }
  for (const file of files) { if (!current(call)) return; await shareFile(call,file,source); }
}
$('choose-files').onclick = () => { heartbeat(state.call,'file_picker_open'); $('share-files').click(); };
$('share-files').onchange = event => { resumeCall(state.call,'file_picker_return'); shareFiles([...event.target.files],'file'); event.target.value = ''; };
$('share-files').addEventListener('cancel', () => resumeCall(state.call,'file_picker_return'));
$('space-panel').addEventListener('dragover', event => { event.preventDefault(); event.dataTransfer.dropEffect = 'copy'; $('space-panel').classList.add('dragging'); });
$('space-panel').addEventListener('dragleave', event => { if (!$('space-panel').contains(event.relatedTarget)) $('space-panel').classList.remove('dragging'); });
$('space-panel').addEventListener('drop', event => {
  event.preventDefault(); $('space-panel').classList.remove('dragging');
  const files = [...event.dataTransfer.files], text = event.dataTransfer.getData('text/plain');
  if (files.length) shareFiles(files,'drop'); else if (text.trim()) shareFiles([new File([text],'shared-text.txt',{type:'text/plain'})],'drop');
});
function pasteToShare(event) {
  if (event.target.closest('input,textarea,[contenteditable="true"]') && !(event.target === $('paste-input') && event.clipboardData?.files.length)) return;
  const files = [...(event.clipboardData?.files || [])], text = event.clipboardData?.getData('text/plain') || '';
  if (!files.length && !text.trim()) return;
  event.preventDefault(); if (event.target === $('paste-input') && !$('paste-input').value) $('paste-composer').hidden = true; shareFiles(files.length ? files : [new File([text],'pasted-text.txt',{type:'text/plain'})],'paste');
}
$('space-panel').addEventListener('paste',pasteToShare);
$('paste-input').addEventListener('paste',pasteToShare);
$('show-paste').onclick = () => { $('paste-composer').hidden = false; $('paste-input').focus(); };
$('paste-composer').onsubmit = event => {
  event.preventDefault(); const text = $('paste-input').value;
  if (!text.trim()) { $('paste-input').focus(); return; }
  if (!current(state.call) || !state.call.connected) return;
  $('paste-input').value = ''; $('paste-composer').hidden = true; shareFiles([new File([text],'shared-note.txt',{type:'text/plain'})],'paste');
};
$('reveal-share').onclick = () => {
  const call = state.call, share = call?.shares.get(call.lastSharedId); if (!current(call) || !share) return;
  const entry = call.cards.get(share.card.dataset.boardId); if(entry)revealEntry(call,entry);
};
function showModelFacts(contact, result) {
  const facts = $('voice-model-facts'); facts.replaceChildren(); $('voice-advanced-facts').replaceChildren();
  const fact = (list,name,value) => { list.append(element('dt',null,name),element('dd',null,String(value))); };
  fact(facts,'Voice model',[result.voice_provider,result.voice_model].filter(value=>typeof value === 'string' && value).join(' · ') || 'Not reported');
  if (current(state.call) && state.call.contact.id === contact.id) fact(facts,'Voice in this call',state.call.voice || 'Not reported');
  const backend = result.backend;
  fact(facts,'Reasoning runtime (last reported)',backend?.agent?.title || backend?.agent?.name || 'Not yet reported by this agent');
  fact(facts,'Reasoning model (last reported)',backend?.model?.name || backend?.model?.id || 'Not yet reported by this agent');
  if (Number.isFinite(backend?.observed_at)) fact(facts,'Last reported',new Date(backend.observed_at).toLocaleString());
  const options = Array.isArray(backend?.options) ? backend.options.filter(option=>['model','thought_level'].includes(option.category) && option.editable === false) : [];
  $('voice-advanced').hidden = options.length === 0;
  for (const option of options) fact($('voice-advanced-facts'),option.name || option.id,option.label || option.value || 'Not reported');
}
async function openVoiceSettings(contact) {
  state.voiceContact = contact; const generation = state.authGeneration;
  $('voice-settings-title').textContent = `Voice for ${contact.label || 'agent'}`; $('voice-choice').replaceChildren(); $('voice-model-facts').replaceChildren(); $('voice-advanced-facts').replaceChildren(); $('voice-advanced').hidden = true; $('voice-advanced').open = false;
  $('voice-settings-status').textContent = 'Loading available voices…'; $('save-voice').disabled = true; $('voice-choice').disabled = true;
  if (!$('voice-settings').open) $('voice-settings').showModal();
  try {
    const result = await api(`/api/v1/companion/agents/${encodeURIComponent(contact.id)}/voice-settings`);
    if (state.voiceContact !== contact || generation !== state.authGeneration) return;
    showModelFacts(contact,result);
    const voices = Array.isArray(result.available_voices) ? result.available_voices.filter(voice => typeof voice === 'string') : [];
    $('voice-choice').replaceChildren(...voices.map(voice => { const option = element('option',null,voice); option.value = voice; return option; }));
    $('voice-choice').value = result.voice; $('voice-choice').disabled = !voices.length; $('save-voice').disabled = !voices.length;
    $('voice-settings-status').textContent = voices.length ? 'Saved choices apply to your next call with this agent.' : 'No voice choices are available.';
  } catch (error) { if (state.voiceContact === contact && generation === state.authGeneration) $('voice-settings-status').textContent = error.message; }
}
$('voice-settings-form').onsubmit = async event => {
  event.preventDefault(); const contact = state.voiceContact, generation = state.authGeneration; if (!contact) return;
  $('save-voice').disabled = true;
  try {
    const result = await api(`/api/v1/companion/agents/${encodeURIComponent(contact.id)}/voice-settings`,'PUT',{voice:$('voice-choice').value});
    if (state.voiceContact !== contact || generation !== state.authGeneration) return;
    showModelFacts(contact,result); contact.voice = result.voice; $('voice-settings-status').textContent = `Saved. ${result.voice} will be used for your next call with ${contact.label}.`;
    const card = [...$('contacts').children].find(card => card.dataset.agentId === contact.id); if (card) card.querySelector('.contact-voice').textContent = `Voice · ${result.voice}`;
  } catch (error) { if (state.voiceContact === contact && generation === state.authGeneration) $('voice-settings-status').textContent = error.message; }
  finally { if (state.voiceContact === contact && generation === state.authGeneration) $('save-voice').disabled = false; }
};
$('close-voice-settings').onclick = () => $('voice-settings').close();
$('voice-settings').addEventListener('close', () => { state.voiceContact = null; });
$('call-voice-settings').onclick = () => { if (current(state.call)) openVoiceSettings(state.call.contact); };

function releaseTransport(call) {
  if (call.transportReleased) return;
  call.transportReleased = true; clearTimeout(call.transportCloseTimer); state.finalizingCalls.delete(call);
  call.channel?.close(); call.peer?.close(); call.audioSource?.disconnect(); call.audioContext?.close().catch(() => {});
}
function releaseMedia(call, retainTransport = false) {
  call.huddles?.destroy(); call.huddles=null; $('call-huddles').hidden=true;
  call.floor?.close(); clearTimeout(call.surfaceRetryTimer);
  clearInterval(call.surfaceTimer); clearInterval(call.micMeterTimer); call.micSource?.disconnect();
  clearTimeout(call.pollTimer); clearInterval(call.heartbeatTimer); clearInterval(call.durationTimer); clearInterval(call.meterTimer); clearInterval(call.controlTimer);
  call.pollRequest?.abort.abort(); call.heartbeatRequest?.abort.abort();
  call.stream?.getTracks().forEach(track => track.stop()); call.remoteStream?.getTracks().forEach(track => track.stop());
  stopCamera(call); for (const share of call.shares.values()) { if (share.previewUrl) URL.revokeObjectURL(share.previewUrl); } $('share-files').value = '';
  audio.pause(); audio.srcObject = null; call.audioContext?.suspend().catch(() => {});
  // Keep only the silent transport until the server observes the matching close
  // event. Devices stop immediately; an unavailable service cannot retain it.
  if (retainTransport) { state.finalizingCalls.add(call); call.transportCloseTimer = setTimeout(() => releaseTransport(call), 15000); }
  else releaseTransport(call);
  releaseWakeLock(call); $('presence').classList.remove('speaking');
}
async function confirmClose(id, result, ownsRecovery) {
  for (let attempt = 0; attempt < 12; attempt++) {
    if (!ownsRecovery()) throw new Error('The earlier call check is no longer current.');
    if (result.id !== id) throw new Error('The service could not confirm the earlier call’s identity.');
    if (['closed','interrupted'].includes(result.state)) return result;
    await new Promise(resolve => setTimeout(resolve, 500));
    if (!ownsRecovery()) throw new Error('The earlier call check is no longer current.');
    result = await api(`/api/v1/companion/calls/${encodeURIComponent(id)}?after=0`);
  }
  throw new Error('Your devices are off. The service is still closing the voice connection; check the same call again shortly.');
}
function terminalMessage(result) {
  if (typeof result.closure?.reason === 'string' && result.closure.reason) return result.closure.reason;
  return result.closure?.confirmed === true || result.state === 'closed'
    ? 'Your microphone and camera are off. The voice call is closed; accepted work can continue.'
    : 'Your microphone and camera are off. The local connection ended, but provider closure was not confirmed. Accepted work stays in the conversation.';
}
function showTerminal(result) {
  $('ended-detail').textContent = terminalMessage(result);
  $('ended-title').textContent = result.state === 'interrupted' || result.closure?.provider_reason === 'connection_lost'
    ? 'Call interrupted' : 'Call ended';
}
function finishRecovery(recovery, result) {
  if (state.recovery !== recovery || state.call || result.id !== recovery.callId || !['closed','interrupted'].includes(result.state)) return;
  if (recovery.call) recovery.call.recoverySettled = true;
  state.recovery = null;
  for (const call of state.finalizingCalls) if (call.id === result.id && call.authGeneration === state.authGeneration) releaseTransport(call);
  showRecovery(); showTerminal(result);
  if (!$('phonebook').hidden) notice(terminalMessage(result));
}
async function checkRecovery() {
  const recovery = state.recovery, authGeneration = state.authGeneration;
  if (!recovery?.callId || recovery.refreshing || state.call) return;
  recovery.refreshing = true; recovery.statusError = false; showRecovery();
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(recovery.callId)}?after=0`);
    if (authGeneration !== state.authGeneration || state.recovery !== recovery || state.call) return;
    if (result.id !== recovery.callId) throw new Error('Call identity did not match.');
    finishRecovery(recovery, result);
  } catch {
    if (authGeneration === state.authGeneration && state.recovery === recovery) recovery.statusError = true;
  } finally {
    if (authGeneration === state.authGeneration && state.recovery === recovery) { recovery.refreshing = false; showRecovery(); }
  }
}
async function closeKnownCall(call) {
  if (!call.id || call.authGeneration !== state.authGeneration) { releaseTransport(call); return; }
  const pending = state.recovery;
  const matches = pending && (pending.callId === call.id || (call.request && pending.request?.request_id === call.request.request_id));
  // The original create response can arrive after explicit same-request recovery
  // has already settled it. It must not revive that call or close it again.
  if (call.recoverySettled || state.call || state.generation !== call.generation + 1 || (pending && (!matches || pending.checking))) { releaseTransport(call); return; }
  const recovery = pending || {callId:call.id,request:null,call};
  recovery.callId = call.id; recovery.conversationId = call.conversationId; state.recovery = recovery; recovery.checking = true; showRecovery();
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/close`, 'POST', {reason:call.clientCloseReason || 'setup_failed',diagnostics:call.closeDiagnostics || diagnostics(call,'call_close')}, {keepalive:true});
    if (call.authGeneration !== state.authGeneration || state.recovery !== recovery) return;
    const terminal = await confirmClose(call.id, result, () => call.authGeneration === state.authGeneration && state.recovery === recovery);
    if (call.authGeneration === state.authGeneration) finishRecovery(recovery, terminal);
  } catch {
    // A later observation may already have settled this call. An old close
    // failure must not restore recovery over another call or owner's session.
  } finally {
    if (call.authGeneration === state.authGeneration && state.recovery === recovery) { recovery.checking = false; showRecovery(); }
    releaseTransport(call);
  }
}
function showRecovery() {
  const recovery = state.recovery;
  $('call-recovery').hidden = !recovery;
  $('recover-call').hidden = !recovery;
  $('recover-call').disabled = Boolean(recovery?.checking || recovery?.refreshing);
  $('recover-call').textContent = recovery?.callId ? 'End earlier call' : 'Resolve unconfirmed call';
  $('check-call-status').hidden = !recovery?.callId;
  $('check-call-status').disabled = Boolean(recovery?.refreshing);
  $('check-call-status').textContent = recovery?.refreshing ? 'Checking call status…' : 'Check call status';
  $('recovery-work-link').hidden = typeof recovery?.conversationId !== 'string';
  $('recovery-work-link').href = typeof recovery?.conversationId === 'string' ? conversationLink(recovery.conversationId) : '/';
  $('recovery-detail').textContent = !recovery ? '' : recovery.checking
    ? 'Your microphone and camera are off. Closing the voice connection… You can browse Contacts while this finishes.'
    : (recovery.discovered
      ? 'This page has not connected your microphone or camera. An earlier voice call is still open. Work your agent already accepted can continue in its conversation.'
      : 'Your microphone and camera are off. The service has not yet confirmed that the earlier voice call ended.')
      + ' You can browse Contacts; starting another call is paused until this is resolved.'
      + (recovery.statusError ? ' Call status could not be checked. Try again shortly.' : '');
  for (const button of $('contacts').querySelectorAll('.primary')) button.disabled = !state.available || Boolean(recovery) || Boolean(state.call);
}
function endCall(message = 'Your microphone and camera are off. Accepted work can continue.', sendClose = true, retainTransport = sendClose, reason = 'user_hangup') {
  const call = state.call; if (!call) return;
  call.clientCloseReason = reason; call.closeDiagnostics = diagnostics(call,reason === 'page_exit'?'page_exit':'call_close');
  call.ended = true; state.call = null; state.generation++; releaseMedia(call, retainTransport && Boolean(call.id)); document.body.classList.remove('call-active');
  $('call-screen').hidden = true; $('phonebook').hidden = true; $('call-ended').hidden = false;
  $('ended-title').textContent = call.id && ['user_hangup','surface_hangup'].includes(reason)
    ? 'Ending call…' : 'Call ended';
  $('ended-detail').textContent = message;
  $('ended-work-link').hidden = !call.conversationId;
  if (call.conversationId) $('ended-work-link').href = conversationLink(call.conversationId);
  if (call.requestPending) state.recovery = {request:call.request,callId:null,call};
  showRecovery();
  if (sendClose && call.id) closeKnownCall(call);
  $('back-to-contacts').focus();
}
$('recover-call').onclick = async () => {
  const recovery = state.recovery; if (!recovery || recovery.checking || recovery.refreshing) return;
  const authGeneration = state.authGeneration;
  recovery.checking = true; showRecovery();
  try {
    let id = recovery.callId;
    if (!id && recovery.request) {
      const saved = await api('/api/v1/companion/calls','POST',recovery.request); id = saved.id;
      if (authGeneration !== state.authGeneration || state.recovery !== recovery) return;
      recovery.callId = id; recovery.conversationId = saved.conversation_id;
    }
    if (authGeneration !== state.authGeneration || state.recovery !== recovery) return;
    if (!id) throw new Error('The service could not identify the earlier call.');
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(id)}/close`,'POST',{reason:'user_hangup'});
    if (authGeneration !== state.authGeneration || state.recovery !== recovery) return;
    const terminal = await confirmClose(id, result, () => authGeneration === state.authGeneration && state.recovery === recovery);
    if (authGeneration === state.authGeneration) finishRecovery(recovery, terminal);
  } catch (error) { if (authGeneration === state.authGeneration && state.recovery === recovery) notice(error.message); }
  finally { if (authGeneration === state.authGeneration && state.recovery === recovery) { recovery.checking = false; showRecovery(); } }
};
$('check-call-status').onclick = checkRecovery;
$('mute').onclick = () => { const call = state.call; if (!current(call) || !call.stream) return; call.muted = !call.muted; call.stream.getAudioTracks().forEach(track => { track.enabled = !call.muted; }); refreshStatus(call); };
$('hangup').onclick = () => endCall();
$('camera-toggle').onclick = toggleCamera;
$('capture-photo').onclick = capturePhoto;
$('screen-awake').onchange = () => { const call = state.call; if (!current(call)) return; call.keepScreenAwake = $('screen-awake').checked; if (call.keepScreenAwake) requestWakeLock(call); else releaseWakeLock(call); };
$('back-to-contacts').onclick = async () => {
  $('call-ended').hidden = true; $('phonebook').hidden = false; showRecovery();
  checkRecovery();
  const authGeneration = state.authGeneration;
  try { await openContacts(); } catch (error) { if (authGeneration === state.authGeneration) notice(error.message); }
};
$('dismiss-notice').onclick = () => notice('');
$('login-form').onsubmit = async event => {
  event.preventDefault(); $('sign-in').disabled = true; $('login-error').hidden = true;
  try { state.session = await api('/auth/login','POST',{username:$('username').value,password:$('password').value}); $('password').value = ''; await openContacts(); }
  catch (error) { $('login-error').textContent = error.message; $('login-error').hidden = false; }
  finally { $('sign-in').disabled = false; }
};
$('sign-out').onclick = async () => {
  if (state.call) endCall();
  try { await api('/auth/logout','POST',{}); state.session = null; showLogin(); }
  catch (error) { notice(error.message); }
};
document.addEventListener('visibilitychange', () => {
  const call = state.call; if (!current(call)) { if (!document.hidden) checkRecovery(); return; }
  if (document.hidden) {
    releaseWakeLock(call);
    $('foreground-note').textContent = 'This page is out of view. Your browser may pause audio; return here to continue.';
    sampleConversationFloor(call); heartbeat(call,'visibility_hidden');
  } else { $('foreground-note').textContent = 'Keep this page in the foreground. A screen lock or app switch may interrupt audio.'; sampleConversationFloor(call); resumeCall(call,'visibility_visible'); }
});
for (const event of ['focus','pageshow','online']) window.addEventListener(event, () => { if (state.call) resumeCall(state.call,event); else checkRecovery(); });
for (const event of ['blur','offline']) window.addEventListener(event, () => heartbeat(state.call,event));
document.addEventListener('resume', () => resumeCall(state.call,'resume'));
window.addEventListener('pagehide', () => { if (state.call) endCall(undefined,true,true,'page_exit'); });
start();

// Browser-owned surface controls. A host request can neither enable a sensor
// nor execute twice after an uncertain acknowledgment.
function startMicrophoneMeter(call) {
  try {
    call.micSource = call.audioContext.createMediaStreamSource(call.stream);
    call.micAnalyser = call.audioContext.createAnalyser(); call.micAnalyser.fftSize = 256;
    call.micSource.connect(call.micAnalyser); const values = new Uint8Array(256);
    call.micMeterTimer = setInterval(() => {
      if (!current(call)) return;
      if (call.audioContext.state !== 'running') { call.userSpeaking = null; call.rawMicActive = null; sampleConversationFloor(call); return; }
      call.micAnalyser.getByteTimeDomainData(values);
      const level = Math.sqrt(values.reduce((sum,x) => sum + ((x-128)/128)**2,0)/values.length);
      // Energy is an activity hint, not speaker identity or a completed turn.
      call.rawMicActive = level > .02 && !call.muted;
      if (call.rawMicActive) call.lastMicEnergy = Date.now();
      sampleConversationFloor(call);
    },100);
  } catch { call.userSpeaking = null; }
}
function sampleConversationFloor(call) {
  if (!current(call)) return;
  call.floor?.sample({mic:call.rawMicActive,output:call.rawOutputActive,
    available:!document.hidden && call.audioContext?.state === 'running' && !audio.paused
      && call.outputReady === true && call.peer?.connectionState === 'connected'});
}
async function surfacePresence(call) {
  if (!current(call) || !call.id) return;
  if (call.surfaceUpdating) { call.surfaceDirty = true; return; }
  call.surfaceUpdating = true; call.surfaceDirty = false;
  const board = call.board.filter(id=>/^\d+$/.test(id) || (id.startsWith('upload:') && call.shares.get(id.slice(7))?.availableRecorded));
  try {
    await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/surface/presence`, 'POST', {
      browser_id:call.browserId,camera_enabled:Boolean(call.cameraStream?.getVideoTracks().some(t=>t.readyState === 'live')),
      capture_consent:Boolean(call.cameraStream && $('voice-photos').checked),
      board_card_ids:board,pinned_card_ids:board.filter(id=>call.cards.get(id)?.pinned),focused_card_id:board.find(id=>readingProtected(call,call.cards.get(id))) || null,
      user_speaking:document.hidden ? null : call.userSpeaking,
      agent_speaking:document.hidden ? null : call.agentSpeaking,
      playback:call.playback || {state:'unknown',floor:'unknown',output_active:null,interruption_id:null},
      ...(call.interruption ? {interruption:call.interruption} : {}),
    });
    if (current(call)) call.surfaceReady = true;
  } catch { if (current(call)) call.surfaceReady = false; }
  finally { call.surfaceUpdating = false; if (current(call) && call.surfaceDirty) { clearTimeout(call.surfaceRetryTimer); call.surfaceRetryTimer = setTimeout(()=>surfacePresence(call),100); } }
}
$('voice-photos').onchange = () => {
  $('voice-photo-hint').textContent = $('voice-photos').checked
    ? 'You can say “Take a photo and help me with this.” Each request shares one photo, not a live feed.'
    : 'Enable this to ask for a photo hands-free. Each request takes one photo; turning off the camera revokes permission.';
  surfacePresence(state.call);
};
async function pollSurface(call) {
  if (!current(call) || !call.surfaceReady || call.surfacePolling) return;
  call.surfacePolling = true;
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/surface/commands?browser_id=${encodeURIComponent(call.browserId)}`);
    if (current(call)) for (const command of result.commands || []) executeSurface(call,command);
  } catch { /* Commands are durable; a lost connection never triggers local replay. */ }
  finally { call.surfacePolling = false; }
}
async function executeSurface(call, receipt) {
  const op = receipt?.operation_id;
  if (!current(call) || receipt.browser_id !== call.browserId || typeof op !== 'string' || !/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i.test(op)
      || receipt.state !== 'queued' || !Number.isSafeInteger(receipt.expires_at) || receipt.expires_at <= Date.now()
      || call.surfaceSeen.has(op) || !['view','capture','hangup'].includes(receipt.kind)) return;
  call.surfaceSeen.add(op);
  const ack = (state,attachments) => api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/surface/commands/${encodeURIComponent(op)}/ack`, 'POST', {browser_id:call.browserId,state,...(attachments ? {attachment_ids:attachments} : {})});
  try {
    const cameraReady = () => call.cameraStream?.getVideoTracks().some(track=>track.readyState === 'live') && $('voice-photos').checked;
    if (receipt.kind === 'capture' && (!cameraReady() || call.capturing)) { await ack('failed'); return; }
    const accepted = await ack('accepted');
    if (!current(call) || accepted.execute !== true || accepted.operation_id !== op || accepted.browser_id !== call.browserId
        || accepted.kind !== receipt.kind || accepted.state !== 'accepted' || !Number.isSafeInteger(accepted.expires_at)
        || accepted.expires_at !== receipt.expires_at || accepted.expires_at <= Date.now()) return;
    if (receipt.kind === 'view') {
      try { applyView(call,receipt.command); }
      catch (error) { await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/surface/commands/${encodeURIComponent(op)}/ack`,'POST',{browser_id:call.browserId,state:'failed',...(['board_full','item_protected'].includes(error.reason) ? {reason:error.reason} : {})}); return; }
      await ack('executed');
    } else if (receipt.kind === 'hangup') {
      // Release this call's media before acknowledging the effect. Closing a
      // voice session never cancels separately authorized background work.
      endCall('Your microphone and camera are off. Closing the voice call…', false, true,'surface_hangup');
      try { await ack('executed'); } finally { closeKnownCall(call); }
    } else {
      if (!cameraReady() || call.capturing || document.hidden) { await ack('failed'); return; }
      call.capturing = true; $('capture-photo').disabled = true;
      try {
        const video = $('camera-preview');
        if (!video.videoWidth || !video.videoHeight) throw new Error('The camera preview is not ready.');
        const canvas = document.createElement('canvas'), scale = Math.min(1,1024/video.videoWidth,1024/video.videoHeight);
        canvas.width = Math.round(video.videoWidth*scale); canvas.height = Math.round(video.videoHeight*scale);
        canvas.getContext('2d').drawImage(video,0,0,canvas.width,canvas.height);
        const blob = await new Promise(resolve=>canvas.toBlob(resolve,'image/jpeg',.75));
        if (!blob) throw new Error('The camera could not capture this photo.');
        if (!current(call)) return;
        captureCue(call);
        if (blob.size>524288) throw new Error('This photo could not be shared within the image limit.');
        // The frame is captured once. An unknown upload never recaptures it.
        await ack('executed');
        if (!current(call)) return;
        const share = await shareFile(call,new File([blob],'voice-photo.jpg',{type:'image/jpeg'}),'camera',op);
        if (share?.attachmentIds?.length === 1) await ack('uploaded',share.attachmentIds);
        else if (share && !share.uploadStarted) await ack('failed');
      } finally { call.capturing = false; if (current(call)) $('capture-photo').disabled = false; }
    }
  } catch (error) {
    // Losing an execute/upload receipt leaves an unknown outcome on the host.
    // Never infer that the command did not happen, and never run it again.
    if (current(call)) notice('That screen or photo request could not be confirmed. Check the shared space before asking again.');
  }
}
function applyView(call, command) {
  if (!current(call) || !command) throw new Error('The call is no longer active.');
  if (command.action === 'replace_board') { changeBoard(call,command.card_ids); return; }
  if (command.action === 'add_card') { presentCard(call,command.card_id); return; }
  if (command.action === 'remove_card') { changeBoard(call,call.board.filter(id=>id !== command.card_id)); return; }
  if (command.action === 'clear_board') { changeBoard(call,[]); return; }
  if (command.action === 'show_shared') { setTab('space'); return; }
  if (command.action === 'show_transcript') { setTab('transcript'); return; }
  if (command.action === 'show_context') { setTab('context'); return; }
  const panel = detailTabs.map(name=>$(name+'-panel')).find(panel=>!panel.hidden);
  if (['scroll_up','scroll_down'].includes(command.action)) {
    panel.scrollBy({top:Math.max(160,panel.clientHeight*.75)*(command.action === 'scroll_up' ? -1 : 1),behavior:matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth'}); return;
  }
  let id = command.card_id;
  if (['next_card','previous_card'].includes(command.action)) {
    const index = call.board.indexOf(call.focusedCard), next = index<0 ? (command.action === 'previous_card' ? call.board.length-1 : 0) : Math.max(0,Math.min(call.board.length-1,index+(command.action === 'previous_card' ? -1 : 1)));
    id = call.board[next];
  }
  if (!call.cards.has(id)) throw new Error('That saved item is not on this page.');
  const reading = call.cards.get(call.readingCard);
  if (reading && reading.id !== id && readingProtected(call,reading)) throw Object.assign(new Error('The current item is being read.'),{reason:'item_protected'});
  presentCard(call,id,false,true); call.focusedCard = id;
  const target = call.cards.get(id).card; target.classList.add('surface-focused'); setTimeout(()=>target.classList.remove('surface-focused'),1800);
  surfacePresence(call);
}
