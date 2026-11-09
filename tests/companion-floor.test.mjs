import test from 'node:test';
import assert from 'node:assert/strict';
import {ConversationFloor} from '../ui/companion-floor.mjs';
function fixture(){let clock=0,ids=0;const events=[];const floor=new ConversationFloor({now:()=>clock,id:()=>`interruption-${++ids}`,changed:value=>events.push(value)});return {floor,events,sample:(ms,mic,output,available=true)=>{clock=ms;floor.sample({mic,output,available});}};}
test('short listener backchannels keep full duplex; sustained overlap interrupts once',()=>{
 const {floor,sample,events}=fixture();sample(0,false,true);sample(100,true,true);sample(300,true,true);sample(400,false,true);
 assert.equal(floor.snapshot().state,'ready');assert.equal(floor.snapshot().floor,'open');
 for(let at=1000;at<=1700;at+=100)sample(at,true,true);
 assert.equal(floor.snapshot().state,'suppressed');assert.equal(floor.interruption.reason,'sustained_speech');
 sample(2000,true,true);assert.equal(new Set(events.filter(e=>e.interruption).map(e=>e.interruption.id)).size,1);
});
test('a new output burst while the caller continues explaining is dropped immediately',()=>{
 const {floor,sample}=fixture();sample(0,false,false);sample(100,true,false);sample(300,false,false);sample(700,true,false);sample(800,true,true);
 assert.equal(floor.snapshot().state,'suppressed');assert.equal(floor.interruption.reason,'continued_explanation');
});
test('suppression cannot resume in middle of speech, on mic pause alone, or from unknown sensors',()=>{
 const {floor,sample}=fixture();sample(0,true,false);sample(100,true,true);const id=floor.interruption.id;
 sample(3000,false,true);assert.equal(floor.snapshot().state,'suppressed');
 sample(3100,false,false);sample(3500,false,false);assert.equal(floor.snapshot().state,'suppressed');
 sample(5000,false,false,false);assert.equal(floor.snapshot().floor,'unknown');assert.equal(floor.snapshot().state,'suppressed');
 sample(5100,false,true);sample(5200,false,false);sample(6300,false,false);assert.equal(floor.snapshot().state,'suppressed');sample(7000,false,false);
 assert.deepEqual(floor.interruption,{id,reason:'continued_explanation',phase:'released'});assert.equal(floor.snapshot().state,'ready');
});
test('a pause preserves user floor for context delivery without stopping an appropriate answer',()=>{
 const {floor,sample}=fixture();sample(0,true,false);sample(600,false,false);sample(700,false,true);
 assert.equal(floor.snapshot().floor,'user');assert.equal(floor.snapshot().state,'ready');
 sample(2000,false,true);assert.equal(floor.snapshot().floor,'open');
});
test('new speech after interruption gets a distinct identity; closed call cannot affect next call',()=>{
 const {floor,sample,events}=fixture();sample(0,true,false);sample(100,true,true);sample(200,false,false);sample(2000,false,false);
 const first=floor.interruption.id;sample(2100,true,false);sample(2200,true,true);assert.notEqual(floor.interruption.id,first);
 floor.close();const count=events.length;sample(9000,false,false);assert.equal(events.length,count);
});

test('returning from unknown sensors cannot count unobserved time as quiet playback',()=>{
 const {floor,sample}=fixture();sample(0,true,false);sample(100,true,true);sample(200,false,false,false);
 sample(10000,false,false);assert.equal(floor.snapshot().state,'suppressed');
 sample(11900,false,false);assert.equal(floor.snapshot().state,'ready');
});

test("a brief backchannel in the agent's inter-word audio gap does not seize playback",()=>{
 const {floor,sample}=fixture();sample(0,false,true);sample(100,true,false);sample(200,true,true);sample(300,false,true);
 assert.equal(floor.snapshot().state,'ready');assert.equal(floor.interruption,null);
});

test('unknown playback omits an old release phase while retaining its correlation identity',()=>{
 const {floor,sample,events}=fixture();sample(0,true,false);sample(100,true,true);sample(200,false,false);sample(2000,false,false);
 const id=floor.interruption.id;assert.equal(floor.interruption.phase,'released');sample(2100,false,false,false);
 assert.equal(events.at(-1).interruption,null);assert.equal(events.at(-1).playback.interruption_id,id);
 sample(2200,false,false);assert.equal(events.at(-1).interruption.id,id);assert.equal(events.at(-1).interruption.phase,'released');
});
