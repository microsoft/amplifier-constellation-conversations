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
const state = {session:null, contacts:[], available:false, call:null, generation:0, authGeneration:0, recovery:null, voiceContact:null};
const audio = $('remote-audio');

async function api(path, method = 'GET', body, options = {}) {
  const headers = body === undefined ? {} : {'Content-Type':'application/json'};
  if (method !== 'GET' && state.session?.csrf) headers['X-CSRF-Token'] = state.session.csrf;
  const abort = new AbortController(), timer = setTimeout(() => abort.abort(), path === '/api/v1/companion/calls' ? 40000 : 10000);
  try {
    const response = await fetch(path, {method, headers, credentials:'same-origin', cache:'no-store', signal:abort.signal,
      body:body === undefined ? undefined : JSON.stringify(body), ...options});
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
  } finally { clearTimeout(timer); }
}
function notice(message) { $('notice').hidden = !message; $('notice-text').textContent = message || ''; }
function current(call) { return Boolean(call && state.call === call && call.generation === state.generation && !call.ended); }
function conversationLink(id) { return '/?conversation=' + encodeURIComponent(id); }

async function openContacts() {
  const authGeneration = state.authGeneration;
  const config = await api('/api/v1/companion');
  if (authGeneration !== state.authGeneration) return;
  state.contacts = Array.isArray(config.contacts) ? config.contacts : [];
  state.available = config.available === true;
  const prior = config.active_call;
  if (!state.call && prior && typeof prior.id === 'string' && !['closed','interrupted'].includes(prior.state)) {
    state.recovery = {callId:prior.id,request:null,discovered:true};
    $('phonebook').hidden = true; $('call-screen').hidden = true; $('call-ended').hidden = false;
    $('ended-title').textContent = 'An earlier call is still open.';
    $('ended-work-link').hidden = typeof prior.conversation_id !== 'string';
    if (typeof prior.conversation_id === 'string') $('ended-work-link').href = conversationLink(prior.conversation_id);
    showRecovery();
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
    button.disabled = !state.available || Boolean(state.recovery);
    button.append(element('span', null, '↗')); button.lastChild.setAttribute('aria-hidden', 'true');
    button.onclick = () => startCall(contact);
    card.append(button); return card;
  }));
}
function showLogin() {
  if (state.call) endCall('Your sign-in expired. Your microphone and camera are off.');
  // A later login must not inherit private presentation or recovery state. Old
  // asynchronous callbacks may close their media, but cannot repopulate it.
  state.authGeneration++; state.recovery = null; state.contacts = [];
  $('voice-settings').close(); state.voiceContact = null; $('voice-choice').replaceChildren(); $('voice-settings-status').textContent = ''; $('cards').replaceChildren(); $('transcript').replaceChildren(); $('contacts').replaceChildren();
  $('caption-speaker').textContent = ''; $('caption-text').textContent = ''; notice('');
  $('ended-detail').textContent = '';
  $('call-ended').hidden = true; $('call-screen').hidden = true; $('phonebook').hidden = false; $('recover-call').hidden = true;
  for (const id of ['continue-work','ended-work-link']) { $(id).hidden = true; $(id).href = '/'; }
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
  if (call.networkError || call.peer?.connectionState === 'disconnected') {
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
async function requestWakeLock(call) {
  if (!current(call) || document.hidden || !navigator.wakeLock || call.wakeLock) return;
  try {
    const lock = await navigator.wakeLock.request('screen');
    if (!current(call)) { await lock.release(); return; }
    call.wakeLock = lock;
    lock.addEventListener('release', () => { if (call.wakeLock === lock) call.wakeLock = null; });
  } catch { /* A denied wake lock never prevents a call. */ }
}
function updateDuration(call) {
  if (!current(call)) return;
  const seconds = Math.floor((Date.now() - call.startedAt) / 1000);
  $('call-duration').textContent = `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}
async function startCall(contact) {
  if (state.call || state.recovery || !state.available) return;
  const call = {generation:++state.generation, authGeneration:state.authGeneration, contact, id:null, request:null, ended:false, connected:false,
    cursor:0, working:new Map(), progress:new Map(), seen:new Set(), shares:new Map(), muted:false, startedAt:Date.now(), timers:new Set(), transcriptRow:null};
  state.call = call;
  document.body.classList.add('call-active');
  notice(''); $('phonebook').hidden = true; $('call-ended').hidden = true; $('call-screen').hidden = false;
  $('call-avatar').textContent = initials(contact.label); $('call-name').textContent = contact.label || 'Your agent';
  $('call-description').textContent = contact.description || 'The same agent. Another place to talk.';
  $('call-duration').textContent = '00:00'; $('cards').replaceChildren(); $('transcript').replaceChildren();
  $('transcript-empty').hidden = false; $('space-empty').hidden = false; $('work-status').hidden = true;
  $('card-count').hidden = true; $('card-count').textContent = '0'; $('continue-work').hidden = true;
  $('caption-speaker').textContent = ''; $('caption-text').textContent = 'Take your time. There’s room for a pause.';
  $('speaker-state').textContent = 'Speaker waiting'; $('enable-audio').hidden = true;
  $('hangup').disabled = false; setTab('space'); refreshStatus(call); updateShareCapability(call);
  refreshCapabilities(call);
  try {
    if (!navigator.mediaDevices?.getUserMedia || !window.RTCPeerConnection) throw new Error('Voice calling needs a browser with WebRTC and microphone access on HTTPS.');
    // This function is entered only from an explicit Call click. Never request
    // media permissions while restoring login, rendering contacts, or polling.
    const stream = await navigator.mediaDevices.getUserMedia({audio:{echoCancellation:true,noiseSuppression:true,autoGainControl:true},video:false});
    if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
    call.stream = stream;
    const peer = new RTCPeerConnection(); call.peer = peer;
    for (const track of stream.getTracks()) { peer.addTrack(track, stream); track.addEventListener('ended', () => { if (current(call)) endCall('The microphone became unavailable. Your camera is off, and accepted work can continue.'); }); }
    const channel = peer.createDataChannel('oai-events'); call.channel = channel;
    // Receive-only by design. The server sideband is the sole owner of provider
    // controls and delegation. Durable API events are also the sole transcript
    // source, so data-channel copies cannot duplicate captions or task results.
    channel.addEventListener('message', () => {});
    peer.addEventListener('track', event => attachRemoteAudio(call, event.streams?.[0] || new MediaStream([event.track])));
    peer.addEventListener('connectionstatechange', () => {
      if (!current(call)) return;
      if (peer.connectionState === 'connected') { call.startedAt = call.audioStartedAt || Date.now(); call.audioStartedAt = call.startedAt; call.peerLostAt = null; call.failed = false; }
      if (['failed','disconnected'].includes(peer.connectionState)) { call.peerLostAt ||= Date.now(); call.failed = peer.connectionState === 'failed'; }
      refreshStatus(call);
    });
    const AudioContext = window.AudioContext || window.webkitAudioContext;
    if (AudioContext) { try { call.audioContext = new AudioContext(); call.audioContext.resume().catch(() => {}); } catch { /* Playback still works without the meter. */ } }
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
    call.id = result.id; call.conversationId = result.conversation_id; call.providerConnected = result.state === 'connected';
    if (!current(call)) { await closeKnownCall(call); return; }
    $('continue-work').href = conversationLink(call.conversationId); $('continue-work').hidden = false;
    await peer.setRemoteDescription({type:'answer',sdp:result.sdp});
    call.lastControlAt = Date.now();
    call.durationTimer = setInterval(() => updateDuration(call), 1000);
    call.heartbeatTimer = setInterval(() => heartbeat(call), 5000);
    call.controlTimer = setInterval(() => {
      if (!current(call)) return;
      if (Date.now() - call.lastControlAt > 15000) {
        endCall('Contact with the Companion service was lost. Your microphone and camera are off. Existing work has not been retried.');
      } else if (call.peerLostAt && Date.now() - call.peerLostAt > 15000) {
        endCall('The audio connection could not recover. Your microphone and camera are off. Existing work stays in the conversation.');
      }
    }, 500);
    await requestWakeLock(call); heartbeat(call); poll(call);
  } catch (error) {
    if (call.authGeneration === state.authGeneration && call.requestPending && (!error.status || error.uncertain)) {
      state.recovery = {request:call.request,callId:call.id};
    }
    call.requestPending = false;
    if (current(call)) endCall(error.name === 'NotAllowedError' ? 'Microphone access was not granted. No voice connection was opened.' : error.message);
    else if (call.id) closeKnownCall(call).catch(() => {});
  }
}

function attachRemoteAudio(call, stream) {
  if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
  call.remoteStream = stream; audio.srcObject = stream;
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
        const speaking = !audio.paused && !audio.muted && audio.volume > 0 && level > .012;
        $('presence').classList.toggle('speaking', speaking);
        $('speaker-state').textContent = speaking ? `${call.contact.label} speaking` : audio.paused ? 'Speaker paused' : 'Speaker on';
      }, 100);
    } catch { /* Do not fabricate a speaking indicator if analysis is unavailable. */ }
  }
}
audio.addEventListener('playing', () => { if (state.call) { $('enable-audio').hidden = true; $('speaker-state').textContent = 'Speaker on'; } });
for (const name of ['pause','ended','waiting']) audio.addEventListener(name, () => {
  $('presence').classList.remove('speaking'); if (state.call) $('speaker-state').textContent = name === 'waiting' ? 'Speaker buffering' : 'Speaker paused';
});
$('enable-audio').onclick = async () => {
  try { await state.call?.audioContext?.resume(); await audio.play(); $('enable-audio').hidden = true; }
  catch { notice('Audio playback is still blocked. Check this browser’s sound permissions.'); }
};

async function heartbeat(call) {
  if (!current(call) || !call.id || call.heartbeating) return;
  call.heartbeating = true;
  try { await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/heartbeat`, 'POST', {}); if (current(call)) call.lastControlAt = Date.now(); }
  catch (error) { if (current(call)) { call.networkError = true; refreshStatus(call); if (error.status === 401) endCall('Your sign-in expired. Your microphone and camera are off.'); } }
  finally { call.heartbeating = false; }
}
async function poll(call) {
  if (!current(call) || !call.id) return;
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}?after=${call.cursor}`);
    if (!current(call)) return;
    call.networkError = false; call.lastControlAt = Date.now();
    call.providerConnected = result.state === 'connected';
    for (const event of result.events || []) renderEvent(call, event);
    if (Number.isSafeInteger(result.next_cursor) && result.next_cursor >= call.cursor) call.cursor = result.next_cursor;
    if (['closed','interrupted'].includes(result.state)) { endCall(result.state === 'interrupted' ? 'The voice connection was interrupted. Accepted work stays in the conversation.' : 'Your microphone and camera are off. Accepted work can continue.', false); return; }
    refreshStatus(call);
  } catch (error) {
    if (!current(call)) return;
    call.networkError = true; refreshStatus(call);
    if (error.status === 401) { endCall('Your sign-in expired. Your microphone and camera are off.'); return; }
  }
  if (current(call)) call.pollTimer = setTimeout(() => poll(call), call.networkError ? 2500 : 900);
}
function renderEvent(call, event) {
  if (!Number.isSafeInteger(event.seq) || event.seq <= call.cursor || call.seen.has(event.seq)) return;
  call.seen.add(event.seq);
  // Sequence retention is bounded; cursor remains authoritative on polling.
  if (call.seen.size > 500) call.seen.delete(call.seen.values().next().value);
  const data = event.data || {};
  if (event.kind === 'transcript' && ['user','assistant'].includes(data.role) && typeof data.text === 'string') {
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
      while ($('transcript').children.length > 160) $('transcript').firstChild.remove();
    }
    $('caption-speaker').textContent = speaker;
    $('caption-text').textContent = call.transcriptRow.text.slice(-360);
    $('transcript-empty').hidden = true;
    if (!$('transcript-panel').hidden) $('transcript-panel').scrollTop = $('transcript-panel').scrollHeight;
  } else if (event.kind === 'delegation') {
    const waiting = ['waiting_transcript','dispatched'].includes(data.state);
    if (waiting) call.working.set(data.id, data.state); else { call.working.delete(data.id); call.progress.delete(data.id); }
    refreshWorkStatus(call);
    if (['failed','unknown','held'].includes(data.state) && data.message) addCard(call, 'Work update', data.message, call.conversationId);
    refreshStatus(call);
  } else if (event.kind === 'progress' && call.working.has(data.delegation_id)) {
    call.progress.set(data.delegation_id, data); refreshWorkStatus(call);
  } else if (event.kind === 'card' && typeof data.text === 'string') {
    addCard(call, typeof data.title === 'string' && data.title ? data.title.slice(0,300) : `From ${call.contact.label}`, data.text, data.conversation_id || call.conversationId, data);
  } else if (event.kind === 'upload' && typeof data.upload_id === 'string') {
    renderShareEvent(call, data);
  } else if (event.kind === 'error') {
    notice(typeof data.message === 'string' ? data.message : 'The voice service reported an error. Check the conversation before repeating a request.');
  } else if (event.kind === 'closed') {
    call.closeReason = typeof data.reason === 'string' ? data.reason : '';
  }
}
function refreshWorkStatus(call) {
  if (!current(call)) return;
  $('work-status').hidden = call.working.size === 0;
  if (!call.working.size) { $('work-status').textContent = ''; return; }
  const update = [...call.progress.values()].at(-1);
  const elapsed = Number.isSafeInteger(update?.elapsed_seconds) && update.elapsed_seconds >= 0 ? ` after ${update.elapsed_seconds} seconds` : '';
  $('work-status').textContent = update
    ? (update.state === 'awaiting_permission' ? 'A request is waiting for your decision. Open work controls to review it.' : `Last update: a request was still pending${elapsed}. No completed result has arrived.`) + (call.working.size > 1 ? ` ${call.working.size} requests pending.` : '')
    : `${call.contact.label} has ${call.working.size === 1 ? 'a pending request' : call.working.size + ' pending requests'}. You can keep talking.`;
}
// All answer content is built from DOM text and validated links. No HTML,
// executable URL schemes, frames, or arbitrary same-origin media endpoints.
function safeLink(value, conversationId) {
  if (typeof value !== 'string' || /[\u0000-\u0020\u007f]/.test(value)) return null;
  try {
    const url = new URL(value, location.origin);
    if (!['http:','https:'].includes(url.protocol) || url.username || url.password) return null;
    if (url.origin === location.origin) {
      const prefix = `/api/v1/conversations/${encodeURIComponent(conversationId)}/attachments/`;
      if (!conversationId || !url.pathname.startsWith(prefix) || !/^[^/]+$/.test(url.pathname.slice(prefix.length)) || url.search || url.hash) return null;
    } else if (!/^https?:\/\//i.test(value)) return null;
    return url.href;
  } catch { return null; }
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
  const body = element('p'), media = new Map(); let at = 0;
  const pattern = /!?\[([^\]\n]{1,240})\]\(([^\s)]+)\)|https?:\/\/[^\s<>]+/g;
  for (const match of String(text).matchAll(pattern)) {
    body.append(document.createTextNode(String(text).slice(at, match.index)));
    const raw = match[2] || match[0].replace(/[.,;!?]+$/, ''), href = safeLink(raw, conversationId);
    if (href) { body.append(resultLink(match[1] || raw, href)); if (mediaType(href)) media.set(href, {url:href,name:match[1] || 'Linked media'}); }
    else body.append(document.createTextNode(match[0]));
    if (!match[2] && href && raw.length < match[0].length) body.append(document.createTextNode(match[0].slice(raw.length)));
    at = match.index + match[0].length;
  }
  body.append(document.createTextNode(String(text).slice(at))); card.append(body);
  if (includeMedia) for (const item of [...media.values()].slice(0,8)) appendMedia(card, item, conversationId);
}
function appendCard(call, card) {
  if (!current(call)) return;
  $('cards').append(card);
  while ($('cards').children.length > 40) {
    const first = [...$('cards').children].find(node => !call.shares.get(node.dataset.uploadId)?.sending); if (!first) break;
    const oldShare = call.shares.get(first.dataset.uploadId);
    if (oldShare?.previewUrl) URL.revokeObjectURL(oldShare.previewUrl);
    call.shares.delete(first.dataset.uploadId); first.remove();
  }
  $('space-empty').hidden = true; $('card-count').hidden = false; $('card-count').textContent = String($('cards').children.length);
}
function addCard(call, title, text, conversationId, data = {}) {
  if (!current(call)) return;
  const card = element('article', 'output-card');
  const heading = element('h2', null, title); heading.append(element('small', null, new Date().toLocaleTimeString([], {hour:'numeric',minute:'2-digit'})));
  card.append(heading);
  if (data.late) card.append(element('p', 'card-context', 'Answer to your earlier request'));
  const answer = element('div', 'card-answer'), explicitMedia = safeLink(data.url, conversationId);
  appendRichText(answer, text, conversationId, !explicitMedia); card.append(answer);
  if (explicitMedia) appendMedia(card, {url:explicitMedia,name:typeof data.title === 'string' ? data.title : 'Shared media',kind:data.kind}, conversationId);
  if (data.truncated) addFullResultControl(call,card,answer,data,conversationId);
  for (const item of (Array.isArray(data.attachments) ? data.attachments : []).slice(0,8)) {
    if (typeof item?.id === 'string') appendMedia(card, {...item,url:`/api/v1/conversations/${encodeURIComponent(conversationId)}/attachments/${encodeURIComponent(item.id)}`}, conversationId);
  }
  for (const item of (Array.isArray(data.media) ? data.media : []).slice(0,8)) appendMedia(card, item, conversationId);
  // This named target is always our own work surface. noopener on a named
  // anchor would force a new browsing context on every click.
  if (conversationId) { const link = element('a', 'conversation-link', 'Open conversation ↗'); link.href = conversationLink(conversationId); link.target = 'constellation-work'; card.append(link); }
  appendCard(call, card);
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
      answer.replaceChildren(); appendRichText(answer,result.text,conversationId);
      status.textContent = 'Full saved result shown.'; button.remove();
    } catch (error) { if (current(call) && card.isConnected) { status.textContent = error.message + ' You can open the conversation or try again.'; button.disabled = false; } }
  };
}
function setTab(name) {
  for (const candidate of ['space','transcript']) {
    const selected = name === candidate, button = $(candidate + '-tab');
    button.setAttribute('aria-selected', String(selected)); button.tabIndex = selected ? 0 : -1;
    $(candidate + '-panel').hidden = !selected;
  }
}
for (const name of ['space','transcript']) {
  $(name + '-tab').onclick = () => setTab(name);
  $(name + '-tab').onkeydown = event => { if (['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) { event.preventDefault(); const next = event.key === 'Home' ? 'space' : event.key === 'End' ? 'transcript' : name === 'space' ? 'transcript' : 'space'; setTab(next); $(next + '-tab').focus(); } };
}

function stopCamera(call) {
  call?.cameraStream?.getTracks().forEach(track => track.stop()); if (call) call.cameraStream = null;
  $('camera-preview').srcObject = null; $('camera-toggle').setAttribute('aria-pressed', 'false'); $('camera-label').textContent = 'Camera';
  $('camera-panel').hidden = true;
}
function updateShareCapability(call) {
  if (!current(call)) return;
  $('share-capability').textContent = call.contact.image_input === false
    ? `${call.contact.label} can save images here, but its current connection cannot inspect them. Text is supported; PDF inspection depends on document support.`
    : call.contact.image_input === true
      ? 'Photos can be inspected when you ask. Sharing makes items available; it does not mean they have been read. PDFs depend on document support.'
      : 'Image inspection has not been verified for this connection. You can still save images, PDFs, and text here; the agent will state any reading limits.';
}
async function refreshCapabilities(call) {
  try {
    const result = await api(`/api/v1/companion/agents/${encodeURIComponent(call.contact.id)}/capabilities`);
    if (current(call)) { call.contact.image_input = typeof result.image_input === 'boolean' ? result.image_input : null; updateShareCapability(call); }
  } catch { if (current(call)) { call.contact.image_input = null; updateShareCapability(call); } }
}
async function toggleCamera() {
  const call = state.call; if (!current(call) || !call.id) return;
  if (call.cameraStream) { stopCamera(call); return; }
  $('camera-toggle').disabled = true;
  try {
    const stream = await navigator.mediaDevices.getUserMedia({video:{facingMode:{ideal:'environment'},width:{ideal:1280},height:{ideal:720}},audio:false});
    if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
    call.cameraStream = stream; $('camera-preview').srcObject = stream; $('camera-panel').hidden = false; setTab('space');
    $('camera-toggle').setAttribute('aria-pressed', 'true'); $('camera-label').textContent = 'Camera off';
    await $('camera-preview').play(); $('space-panel').scrollTop = 0;
  } catch (error) { notice(error.name === 'NotAllowedError' ? 'Camera permission was not granted. The voice call is unchanged.' : 'The camera could not be opened. Your voice call can continue.'); stopCamera(call); }
  finally { if (current(call)) $('camera-toggle').disabled = false; }
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
    await shareFile(call, new File([blob], 'companion-photo.jpg', {type:'image/jpeg'}), 'camera');
  } catch (error) { if (current(call)) notice(error.message); }
  finally { call.capturing = false; if (current(call)) $('capture-photo').disabled = false; }
}
function fileMime(file) {
  return ({md:'text/markdown',txt:'text/plain',csv:'text/csv',json:'application/json',pdf:'application/pdf',png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',gif:'image/gif',webp:'image/webp'})[file.name.split('.').at(-1).toLowerCase()] || file.type;
}
function shareStatus(share, status, message) {
  share.card.dataset.state = status; share.status.textContent = message; share.retry.hidden = true;
}
function availableText(call, share) {
  return share.mime?.startsWith('image/') && call.contact.image_input === false
    ? 'Saved and available in this conversation. This connection cannot inspect images.'
    : 'Saved and available in this conversation. Sharing does not mean the agent has read it.';
}
function shareCard(call, id, name) {
  const card = element('article', 'output-card upload-card'); card.dataset.uploadId = id;
  card.append(element('h2', null, name));
  const status = element('p', 'upload-status'), retry = element('button', 'text-button', 'Check voice notification'); retry.type = 'button'; retry.hidden = true;
  card.append(status, retry); appendCard(call, card);
  const share = {id,name,card,status,retry,attachmentIds:[],sending:false}; call.shares.set(id,share); return share;
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
    share.retry.hidden = confirmed;
  } catch (error) {
    if (current(call)) { shareStatus(share, 'available', availableText(call, share) + ' Voice notification is unconfirmed.'); share.retry.hidden = false; }
  }
}
async function shareFile(call, file, source) {
  if (!current(call) || !call.conversationId) return;
  const mime = fileMime(file), allowed = ['image/png','image/jpeg','image/gif','image/webp','application/pdf','text/plain','text/markdown','text/csv','application/json'];
  const name = (file.name || 'shared-file').replace(/[\\/"\u0000-\u001f\u007f]/g,'_').slice(0,150);
  const share = shareCard(call, crypto.randomUUID(), name); Object.assign(share,{source,mime,sending:true,local:true});
  share.retry.onclick = () => announceAvailable(call, share);
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
}
function renderShareEvent(call, data) {
  let share = call.shares.get(data.upload_id);
  if (share?.local) return; // The local upload tracks its richer receipt/uncertainty state.
  share ||= shareCard(call,data.upload_id,typeof data.name === 'string' ? data.name : 'Shared item');
  shareStatus(share,data.state,({captured:'Captured on this device; not yet uploaded.',uploading:'Uploading to this conversation…',available:'Saved and available in this conversation. Sharing does not mean the agent has read it.',failed:'The item could not be made available. Check before sharing it again.'})[data.state] || 'Sharing status unavailable.');
}
async function shareFiles(files, source) {
  const call = state.call;
  if (!current(call) || !call.id || !call.connected) { notice('Connect a call before sharing an item.'); return; }
  setTab('space');
  if (files.length > 4) { notice('Share up to four files at a time.'); return; }
  for (const file of files) { if (!current(call)) return; await shareFile(call,file,source); }
}
$('choose-files').onclick = () => $('share-files').click();
$('share-files').onchange = event => { shareFiles([...event.target.files],'file'); event.target.value = ''; };
$('space-panel').addEventListener('dragover', event => { event.preventDefault(); event.dataTransfer.dropEffect = 'copy'; $('space-panel').classList.add('dragging'); });
$('space-panel').addEventListener('dragleave', event => { if (!$('space-panel').contains(event.relatedTarget)) $('space-panel').classList.remove('dragging'); });
$('space-panel').addEventListener('drop', event => {
  event.preventDefault(); $('space-panel').classList.remove('dragging');
  const files = [...event.dataTransfer.files], text = event.dataTransfer.getData('text/plain');
  if (files.length) shareFiles(files,'drop'); else if (text.trim()) shareFiles([new File([text],'shared-text.txt',{type:'text/plain'})],'drop');
});
$('space-panel').addEventListener('paste', event => {
  if (event.target.closest('input,textarea,[contenteditable="true"]')) return;
  const files = [...(event.clipboardData?.files || [])], text = event.clipboardData?.getData('text/plain') || '';
  if (!files.length && !text.trim()) return;
  event.preventDefault(); shareFiles(files.length ? files : [new File([text],'pasted-text.txt',{type:'text/plain'})],'paste');
});
async function openVoiceSettings(contact) {
  state.voiceContact = contact; const generation = state.authGeneration;
  $('voice-settings-title').textContent = `Voice for ${contact.label || 'agent'}`; $('voice-choice').replaceChildren();
  $('voice-settings-status').textContent = 'Loading available voices…'; $('save-voice').disabled = true; $('voice-choice').disabled = true;
  if (!$('voice-settings').open) $('voice-settings').showModal();
  try {
    const result = await api(`/api/v1/companion/agents/${encodeURIComponent(contact.id)}/voice-settings`);
    if (state.voiceContact !== contact || generation !== state.authGeneration) return;
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
    contact.voice = result.voice; $('voice-settings-status').textContent = `Saved. ${result.voice} will be used for your next call with ${contact.label}.`;
    const card = [...$('contacts').children].find(card => card.dataset.agentId === contact.id); if (card) card.querySelector('.contact-voice').textContent = `Voice · ${result.voice}`;
  } catch (error) { if (state.voiceContact === contact && generation === state.authGeneration) $('voice-settings-status').textContent = error.message; }
  finally { if (state.voiceContact === contact && generation === state.authGeneration) $('save-voice').disabled = false; }
};
$('close-voice-settings').onclick = () => $('voice-settings').close();
$('voice-settings').addEventListener('close', () => { state.voiceContact = null; });
$('call-voice-settings').onclick = () => { if (current(state.call)) openVoiceSettings(state.call.contact); };

function releaseMedia(call) {
  clearTimeout(call.pollTimer); clearInterval(call.heartbeatTimer); clearInterval(call.durationTimer); clearInterval(call.meterTimer); clearInterval(call.controlTimer);
  call.stream?.getTracks().forEach(track => track.stop()); call.remoteStream?.getTracks().forEach(track => track.stop());
  stopCamera(call); for (const share of call.shares.values()) { if (share.previewUrl) URL.revokeObjectURL(share.previewUrl); } $('share-files').value = ''; call.channel?.close(); call.peer?.close();
  audio.pause(); audio.srcObject = null; call.audioSource?.disconnect(); call.audioContext?.close().catch(() => {});
  call.wakeLock?.release().catch(() => {}); call.wakeLock = null; $('presence').classList.remove('speaking');
}
async function confirmClose(id, result) {
  for (let attempt = 0; attempt < 12; attempt++) {
    if (['closed','interrupted'].includes(result.state)) return result;
    await new Promise(resolve => setTimeout(resolve, 500));
    result = await api(`/api/v1/companion/calls/${encodeURIComponent(id)}?after=0`);
  }
  throw new Error('Your devices are off. The service is still closing the voice connection; check the same call again shortly.');
}
async function closeKnownCall(call) {
  if (!call.id || call.authGeneration !== state.authGeneration) return;
  const recovery = state.recovery || {callId:call.id,request:null};
  recovery.callId = call.id; state.recovery = recovery; recovery.checking = true; showRecovery();
  try {
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(call.id)}/close`, 'POST', {}, {keepalive:true});
    if (call.authGeneration !== state.authGeneration) return;
    const terminal = await confirmClose(call.id, result);
    if (call.authGeneration !== state.authGeneration) return;
    if (state.recovery?.callId === call.id || (call.request && state.recovery?.request?.request_id === call.request.request_id)) {
      state.recovery = null; showRecovery();
      $('ended-detail').textContent = terminal.state === 'closed'
        ? 'Your microphone and camera are off. The voice call is closed; accepted work can continue.'
        : 'Your microphone and camera are off. The local connection ended, but provider closure was not confirmed. Accepted work stays in the conversation.';
    }
  }
  catch {
    // No reconnect or replay: retain the same known call for an explicit close
    // retry. The server's heartbeat lease also bounds orphaned voice sessions.
    if (call.authGeneration === state.authGeneration) { state.recovery = {callId:call.id,request:null}; showRecovery(); }
  }
}
function showRecovery() {
  $('recover-call').hidden = !state.recovery;
  $('recover-call').disabled = Boolean(state.recovery?.checking);
  $('recover-call').textContent = state.recovery?.discovered ? 'End earlier call' : 'Resolve unconfirmed call';
  if (state.recovery) $('ended-detail').textContent = state.recovery.checking
    ? 'Your microphone and camera are off. Closing the voice connection…'
    : state.recovery.discovered
      ? 'This page has not connected your microphone or camera. End the earlier voice call before starting another. Work your agent already accepted can continue in its conversation.'
      : 'Your microphone and camera are off. The service has not yet confirmed that this voice call closed. Resolve it before opening another call.';
}
function endCall(message = 'Your microphone and camera are off. Accepted work can continue.', sendClose = true) {
  const call = state.call; if (!call) return;
  call.ended = true; state.call = null; state.generation++; releaseMedia(call); document.body.classList.remove('call-active');
  $('call-screen').hidden = true; $('phonebook').hidden = true; $('call-ended').hidden = false;
  $('ended-title').textContent = call.connected ? 'Until next time.' : 'Call ended.';
  $('ended-detail').textContent = message;
  $('ended-work-link').hidden = !call.conversationId;
  if (call.conversationId) $('ended-work-link').href = conversationLink(call.conversationId);
  if (call.requestPending) state.recovery = {request:call.request,callId:null};
  showRecovery();
  if (sendClose && call.id) closeKnownCall(call);
  $('back-to-contacts').focus();
}
$('recover-call').onclick = async () => {
  const recovery = state.recovery; if (!recovery) return;
  const authGeneration = state.authGeneration;
  recovery.checking = true; showRecovery();
  try {
    let id = recovery.callId;
    if (!id && recovery.request) { const saved = await api('/api/v1/companion/calls','POST',recovery.request); id = saved.id; }
    if (authGeneration !== state.authGeneration) return;
    if (!id) throw new Error('The service could not identify the earlier call.');
    const result = await api(`/api/v1/companion/calls/${encodeURIComponent(id)}/close`,'POST',{});
    const terminal = await confirmClose(id, result);
    if (authGeneration !== state.authGeneration) return;
    if (state.recovery === recovery) state.recovery = null;
    $('ended-detail').textContent = terminal.state === 'closed'
      ? 'The voice call is closed. Your microphone and camera are off.'
      : 'Your devices are off. The local connection ended without confirmed provider closure. Accepted work stays in the conversation.';
    showRecovery();
  } catch (error) { if (authGeneration === state.authGeneration) notice(error.message); }
  finally { if (authGeneration === state.authGeneration) { recovery.checking = false; showRecovery(); } }
};
$('mute').onclick = () => { const call = state.call; if (!current(call) || !call.stream) return; call.muted = !call.muted; call.stream.getAudioTracks().forEach(track => { track.enabled = !call.muted; }); refreshStatus(call); };
$('hangup').onclick = () => endCall();
$('camera-toggle').onclick = toggleCamera;
$('capture-photo').onclick = capturePhoto;
$('back-to-contacts').onclick = async () => {
  if (state.recovery) { showRecovery(); notice('Resolve the previous call before starting another. Your devices are already off.'); return; }
  $('call-ended').hidden = true; $('phonebook').hidden = false;
  try { await openContacts(); } catch (error) { notice(error.message); }
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
  const call = state.call; if (!current(call)) return;
  if (document.hidden) {
    $('foreground-note').textContent = 'This page is out of view. Your browser may pause audio; return here to continue.';
  } else { $('foreground-note').textContent = 'Keep this page in the foreground. A screen lock or app switch may interrupt audio.'; requestWakeLock(call); heartbeat(call); }
});
window.addEventListener('pagehide', () => { if (state.call) endCall(); });
start();
