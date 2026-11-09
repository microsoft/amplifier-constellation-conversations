// Real Chromium AudioContext + MediaStream playback; generated local tones stand
// in for microphone/provider audio. No device capture, model, or paid call.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const source=await readFile(new URL('../ui/companion-floor.mjs',import.meta.url));
const server=createServer((request,response)=>{
 response.writeHead(200,{'Content-Type':request.url==='/companion-floor.mjs'?'text/javascript':'text/html'});
 response.end(request.url==='/companion-floor.mjs'?source:'<!doctype html><audio autoplay></audio>');
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox','--autoplay-policy=no-user-gesture-required']});
try{
 const page=await browser.newPage();await page.goto(`http://127.0.0.1:${server.address().port}`);
 await page.evaluate(async()=>{
  const {ConversationFloor}=await import('/companion-floor.mjs');
  const ctx=new AudioContext();await ctx.resume();const audio=document.querySelector('audio'),destination=ctx.createMediaStreamDestination();
  const signal=(frequency)=>{const oscillator=ctx.createOscillator(),gain=ctx.createGain(),analyser=ctx.createAnalyser();oscillator.frequency.value=frequency;gain.gain.value=0;analyser.fftSize=256;oscillator.connect(gain);gain.connect(analyser);oscillator.start();return{oscillator,gain,analyser};};
  const mic=signal(330),remote=signal(660);remote.gain.connect(destination);audio.srcObject=destination.stream;await audio.play();
  const events=[],floor=new ConversationFloor({changed:value=>{events.push(value);audio.muted=value.playback.state==='suppressed';}});
  const rms=analyser=>{const samples=new Float32Array(256);analyser.getFloatTimeDomainData(samples);return Math.sqrt(samples.reduce((total,value)=>total+value*value,0)/samples.length);};
  const timer=setInterval(()=>floor.sample({mic:rms(mic.analyser)>.02,output:rms(remote.analyser)>.012,available:ctx.state==='running'&&!audio.paused}),25);
  window.fixture={ctx,audio,mic,remote,floor,events,timer};remote.gain.gain.value=.2;
 });
 await page.waitForFunction(()=>fixture.floor.snapshot().output_active===true);
 await page.evaluate(()=>fixture.mic.gain.gain.value=.2);await page.waitForTimeout(200);await page.evaluate(()=>fixture.mic.gain.gain.value=0);
 assert.equal(await page.evaluate(()=>fixture.audio.muted),false,'short backchannel must remain duplex');
 await page.waitForTimeout(400);await page.evaluate(()=>fixture.mic.gain.gain.value=.2);
 await page.waitForFunction(()=>fixture.audio.muted);
 const held=await page.evaluate(()=>({time:fixture.audio.currentTime,id:fixture.floor.interruption.id,paused:fixture.audio.paused}));assert.equal(held.paused,false);
 await page.evaluate(()=>fixture.mic.gain.gain.value=0);await page.waitForTimeout(2000);
 assert.equal(await page.evaluate(()=>fixture.audio.muted),true);assert(await page.evaluate(time=>fixture.audio.currentTime>time,held.time),'muted stream must keep advancing instead of queueing');
 await page.evaluate(()=>fixture.remote.gain.gain.value=0);await page.waitForFunction(()=>!fixture.audio.muted);
 assert.deepEqual(await page.evaluate(()=>({id:fixture.floor.interruption.id,phase:fixture.floor.interruption.phase})),{id:held.id,phase:'released'});
 await page.evaluate(()=>{clearInterval(fixture.timer);fixture.floor.close();fixture.mic.oscillator.stop();fixture.remote.oscillator.stop();fixture.audio.srcObject.getTracks().forEach(track=>track.stop());fixture.audio.pause();fixture.audio.srcObject=null;return fixture.ctx.close();});
 console.log(JSON.stringify({passed:1,scope:'real Chromium AudioContext and continuously consumed MediaStream, local generated tones; no acoustic echo or provider compliance claim'}));
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
