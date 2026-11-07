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
const state = {session:null, contacts:[], available:false, call:null, generation:0, authGeneration:0, recovery:null, photo:null};
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
    if (contact.voice) card.append(element('span', 'contact-voice', `Voice · ${contact.voice}`));
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
  discardPhoto(); $('cards').replaceChildren(); $('transcript').replaceChildren(); $('contacts').replaceChildren();
  $('caption-speaker').textContent = ''; $('caption-text').textContent = ''; notice('');
  $('photo-question').value = ''; $('ended-detail').textContent = '';
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
    cursor:0, working:new Map(), seen:new Set(), muted:false, startedAt:Date.now(), timers:new Set(), transcriptRow:null};
  state.call = call;
  notice(''); discardPhoto(); $('phonebook').hidden = true; $('call-ended').hidden = true; $('call-screen').hidden = false;
  $('call-avatar').textContent = initials(contact.label); $('call-name').textContent = contact.label || 'Your agent';
  $('call-description').textContent = contact.description || 'The same agent. Another place to talk.';
  $('call-duration').textContent = '00:00'; $('cards').replaceChildren(); $('transcript').replaceChildren();
  $('transcript-empty').hidden = false; $('space-empty').hidden = false; $('work-status').hidden = true;
  $('card-count').hidden = true; $('card-count').textContent = '0'; $('continue-work').hidden = true;
  $('caption-speaker').textContent = ''; $('caption-text').textContent = 'Take your time. There’s room for a pause.';
  $('speaker-state').textContent = 'Speaker waiting'; $('enable-audio').hidden = true;
  $('hangup').disabled = false; setTab('space'); refreshStatus(call);
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
    if (!$('transcript-panel').hidden) $('transcript').scrollTop = $('transcript').scrollHeight;
  } else if (event.kind === 'delegation') {
    const waiting = ['waiting_transcript','dispatched'].includes(data.state);
    if (waiting) call.working.set(data.id, data.state); else call.working.delete(data.id);
    $('work-status').hidden = call.working.size === 0;
    $('work-status').textContent = call.working.size ? `${call.contact.label} is working. You can keep talking.` : '';
    if (['failed','unknown','held'].includes(data.state) && data.message) addCard(call, 'Work update', data.message, call.conversationId);
    refreshStatus(call);
  } else if (event.kind === 'card' && typeof data.text === 'string') {
    addCard(call, `From ${call.contact.label}`, data.text, data.conversation_id || call.conversationId);
  } else if (event.kind === 'error') {
    notice(typeof data.message === 'string' ? data.message : 'The voice service reported an error. Check the conversation before repeating a request.');
  } else if (event.kind === 'closed') {
    call.closeReason = typeof data.reason === 'string' ? data.reason : '';
  }
}
function addCard(call, title, text, conversationId) {
  if (!current(call)) return;
  const card = element('article', 'output-card');
  const heading = element('h2', null, title); heading.append(element('small', null, new Date().toLocaleTimeString([], {hour:'numeric',minute:'2-digit'})));
  card.append(heading, element('p', null, String(text)));
  if (conversationId) { const link = element('a', null, 'Open conversation ↗'); link.href = conversationLink(conversationId); link.target = '_blank'; link.rel = 'noopener'; card.append(link); }
  $('cards').append(card); while ($('cards').children.length > 40) $('cards').firstChild.remove();
  $('space-empty').hidden = true; $('card-count').hidden = false; $('card-count').textContent = String($('cards').children.length);
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
function discardPhoto() {
  if (state.photo?.url) URL.revokeObjectURL(state.photo.url);
  state.photo = null; $('photo-preview').removeAttribute('src'); $('photo-form').hidden = true;
  $('photo-status').textContent = ''; $('send-photo').disabled = false; $('send-photo').textContent = 'Send photo & question';
}
async function toggleCamera() {
  const call = state.call; if (!current(call) || !call.id) return;
  if (call.cameraStream) { stopCamera(call); discardPhoto(); return; }
  $('camera-toggle').disabled = true;
  try {
    const stream = await navigator.mediaDevices.getUserMedia({video:{facingMode:{ideal:'environment'},width:{ideal:1280},height:{ideal:720}},audio:false});
    if (!current(call)) { stream.getTracks().forEach(track => track.stop()); return; }
    call.cameraStream = stream; $('camera-preview').srcObject = stream; $('camera-panel').hidden = false;
    $('camera-toggle').setAttribute('aria-pressed', 'true'); $('camera-label').textContent = 'Camera off';
    await $('camera-preview').play();
  } catch (error) { notice(error.name === 'NotAllowedError' ? 'Camera permission was not granted. The voice call is unchanged.' : 'The camera could not be opened. Your voice call can continue.'); stopCamera(call); }
  finally { if (current(call)) $('camera-toggle').disabled = false; }
}
async function capturePhoto() {
  const call = state.call, video = $('camera-preview');
  if (!current(call) || !call.cameraStream || !video.videoWidth) { notice('Wait for the camera preview before capturing a photo.'); return; }
  const canvas = document.createElement('canvas'), scale = Math.min(1,1280/video.videoWidth,1280/video.videoHeight);
  canvas.width = Math.round(video.videoWidth * scale); canvas.height = Math.round(video.videoHeight * scale);
  canvas.getContext('2d').drawImage(video,0,0,canvas.width,canvas.height);
  const blob = await new Promise(resolve => canvas.toBlob(resolve,'image/jpeg',.85));
  if (!current(call)) return;
  if (!blob || blob.size > 2 * 1024 * 1024) { notice('This photo could not be captured within the attachment limit. Try a smaller image.'); return; }
  discardPhoto();
  state.photo = {blob,url:URL.createObjectURL(blob),requestId:crypto.randomUUID(),attachmentId:null,question:null,sending:false};
  $('photo-preview').src = state.photo.url; $('photo-form').hidden = false; $('photo-question').value = 'What do you notice in this photo?';
  $('send-photo').disabled = call.contact.image_input === false;
  $('photo-question').disabled = call.contact.image_input === false;
  $('photo-capability-note').textContent = call.contact.image_input === false
    ? `${call.contact.label} does not currently accept image questions. This photo stays on your device; local preview is still available.`
    : call.contact.image_input === true
      ? 'Sending shares this photo with the agent’s conversation for its runtime to interpret. GPT-Live does not receive the camera feed.'
      : 'Image support has not been verified for this agent. Its runtime checks support when you send; it may be unable to interpret the photo. GPT-Live does not receive the camera feed.';
  $('photo-status').textContent = 'Captured on this device. Not sent.';
}
async function sendPhoto(event) {
  event.preventDefault(); const call = state.call, photo = state.photo;
  if (!current(call) || !call.conversationId || !photo || photo.sending) return;
  if (call.contact.image_input === false) { notice('This agent does not currently accept image questions. Your photo remains on this device.'); return; }
  const question = $('photo-question').value.trim(); if (!question) { $('photo-question').focus(); return; }
  if (photo.question !== null && photo.question !== question) { notice('The previous send is unconfirmed. Keep the original question when checking it again.'); return; }
  photo.sending = true; $('send-photo').disabled = true; $('photo-status').textContent = 'Sending to the agent’s conversation…';
  try {
    if (!photo.attachmentId) {
      const data = await new Promise((resolve,reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result).split(',')[1]); reader.onerror = () => reject(new Error('The captured photo could not be read.')); reader.readAsDataURL(photo.blob); });
      const uploaded = await api(`/api/v1/conversations/${encodeURIComponent(call.conversationId)}/attachments`, 'POST', {name:'companion-photo.jpg',mime_type:'image/jpeg',data});
      photo.attachmentId = uploaded.id;
      if (!photo.attachmentId) throw new Error('The photo upload did not return a saved attachment.');
    }
    if (!current(call) || state.photo !== photo) return;
    photo.question = question;
    await api(`/api/v1/conversations/${encodeURIComponent(call.conversationId)}/messages`, 'POST', {text:question,request_id:photo.requestId,target_agent_ids:[call.contact.id],attachment_ids:[photo.attachmentId]});
    if (!current(call) || state.photo !== photo) return;
    $('photo-status').textContent = 'Sent. The agent will check whether its runtime can interpret the photo.';
    $('send-photo').textContent = 'Photo sent'; photo.sent = true;
    addCard(call, 'Photo shared', question, call.conversationId);
  } catch (error) {
    if (current(call) && state.photo === photo) {
      $('photo-status').textContent = error.message;
      $('send-photo').textContent = photo.question !== null ? 'Check the same send' : 'Try upload again';
    }
  } finally { photo.sending = false; if (state.photo === photo) $('send-photo').disabled = Boolean(photo.sent); }
}

function releaseMedia(call) {
  clearTimeout(call.pollTimer); clearInterval(call.heartbeatTimer); clearInterval(call.durationTimer); clearInterval(call.meterTimer); clearInterval(call.controlTimer);
  call.stream?.getTracks().forEach(track => track.stop()); call.remoteStream?.getTracks().forEach(track => track.stop());
  stopCamera(call); discardPhoto(); call.channel?.close(); call.peer?.close();
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
  call.ended = true; state.call = null; state.generation++; releaseMedia(call);
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
$('discard-photo').onclick = discardPhoto;
$('photo-form').onsubmit = sendPhoto;
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
