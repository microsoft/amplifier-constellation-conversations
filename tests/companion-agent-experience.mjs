// Run through the opt-in Rust browser_agent_working_and_interrupted_experiences test.
// The browser uses the actual HTTP app, a disposable legitimate owner session,
// and fake media. The test voice bridge supplies deterministic transcript events.
import assert from 'node:assert/strict';
import {readFile, rename, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const [origin, cookieName, fixtureHome, scenario] = process.argv.slice(2);
const browser = await chromium.launch({
  headless:true, executablePath:process.env.CHROMIUM_EXECUTABLE || undefined,
  args:['--no-sandbox','--use-fake-device-for-media-stream','--use-fake-ui-for-media-stream'],
});
try {
  const context = await browser.newContext({permissions:['microphone']});
  await context.addCookies([{name:cookieName,value:'fixture-surface-cookie',url:origin}]);
  await context.addInitScript(() => {
    // Deterministic local tracks (silent mic, locally rendered fixture speech).
    // Companion owns the audio element, its meter and floor detection.
    const audio = new AudioContext();
    const mic = audio.createMediaStreamDestination(), remote = audio.createMediaStreamDestination();
    for (const out of [mic,remote]) {
      const silence=audio.createConstantSource(); silence.offset.value=0; silence.connect(out); silence.start();
    }
    navigator.mediaDevices.getUserMedia = async () => { await audio.resume(); return mic.stream; };
    window.playReceivedFixtureSpeech = async bytes => {
      const buffer = await audio.decodeAudioData(Uint8Array.from(atob(bytes), c=>c.charCodeAt(0)).buffer);
      const source=audio.createBufferSource(); source.buffer=buffer; source.connect(remote); source.start();
    };
    window.RTCPeerConnection = class extends EventTarget {
      connectionState='new'; iceGatheringState='complete'; iceConnectionState='new';
      addTrack() {}
      getSenders() { return []; }
      createDataChannel() { return Object.assign(new EventTarget(),{readyState:'open', close() {}}); }
      async createOffer() { return {type:'offer',sdp:'v=0\r\ndisposable-offer'}; }
      async setLocalDescription(description) { this.localDescription=description; }
      async setRemoteDescription() {
        this.connectionState='connected'; this.iceConnectionState='connected';
        this.dispatchEvent(new Event('connectionstatechange'));
        this.dispatchEvent(Object.assign(new Event('track'),{streams:[remote.stream]}));
      }
      close() { this.connectionState='closed'; }
    };
  });
  const page = await context.newPage();
  page.setDefaultTimeout(12000);
  const errors=[];
  page.on('pageerror', error=>errors.push(error.message));
  const transcript = async (n, text) => {
    const at = n * 3000;
    const events=[
      {type:'session.input_transcript.delta',event_id:'speech-'+n,delta:text,start_ms:at,end_ms:at+300},
      {type:'session.delegation.created',event_id:'request-'+n,offset_ms:at+300,delegation:{id:'caller-'+n,target:'client'}},
    ];
    const tmp=join(fixtureHome,'provider-input.tmp');
    await writeFile(tmp,JSON.stringify(events));
    await rename(tmp,join(fixtureHome,'provider-input.json'));
  };
  async function hearCommentary(match) {
    // Read the actual bridge command after Companion's queue, floor and final
    // source checks. Feed those words through local offline TTS into our RTC
    // receive track, then verify real non-silent playback in the caller UI.
    let command;
    const deadline=Date.now()+16000;
    while (!command && Date.now()<deadline) {
      let log='';
      try { log=await readFile(join(fixtureHome,'bridge-commands'),'utf8'); }
      catch (err) { if (err.code!=='ENOENT') throw err; }
      const complete=log.slice(0,log.lastIndexOf('\n')+1).trim();
      if (complete) command=complete.split('\n').map(JSON.parse).find(c=>c.op==='append' && c.channel==='commentary' && match.test(c.content));
      if (!command) await delay(60);
    }
    assert.ok(command, 'Missing eligible commentary: '+match);
    await writeFile(join(fixtureHome,'spoken.txt'),command.content);
    const wav=execFileSync('ffmpeg',['-hide_banner','-loglevel','error','-f','lavfi','-i','flite=textfile=spoken.txt:voice=slt','-f','wav','pipe:1'],{cwd:fixtureHome});
    await page.evaluate(bytes=>window.playReceivedFixtureSpeech(bytes),wav.toString('base64'));
    await page.locator('#presence.speaking').waitFor();
    assert.equal(await page.locator('#remote-audio').evaluate(a=>!a.muted && !a.paused && a.currentTime>0),true);
  }
  try {
    await page.goto(origin+'/companion');
    const presence = page.waitForResponse(res=>res.url().endsWith('/surface/presence') && res.ok());
    await page.getByRole('button',{name:'Call Coach',exact:true}).click();
    await presence;
    await page.getByText('Connected',{exact:true}).waitFor();
    await transcript(1,'Show a private work list with Review agenda.');
    if (scenario==='working') {
      await hearCommentary(/^Your request is saved\. I'm still waiting for the answer\.$/);
      await writeFile(join(fixtureHome,'release-list'),'observed');
      await page.locator('#board-items .output-card').filter({hasText:'Review agenda'}).waitFor();
      await page.getByText('Your first list item is on the board.',{exact:true}).first().waitFor({state:'attached'});
      await transcript(2,'Add Prepare notes to the work list.');
      await page.locator('#board-items .output-card').filter({hasText:'Prepare notes'}).waitFor();
      await page.getByText('The updated list is on your board.',{exact:true}).first().waitFor({state:'attached'});
      await transcript(3,'Save my private product feedback: Keep the call status readable.');
      await page.getByText('Your feedback was saved privately.',{exact:true}).first().waitFor({state:'attached'});
      await transcript(4,'Show a handoff diagram and source documents.');
      const diagram=page.locator('#board-items .output-card').filter({hasText:'Private handoff diagram'}).locator('img.artifact-diagram');
      await diagram.waitFor();
      await page.waitForFunction(()=>document.querySelector('#board-items img.artifact-diagram')?.naturalWidth>0);
      await page.getByText('The diagram and source documents are ready.',{exact:true}).first().waitFor({state:'attached'});
      const documents=page.locator('.output-card').filter({hasText:'The diagram and source documents are ready.'});
      const svgPreview=documents.getByText(/^Preview generated-.*\.svg$/);
      if (!await svgPreview.isVisible()) await page.locator('#board-history').evaluate(el=>el.open=true);
      await svgPreview.click();
      await documents.getByRole('img',{name:'Private rectangle'}).waitFor();
      await documents.getByText('View HTML source',{exact:true}).click();
      await documents.locator('code').filter({hasText:'<h1>Private HTML example</h1>'}).waitFor();
      assert.equal(await page.evaluate(()=>globalThis.artifactRan),undefined);
      const textPage=await context.newPage();
      await textPage.goto(new URL(await page.locator('#continue-work').getAttribute('href'),origin).href);
      const chatSource=textPage.getByText('View HTML source',{exact:true});
      await chatSource.click();
      await textPage.locator('code').filter({hasText:'<h1>Private HTML example</h1>'}).waitFor();
      assert.equal(await textPage.evaluate(()=>globalThis.artifactRan),undefined);
      await textPage.close();
      console.log('working: scoped diagram note, published SVG/HTML resources, private source preview in the call and reopened Conversations');
    } else {
      await page.getByText('Agent needs review',{exact:true}).waitFor();
      const held = page.waitForResponse(async response => {
        if (response.request().method() !== 'GET' || !response.url().includes('?after=')) return false;
        const poll = await response.json();
        return poll.events?.some(event => event.kind === 'progress' && event.data.delegation_id === 'caller-2' && event.data.state === 'blocked');
      });
      await transcript(2,'Please add a note and save my feedback.');
      await held;
      await hearCommentary(/has not reached the agent/);
      await page.getByText('Please add a note and save my feedback.',{exact:false}).last().waitFor({state:'attached'});
      await page.getByText('Open the conversation to review the earlier request before more work can start.',{exact:true}).waitFor();
      assert.equal(await page.locator('#continue-work').isVisible(),true);
      assert.equal(await page.locator('#board-items').getByText('Review agenda').count(),0);
      assert.equal(await page.locator('#call-detail').innerText(),'Voice is connected. New agent work is on hold.');
    }
    await page.locator('#hangup').click();
    await page.locator('#call-ended').waitFor();
    assert.deepEqual(errors,[]);
    console.log(scenario+': call connected, local speech played through the browser, agent outcome observed and browser hang-up confirmed');
  } catch (error) {
    console.error('Browser observations at failure:', await page.locator('#call-status-text, #work-status, #notice-text, #call-ended, #cards, #board-items, #context-items').allInnerTexts(), errors);
    throw error;
  }
} finally {
  await browser.close();
}
