// Voice is an experience adapter: recognized words use the ordinary conversation
// APIs. It never substitutes a second model or changes the connected ACP agent.
export function parseVoice(text) {
  const value = text.trim();
  const command = /^workshop[,:]?\s+(.+?)[.!?]*$/i.exec(value);
  if (!command) return {action: 'dictate', text: value};
  const phrase = command[1].toLowerCase();
  const commands = {'send message':'send', 'clear draft':'clear', 'new conversation':'new',
    'stop agent':'cancel', 'show participants':'participants', 'hide participants':'hide_participants',
    'show activity':'activity', 'show conversation':'conversation', 'show connection':'connection',
    'read last reply':'read', 'stop reading':'quiet', 'stop listening':'off',
    'show action details':'approval_details', 'hide action details':'hide_approval_details',
    'read approval':'read_approval', 'allow once':'approve_once', 'reject once':'reject_once'};
  if (commands[phrase]) return {action:commands[phrase]};
  if (phrase.startsWith('open conversation ')) return {action:'open', text:command[1].slice(18).trim()};
  if (phrase.startsWith('reply to ')) return {action:'target', text:command[1].slice(9).trim()};
  if (phrase.startsWith('name conversation ')) return {action:'rename', text:command[1].slice(18).trim()};
  return {action:'unknown'};
}

export function createVoice({perform, status, onEnabled, onReadAloud}) {
  const Recognition = globalThis.SpeechRecognition || globalThis.webkitSpeechRecognition;
  let recognition, armed=false, enabled=false, running=false, reading=false, readAloud=false, restarting;
  let chain=Promise.resolve();
  let speechGeneration=0;
  const speechQueue=[];
  const seen=new Set();
  function report(text) { status({text, supported:Boolean(Recognition), enabled, running, reading, readAloud, armed}); }
  function stopReading() { speechGeneration++; speechQueue.length=0; globalThis.speechSynthesis?.cancel(); reading=false; }
  function startRecognition() {
    clearTimeout(restarting);
    if (!Recognition || !armed || !enabled || running || reading || document.hidden) return;
    try { recognition.start(); } catch { report('Microphone unavailable. Turn voice off and try again.'); }
  }
  function enable(value, consent=false) {
    if (consent) armed=true;
    enabled=Boolean(value);
    if (!enabled) { clearTimeout(restarting); recognition?.abort(); stopReading(); report('Voice is off.'); }
    else if (!armed) report('Press Start voice to allow this page to listen.');
    else if (!Recognition) report('Voice recognition is unavailable in this browser. You can still type or use your device’s keyboard dictation.');
    else startRecognition();
    onEnabled(enabled);
  }
  function nextSpeech() {
    if (reading || !speechQueue.length) return;
    reading=true; recognition?.abort();
    const generation=speechGeneration;
    const utterance=new SpeechSynthesisUtterance(speechQueue.shift());
    utterance.onend=utterance.onerror=()=>{
      if(generation!==speechGeneration)return;
      reading=false;
      if(speechQueue.length)nextSpeech();
      else { report(enabled?'Listening…':'Voice is off.'); startRecognition(); }
    };
    report('Reading the reply aloud.'); speechSynthesis.speak(utterance);
  }
  function speak(text, enqueue=false) {
    if (!armed || !globalThis.speechSynthesis || !text.trim()) return;
    if(!enqueue)stopReading();
    if(speechQueue.length>=20){report('Several replies arrived. Use Read last reply to catch up.');return;}
    speechQueue.push(text.slice(0,12000));nextSpeech();
  }
  async function accept(text) {
    const command=parseVoice(text);
    if (command.action==='unknown') { report('Command not recognized. Say “Workshop, send message” or speak your draft.'); return; }
    if (command.action==='off') { enable(false); return; }
    if (command.action==='quiet') { stopReading(); startRecognition(); return; }
    const result=await perform(command);
    if (['read','read_approval'].includes(command.action) && result) speak(result);
    if(!reading)report(command.action==='dictate'?'Draft updated. Say “Workshop, send message” when ready.':enabled?'Done. Listening…':'Done. Voice is off.');
  }
  if (Recognition) {
    recognition=new Recognition(); recognition.continuous=true; recognition.interimResults=false;
    recognition.lang=navigator.language || 'en-US';
    recognition.onstart=()=>{running=true;report('Listening. Speak a draft, or say “Workshop” followed by a command.');};
    recognition.onresult=event=>{
      if (!enabled || reading) return;
      for (let i=event.resultIndex;i<event.results.length;i++) if(event.results[i].isFinal) {
        const text=event.results[i][0].transcript;
        chain=chain.then(()=>accept(text)).catch(e=>report(e.message));
      }
    };
    recognition.onerror=event=>{
      if(['not-allowed','service-not-allowed','audio-capture','network'].includes(event.error)) {
        enabled=false; onEnabled(false); report('Voice stopped: '+event.error+'. Check microphone access or your connection.');
      }
    };
    recognition.onend=()=>{running=false; if(enabled && armed && !reading) restarting=setTimeout(startRecognition,350);};
  }
  document.addEventListener('visibilitychange',()=>{if(document.hidden){recognition?.abort();stopReading();}else startRecognition();});
  return {
    enable, speak,
    readAloud(value){readAloud=Boolean(value);onReadAloud(readAloud);if(!readAloud)stopReading();},
    // Baseline existing history before enabling: never read private historical
    // messages merely because someone navigated into a different conversation.
    observe(messages, baseline=false){
      for(const m of messages||[]) { const fresh=!seen.has(m.id);
        if(baseline||m.status==='complete')seen.add(m.id);
        if(fresh&&!baseline&&readAloud&&enabled&&m.role==='assistant'&&m.status==='complete'&&m.text) speak(m.text,true); }
    },
    perform:command=>perform(command),
    transcript:text=>accept(text),
    state:()=>({supported:Boolean(Recognition),enabled,running,reading,readAloud,armed}),
    close(){enable(false);armed=false;seen.clear();},
  };
}
